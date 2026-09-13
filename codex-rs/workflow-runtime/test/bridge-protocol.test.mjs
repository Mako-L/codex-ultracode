import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough,Writable} from 'node:stream';
import {createPeer} from '../src/bridge/protocol.mjs';

function pair(optionsA={},optionsB={}) {
  const ab=new PassThrough(),ba=new PassThrough();
  const a=createPeer({input:ba,output:ab,...optionsA});
  const b=createPeer({input:ab,output:ba,...optionsB});
  return {a,b,ab,ba};
}

test('bidirectional requests use disjoint IDs and carry chunked UTF-8 plus events',async()=>{
  const events=[];const seen=[];
  const p=pair({onRequest:async(method,params)=>{seen.push(method);return params;}},{onRequest:async(method,params)=>{seen.push(method);return params;},onEvent:event=>events.push(event)});
  assert.deepEqual(await Promise.all([p.a.request('left',{text:'café'}),p.b.request('right',{text:'雪'})]),[{text:'café'},{text:'雪'}]);
  assert.deepEqual(new Set(seen),new Set(['left','right']));
  await p.a.notify({event:'runChanged',runId:'run-1',revision:2});assert.equal(events[0].runId,'run-1');
  const raw=new PassThrough(),output=new PassThrough();let unicode;
  createPeer({input:raw,output,onRequest:async(_method,params)=>{unicode=params.text;}});
  const bytes=Buffer.from('{"id":"foreign:1","method":"x","params":{"text":"😀"}}\n');raw.write(bytes.subarray(0,bytes.length-3));raw.write(bytes.subarray(bytes.length-3));
  await new Promise(resolve=>setImmediate(resolve));assert.equal(unicode,'😀');await Promise.all([p.a.close(),p.b.close()]);
});

test('native HOST_ERROR preserves its message without disconnecting the bridge',async t=>{
  const input=new PassThrough(),output=new PassThrough();
  const peer=createPeer({input,output});
  t.after(()=>peer.close());
  output.on('data',chunk=>{
    const request=JSON.parse(chunk.toString());
    const response=request.method==='worker.start'
      ?{id:request.id,ok:false,error:{code:'HOST_ERROR',message:'native worker start rejected'}}
      :{id:request.id,ok:true,result:{connected:true}};
    input.write(`${JSON.stringify(response)}\n`);
  });
  await assert.rejects(()=>peer.request('worker.start',{}),error=>
    error.code==='HOST_ERROR'&&error.message==='native worker start rejected'&&!error.outcomeUnresolved);
  assert.deepEqual(await peer.request('inspect',{}),{connected:true});
});

test('remote errors preserve stable code and message',async()=>{
  const p=pair({}, {onRequest:async()=>{const error=new Error('missing');error.code='NOT_FOUND';throw error;}});
  await assert.rejects(()=>p.a.request('inspect',{}),error=>error.code==='NOT_FOUND'&&error.message==='missing');await Promise.all([p.a.close(),p.b.close()]);
});

test('malformed JSON and duplicate active IDs return INVALID_REQUEST and peer recovers',async()=>{
  const input=new PassThrough(),output=new PassThrough();let release;const held=new Promise(resolve=>{release=resolve;});
  const peer=createPeer({input,output,onRequest:async()=>{await held;return 'ok';}});const lines=[];output.on('data',chunk=>lines.push(...chunk.toString().trim().split('\n').filter(Boolean).map(JSON.parse)));
  input.write('{bad}\n');input.write('{"id":"same","method":"hold","params":{}}\n');input.write('{"id":"same","method":"hold","params":{}}\n');
  await new Promise(resolve=>setImmediate(resolve));release();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(lines.filter(line=>line.error?.code==='INVALID_REQUEST').length,2);assert.ok(lines.some(line=>line.id==='same'&&line.ok===true));await peer.close();
});

test('timeout and cancellation identify unresolved remote outcomes',async()=>{
  const p=pair({}, {onRequest:async()=>new Promise(()=>{})});
  await assert.rejects(()=>p.a.request('slow',{}, {timeoutMs:5}),error=>error.outcomeUnresolved===true&&/unresolved/i.test(error.message));
  const controller=new AbortController();const pending=p.a.request('slow',{}, {signal:controller.signal});controller.abort();
  await assert.rejects(()=>pending,error=>error.outcomeUnresolved===true&&/cancelled/i.test(error.message));await Promise.all([p.a.close(),p.b.close()]);
});

test('disconnect rejects outstanding requests and close is idempotent',async()=>{
  const p=pair({}, {onRequest:async()=>new Promise(()=>{})});const pending=p.a.request('slow',{});p.ba.end();
  await assert.rejects(()=>pending,/disconnected.*unresolved/i);await p.a.close();await p.a.close();await p.b.close();
});

test('bounds pending IDs and oversized unterminated lines',async()=>{
  const p=pair({maxPendingRequests:2},{onRequest:async()=>new Promise(()=>{})});
  const one=p.a.request('x',{}),two=p.a.request('x',{});await assert.rejects(()=>p.a.request('x',{}),/pending request limit/i);
  await p.a.close();await assert.rejects(one);await assert.rejects(two);await p.b.close();
  const input=new PassThrough(),output=new PassThrough();const peer=createPeer({input,output,maxLineBytes:8});input.write('123456789');
  await new Promise(resolve=>setImmediate(resolve));await assert.rejects(()=>peer.request('after',{}),/closed|line/i);await peer.close();
});

test('waits for output backpressure',async()=>{
  let writes=0;const output=new Writable({highWaterMark:1,write(_chunk,_encoding,callback){writes++;setTimeout(callback,10);}});const input=new PassThrough();
  const peer=createPeer({input,output});await peer.notify({event:'one'});await peer.notify({event:'two'});assert.equal(writes,2);await peer.close();
});

test('oversized input rejects already-sent requests as unresolved',async()=>{
  const p=pair({maxLineBytes:128},{onRequest:async()=>new Promise(()=>{})});
  const pending=p.a.request('x',{});p.ba.write(Buffer.alloc(129,120));
  await assert.rejects(()=>pending,error=>error.outcomeUnresolved===true&&/line.*unresolved/i.test(error.message));await Promise.all([p.a.close(),p.b.close()]);
});

test('malformed success and ambiguous failure responses reject as unresolved',async()=>{
  for(const malformed of [id=>({id,ok:true}),id=>({id,result:null}),id=>({id,ok:false,result:null,error:{code:'INTERNAL',message:'bad'}})]) {
    const input=new PassThrough(),output=new PassThrough();const peer=createPeer({input,output});
    const id=new Promise(resolve=>output.once('data',chunk=>resolve(JSON.parse(chunk).id)));const pending=peer.request('sent',{});input.write(`${JSON.stringify(malformed(await id))}\n`);
    await assert.rejects(()=>pending,error=>error.outcomeUnresolved===true&&/response.*unresolved/i.test(error.message));await peer.close();
  }
});

test('close resolves when output never drains and rejects pending request as unresolved',async()=>{
  const input=new PassThrough();const output=new Writable({highWaterMark:1,write(){}});const peer=createPeer({input,output});
  const pending=peer.request('sent',{});const rejected=assert.rejects(pending,error=>error.outcomeUnresolved===true);await new Promise(resolve=>setImmediate(resolve));
  await Promise.race([peer.close(),new Promise((_,reject)=>setTimeout(()=>reject(new Error('close hung on output backpressure')),100))]);
  await rejected;await peer.close();
});
test('omitted timeout uses default, null has no deadline, and explicit timeout remains bounded',async()=>{
  const p=pair({requestTimeoutMs:0},{onRequest:async()=>new Promise(()=>{})});
  await assert.rejects(()=>p.a.request('default',{}),/timed out after 0ms/);
  await assert.rejects(()=>p.a.request('explicit',{}, {timeoutMs:0}),/timed out after 0ms/);
  const controller=new AbortController();const pending=p.a.request('unbounded',{}, {timeoutMs:null,signal:controller.signal});
  await new Promise(resolve=>setImmediate(resolve));controller.abort();
  await assert.rejects(()=>pending,/cancelled.*unresolved/i);
  await Promise.all([p.a.close(),p.b.close()]);
});
