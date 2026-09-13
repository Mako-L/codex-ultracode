const unresolved=error=>Object.assign(new Error(`${error?.message??error}; worker start outcome unresolved`),{outcomeUnresolved:true});

export function createNativeAdapter({host}) {
  const active=new Map();
  const key=(runId,workerId)=>`${runId}\0${workerId}`;
  const interrupt=entry=>{
    if(entry.interrupting||!entry.signal?.aborted||!entry.threadId||!entry.turnId)return;
    entry.interrupting=true;
    entry.interruptPromise=host.request('worker.interrupt',{runId:entry.runId,workerId:entry.workerId,authorityRef:entry.authorityRef,authorityGeneration:entry.authorityGeneration,threadId:entry.threadId,turnId:entry.turnId},entry.timeoutMs===undefined?{}:{timeoutMs:entry.timeoutMs});
    entry.interruptPromise.catch(()=>{});
  };
  return {
    prepare:request=>host.request('workspace.prepare',request),
    release:request=>host.request('workspace.release',request),
    handleEvent(event) {
      if(event?.event!=='worker.updated')return;
      const entry=active.get(key(event.runId,event.workerId));
      if(!entry||!Number.isInteger(event.revision)||event.revision<=entry.revision)return;
      if(entry.threadId&&event.threadId&&entry.threadId!==event.threadId)return;
      if(entry.turnId&&event.turnId&&entry.turnId!==event.turnId)return;
      entry.revision=event.revision;entry.threadId=event.threadId??entry.threadId;entry.turnId=event.turnId??entry.turnId;
      entry.onUpdate?.({threadId:event.threadId,sessionId:event.sessionId,turnId:event.turnId,status:event.status,text:event.text,usage:event.usage,activity:event.activity,firstResponseStarted:event.firstResponseStarted===true});
      interrupt(entry);
    },
    async run(options) {
      const entry={...options,revision:-1,threadId:options.threadId,turnId:null,interrupting:false};
      const identity=key(options.runId,options.workerId);active.set(identity,entry);
      const abort=()=>interrupt(entry);options.signal?.addEventListener('abort',abort,{once:true});
      try {
        let result;
        try {result=await host.request('worker.start',{runId:options.runId,workerId:options.workerId,authorityRef:options.authorityRef,authorityDigest:options.authorityDigest,authorityGeneration:options.authorityGeneration,prompt:options.prompt,model:options.model??null,effort:options.effort??null,modelExplicit:options.modelExplicit===true,effortExplicit:options.effortExplicit===true,agentType:options.agentType,readOnly:options.readOnly,schema:options.schema??null,workspace:options.workspace,resumeThreadId:options.threadId??null},{timeoutMs:options.timeoutMs??null});}
        catch(error){throw error?.outcomeUnresolved?unresolved(error):error;}
        // A short turn can finish before the first progress event supplies its
        // identity. Cancellation must still cross the exact-turn close barrier.
        entry.threadId=result?.threadId??entry.threadId;entry.turnId=result?.turnId??entry.turnId;
        // Final snapshots can contain accounting absent from progress events, even
        // when interruption fails. Preserve it before enforcing the close barrier.
        if(result&&(result.status!=='completed'||entry.signal?.aborted))entry.onUpdate?.(result);
        interrupt(entry);
        if(entry.signal?.aborted&&!entry.interruptPromise)throw unresolved(new Error('Native worker interruption identity unavailable'));
        // Hosts reject interruption after completion. Keep that outcome unresolved
        // with the terminal identity above so explicit resume can recover it.
        if(entry.interruptPromise)try{await entry.interruptPromise;}catch(error){throw unresolved(error);}
        if(entry.signal?.aborted||result?.status==='interrupted')throw new Error(result?.error??'Native worker interrupted');
        if(result?.status!=='completed')throw new Error(result?.error??`Native worker ${result?.status??'failed'}`);
        return result;
      } finally {options.signal?.removeEventListener('abort',abort);active.delete(identity);}
    },
  };
}
