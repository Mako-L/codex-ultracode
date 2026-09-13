import {randomUUID} from 'node:crypto';

const CODES=new Set(['INVALID_REQUEST','VERSION_MISMATCH','NOT_FOUND','PERMISSION_DENIED','CONFLICT','STALE_AUTHORITY','UNAVAILABLE_MODEL','INVALID_OUTPUT','HOST_ERROR','INTERNAL']);
const makeError=(message,code='INTERNAL',unresolved=false)=>Object.assign(new Error(message),{code,outcomeUnresolved:unresolved});

export function createPeer({input,output,onRequest=async()=>{throw makeError('Method not implemented','NOT_FOUND');},onEvent=()=>{},maxLineBytes=8*1024*1024,requestTimeoutMs=30000,maxPendingRequests=1024}) {
  if(!input?.on||!output?.write)throw new TypeError('input and output streams are required');
  if(!Number.isSafeInteger(maxLineBytes)||maxLineBytes<1)throw new TypeError('maxLineBytes must be positive');
  const namespace=randomUUID(),pending=new Map(),activeInbound=new Set(),writeWaiters=new Set();let sequence=0,buffer=Buffer.alloc(0),closed=false,closePromise,fatalReason;
  let writes=Promise.resolve();
  const enqueue=value=>{
    if(closed)return Promise.reject(fatalReason??makeError('Bridge is closed'));
    const encoded=JSON.stringify(value),line=`${encoded}\n`;
    if(Buffer.byteLength(encoded)>maxLineBytes){const error=makeError('Outgoing bridge line exceeds limit; remote outcome unresolved','INVALID_REQUEST',true);disconnect(error);return Promise.reject(error);}
    const operation=writes.then(()=>new Promise((resolve,reject)=>{
      let settled=false;
      const finish=error=>{if(settled)return;settled=true;writeWaiters.delete(cancel);error?reject(error):resolve();};
      const cancel=()=>finish();writeWaiters.add(cancel);
      try{output.write(line,finish);}catch(error){finish(error);}
    }));
    writes=operation.catch(error=>{disconnect(makeError(`Bridge output disconnected: ${error.message}; remote outcome unresolved`,'INTERNAL',true));});
    return operation;
  };
  const rejectPending=error=>{for(const item of pending.values()){clearTimeout(item.timer);item.signal?.removeEventListener('abort',item.abort);item.reject(error);}pending.clear();};
  function disconnect(error=makeError('Bridge disconnected; remote outcome unresolved','INTERNAL',true)) {
    if(closed)return;closed=true;fatalReason=error;input.off('data',data);input.off('end',ended);input.off('close',ended);input.off('error',failed);output.off('error',failed);for(const unblock of writeWaiters)unblock();writeWaiters.clear();rejectPending(error);
  }
  const invalid=id=>enqueue({id:typeof id==='string'?id:null,ok:false,error:{code:'INVALID_REQUEST',message:'Invalid bridge request'}}).catch(()=>{});
  const handleRequest=message=>{
    if(typeof message.id!=='string'||!message.id||typeof message.method!=='string'||!message.method||!Object.hasOwn(message,'params')||activeInbound.has(message.id)||activeInbound.size>=maxPendingRequests){invalid(message.id);return;}
    activeInbound.add(message.id);
    Promise.resolve().then(()=>onRequest(message.method,message.params)).then(
      result=>enqueue({id:message.id,ok:true,result:result??null}),
      error=>enqueue({id:message.id,ok:false,error:{code:CODES.has(error?.code)?error.code:'INTERNAL',message:String(error?.message??error)}}),
    ).catch(()=>{}).finally(()=>activeInbound.delete(message.id));
  };
  const handle=response=>{
    if(!response||typeof response!=='object'||Array.isArray(response)){invalid(null);return;}
    if(typeof response.event==='string'&&!Object.hasOwn(response,'id')&&!Object.hasOwn(response,'method')&&!Object.hasOwn(response,'ok')){Promise.resolve().then(()=>onEvent(response)).catch(()=>{});return;}
    if(typeof response.id==='string'&&!Object.hasOwn(response,'method')&&(Object.hasOwn(response,'ok')||pending.has(response.id))){
      const item=pending.get(response.id);if(!item)return;
      const validSuccess=response.ok===true&&Object.hasOwn(response,'result')&&!Object.hasOwn(response,'error');
      const validFailure=response.ok===false&&!Object.hasOwn(response,'result')&&response.error&&CODES.has(response.error.code)&&typeof response.error.message==='string';
      if(!validSuccess&&!validFailure){pending.delete(response.id);clearTimeout(item.timer);item.signal?.removeEventListener('abort',item.abort);item.reject(makeError('Invalid bridge response; remote outcome unresolved','INVALID_REQUEST',true));return;}
      pending.delete(response.id);clearTimeout(item.timer);item.signal?.removeEventListener('abort',item.abort);
      response.ok?item.resolve(response.result):item.reject(makeError(response.error.message,response.error.code));return;
    }
    if(Object.hasOwn(response,'method'))handleRequest(response);
  };
  const line=bytes=>{if(bytes.length>maxLineBytes){disconnect(makeError('Incoming bridge line exceeds limit; remote outcomes unresolved','INVALID_REQUEST',true));return;}let message;try{message=JSON.parse(bytes.toString('utf8'));}catch{invalid(null);return;}handle(message);};
  function data(chunk) {
    chunk=Buffer.isBuffer(chunk)?chunk:Buffer.from(chunk);
    for(let offset=0;offset<chunk.length&&!closed;){const newline=chunk.indexOf(10,offset);if(newline===-1){const tail=chunk.subarray(offset);if(buffer.length+tail.length>maxLineBytes){disconnect(makeError('Incoming bridge line exceeds limit; remote outcomes unresolved','INVALID_REQUEST',true));return;}buffer=buffer.length?Buffer.concat([buffer,tail]):Buffer.from(tail);return;}
      const segment=chunk.subarray(offset,newline);if(buffer.length+segment.length>maxLineBytes){disconnect(makeError('Incoming bridge line exceeds limit; remote outcomes unresolved','INVALID_REQUEST',true));return;}
      const complete=buffer.length?Buffer.concat([buffer,segment]):segment;buffer=Buffer.alloc(0);line(complete);offset=newline+1;}
  }
  const ended=()=>disconnect();const failed=error=>disconnect(makeError(`Bridge disconnected: ${error.message}; remote outcome unresolved`,'INTERNAL',true));
  input.on('data',data);input.once('end',ended);input.once('close',ended);input.once('error',failed);output.once('error',failed);
  return {
    request(method,params,options={}) {
      if(closed)return Promise.reject(fatalReason??makeError('Bridge is closed'));
      if(typeof method!=='string'||!method) return Promise.reject(makeError('Bridge method is required','INVALID_REQUEST'));
      const timeout=Object.hasOwn(options,'timeoutMs')?options.timeoutMs:requestTimeoutMs;if(timeout!==null&&(!Number.isFinite(timeout)||timeout<0))return Promise.reject(makeError('Bridge request timeout is invalid','INVALID_REQUEST'));
      if(pending.size>=maxPendingRequests)return Promise.reject(makeError('Bridge pending request limit exceeded','CONFLICT'));
      if(options.signal?.aborted)return Promise.reject(options.signal.reason??makeError('Bridge request cancelled before send','INTERNAL'));
      const id=`${namespace}:${++sequence}`;
      return new Promise((resolve,reject)=>{
        const abort=()=>{if(!pending.delete(id))return;clearTimeout(timer);reject(makeError('Local request cancelled; remote outcome unresolved','INTERNAL',true));};
        const timer=timeout===null?undefined:setTimeout(()=>{if(!pending.delete(id))return;options.signal?.removeEventListener('abort',abort);reject(makeError(`Bridge request timed out after ${timeout}ms; remote outcome unresolved`,'INTERNAL',true));},timeout);
        pending.set(id,{resolve,reject,timer,signal:options.signal,abort});options.signal?.addEventListener('abort',abort,{once:true});
        enqueue({id,method,params:params??null}).catch(error=>{if(!pending.delete(id))return;clearTimeout(timer);options.signal?.removeEventListener('abort',abort);reject(error);});
      });
    },
    notify(event) {
      if(!event||typeof event!=='object'||Array.isArray(event)||typeof event.event!=='string'||!event.event||Object.hasOwn(event,'id')||Object.hasOwn(event,'method')||Object.hasOwn(event,'ok'))return Promise.reject(makeError('Bridge event is required','INVALID_REQUEST'));
      return enqueue(event);
    },
    close() {
      return closePromise??=(async()=>{disconnect(makeError('Bridge closed; remote outcome unresolved','INTERNAL',true));await writes.catch(()=>{});if(!output.destroyed&&!output.writableEnded)output.end();})();
    },
  };
}
