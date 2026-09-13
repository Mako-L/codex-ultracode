import {sandboxWithinCeiling,supportedApprovalPolicy} from '../codex/permissions.mjs';
import path from 'node:path';
import {appendFile} from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { availableParallelism } from 'node:os';
import { execFileSync } from 'node:child_process';
import Ajv from 'ajv';
import { executeScript, parseScript } from './script.mjs';
import { acquireRun, consumeControl, digest, readRun, runDirectory, writeRun, writeRunScript } from './store.mjs';
import {createPrefixStagger,prefixStaggerDelay} from './prefix-stagger.mjs';
import {requestApproval} from './approvals.mjs';

const ajv=new Ajv({strict:false,allErrors:true});
const now=()=>new Date().toISOString();
const allowedOptions=new Set(['label','phase','model','effort','schema','write','isolation','agentType']);
const builtInAgentTypes=new Set(['general-purpose','Explore','Plan']);
const normalizeUsage=usage=>usage?.total?{...usage.total,...(usage.modelContextWindow==null?{}:{modelContextWindow:usage.modelContextWindow})}:usage;
const normalizeWorkerStatus=status=>['starting','inProgress'].includes(status)?'running':status;
function permissionEnvelope(requested={sandbox:'read-only',approvalPolicy:'never'}, previous) {
  if(!sandboxWithinCeiling(requested.sandbox,'danger-full-access')||!supportedApprovalPolicy(requested.approvalPolicy))throw new Error('Unsupported permission envelope');
  if(previous&&!sandboxWithinCeiling(requested.sandbox,previous.sandbox))throw new Error('Cannot widen persisted permission ceiling on resume');
  if(previous&&previous.approvalPolicy!==requested.approvalPolicy)throw new Error('Cannot change persisted approval ceiling on resume');
  return {sandbox:requested.sandbox,approvalPolicy:requested.approvalPolicy};
}
function impossibleSchema(schema) {
  if(schema===false)return true;
  if(!schema||typeof schema!=='object'||Array.isArray(schema))return false;
  if(Array.isArray(schema.allOf)&&schema.allOf.some(impossibleSchema))return true;
  if(Array.isArray(schema.anyOf)&&schema.anyOf.every(impossibleSchema))return true;
  if(Array.isArray(schema.oneOf)&&schema.oneOf.every(impossibleSchema))return true;
  if(schema.type==='object') {
    const properties=schema.properties??{};
    for(const name of schema.required??[]) {
      if(Object.hasOwn(properties,name)) {
        if(impossibleSchema(properties[name]))return true;
      } else if(schema.additionalProperties===false&&!Object.keys(schema.patternProperties??{}).some(pattern=>{
        try{return new RegExp(pattern,'u').test(name);}catch{return false;}
      })) return true;
    }
  }
  return false;
}
function structuredOutputAttempts() {
  const value=Number(process.env.MAX_STRUCTURED_OUTPUT_RETRIES??5);
  return Number.isInteger(value)&&value>=1&&value<=20?value:5;
}

export async function runWorkflow(options) {
  const cwd=path.resolve(options.cwd??process.cwd());
  const stateDir=path.resolve(options.stateDir??path.join(cwd,'.ultracode'));
  const id=options.runId??randomUUID();
  const release=acquireRun(stateDir,id);
  let ownsRun=true;
  let client=options.client;
  let state;
  let recoveryBaseline=[];
  const pendingRecovery=new Map();
  let timer;
  let interrupted;
  const controller=new AbortController();
  const active=new Map();
  const preparationStops=new Map();
  const pending=new Set();
  let running=0;
  const queue=[];
  const prefixStagger=createPrefixStagger({delayMs:options.prefixStaggerMs??prefixStaggerDelay()});
  const save=()=>{
    if(pendingRecovery.size){
      const records=new Map(recoveryBaseline.map(worker=>[worker.id,worker]));
      for(const worker of state.workers)if(worker.signature)records.set(worker.id,worker);
      for(const [workerId,worker] of pendingRecovery)records.set(workerId,worker);
      state.recoveryWorkers=[...records.values()];
    }else delete state.recoveryWorkers;
    state.updatedAt=now();writeRun(stateDir,state);options.onUpdate?.(structuredClone(state));
  };
  try {
    const old=options.resume||options.restart?readRun(stateDir,id):null;
    const concurrency=options.concurrency??old?.concurrency??Math.min(16,availableParallelism());
    if(!Number.isInteger(concurrency)||concurrency<1||concurrency>16)throw new Error('Concurrency must be 1–16');
    const source=options.source??old?.source;
    const {meta}=parseScript(source);
    if(options.nativeWorkspace&&(!options.authorityRef||!options.authorityDigest))throw new Error('Native workflow requires authorityRef and authorityDigest');
    const permission=permissionEnvelope(options.permission??old?.permission,old?.permission);
    const prior=(old?.uncertainWorkers?old.recoveryWorkers??old.workers:old?.workers)??[];
    recoveryBaseline=prior;
    if(old?.uncertainWorkers)for(const worker of prior)if(worker.status==='interrupted'||worker.launchIntent)pendingRecovery.set(worker.id,worker);
    if(old?.uncertainWorkers&&!pendingRecovery.size)throw new Error('Recovery blocked: unresolved worker termination has no recoverable identity');
    if(old?.workers?.some(worker=>worker.launchIntent&&!worker.turnId))throw new Error('Recovery blocked: unresolved worker launch intent');
    if(options.restart&&old?.uncertainWorkers)throw new Error('Restart blocked: unresolved worker termination');
    const authorityMatches=!options.nativeWorkspace||!old||old.authorityDigest===options.authorityDigest;
    if(pendingRecovery.size&&!authorityMatches)throw new Error('Recovery blocked: unresolved worker termination requires the original authority');
    let replay=!options.restart&&authorityMatches;
    let replayDecision=Promise.resolve();
    state={version:1,id,name:meta.name,description:meta.description,source,sourceDigest:digest(source),args:Object.hasOwn(options,'args')?options.args:old?.args,cwd,permission,...(options.authorityDigest?{authorityDigest:options.authorityDigest}:{}),model:options.model??old?.model,effort:options.effort??old?.effort??(options.nativeWorkspace?null:'medium'),concurrency,status:'running',createdAt:old?.createdAt??now(),startedAt:now(),updatedAt:now(),phases:(meta.phases??[]).map(name=>({name})),workers:[],logs:[],launchCount:old?.launchCount??prior.filter(worker=>worker.startedAt&&!worker.cached).length,attempt:(old?.attempt??0)+1,history:old?[...(old.history??[]),{attempt:old.attempt,status:old.status,workers:old.workers,result:old.result,scriptPath:old.scriptPath}]:[]};
    if(!state.model&&!options.nativeWorkspace)throw new Error('An explicit Codex catalog model is required');
    if(pendingRecovery.size)state.uncertainWorkers=true;
    state.scriptPath=writeRunScript(stateDir,id,source);
    save();
    const stop=(type)=>{
      interrupted=type;
      controller.abort(new Error(`Workflow ${type}`));
      for(const item of active.values())item.abort();
      while(queue.length)queue.shift()();
    };
    const poll=()=>{
      const command=consumeControl(stateDir,id);
      if(!command)return;
      if(command.workerId) {
        const worker=state.workers.find(w=>w.id===command.workerId);
        if(worker) {
          if(command.type==='restart')worker.restart=true;
          else worker.stopRequested=true;
          if(active.has(worker.id))active.get(worker.id).abort();
          if(command.type!=='restart')preparationStops.get(worker.id)?.abort(new Error('Worker stopped'));
          save();
        }
      } else stop(command.type==='pause'?'paused':command.type==='restart'?'restart':'stopped');
    };
    timer=setInterval(()=>{try{poll();}catch(error){state.error=error.message;stop('interrupted');}},20);
    const abort=()=>stop('stopped');
    options.signal?.addEventListener('abort',abort,{once:true});
    if(options.signal?.aborted)abort();
    let phase='Workflow';
    const getClient=async()=>{
      if(client)return client;
      const {CodexClient}=await import('../codex/client.mjs');
      if(state.serverPid) {state.serverPid=null;state.serverExitedAt=now();save();}
      state.launchIntent={at:now(),ownerPid:process.pid};save();
      client=new CodexClient({cwd,onSpawn:pid=>{state.serverPid=pid;state.serverPgid=process.platform==='win32'?null:pid;save();}});
      await client.start();
      state.serverPid=client.pid??state.serverPid;
      state.launchIntent=null;save();
      return client;
    };
    // Serialize client creation; agents can otherwise race the initial connection.
    let connection;
    const connected=()=>connection??=getClient();
    const resolveRoles=!options.nativeWorkspace&&(!options.client||typeof options.client.resolveRole==='function');
    const asynchronousPreparation=Boolean(options.nativeWorkspace||resolveRoles);
    const call=async(type,payload)=>{
      if(controller.signal.aborted)throw controller.signal.reason;
      if(type==='phase') {
        if(typeof payload.name!=='string'||!payload.name.trim()||payload.name.length>200)throw new Error('Invalid phase name');
        phase=payload.name;
        if(!state.phases.some(p=>p.name===phase))state.phases.push({name:phase});
        save();return null;
      }
      if(type==='log'){if(state.logs.length<1000)state.logs.push({at:now(),value:payload.value});save();return null;}
      if(type!=='agent')throw new Error('Unknown orchestration capability');
      const {prompt,options:workerOptions={}}=payload;
      if(typeof prompt!=='string'||!prompt.trim()||prompt.length>200_000)throw new Error('Invalid agent prompt');
      if(!workerOptions||typeof workerOptions!=='object'||Array.isArray(workerOptions))throw new Error('Invalid agent options');
      for(const key of Object.keys(workerOptions))if(!allowedOptions.has(key))throw new Error(`Unsupported worker option: ${key}`);
      if(workerOptions.isolation!==undefined&&workerOptions.isolation!=='worktree')throw new Error(`Unsupported worker isolation: ${workerOptions.isolation}`);
      if(workerOptions.agentType!==undefined&&(typeof workerOptions.agentType!=='string'||!workerOptions.agentType.trim()||workerOptions.agentType.length>200))throw new Error(`Invalid agent type: ${workerOptions.agentType}`);
      const agentType=workerOptions.agentType?.trim()??'general-purpose';
      if(!options.nativeWorkspace&&!resolveRoles&&!builtInAgentTypes.has(agentType))throw new Error(`Unsupported agent type: ${agentType}`);
      if(workerOptions.write!==undefined&&typeof workerOptions.write!=='boolean')throw new Error('Worker write must be boolean');
      const workerPhase=workerOptions.phase??phase;
      if(typeof workerPhase!=='string'||!workerPhase.trim()||workerPhase.length>200)throw new Error('Invalid phase name');
      if(workerOptions.write&&!options.nativeWorkspace&&permission.sandbox==='read-only')throw new Error('Worker write exceeds parent permission ceiling');
      let validate=null;
      if(workerOptions.schema!==undefined) {
        if(impossibleSchema(workerOptions.schema))throw new Error('Output schema is contradictory');
        validate=ajv.compile(workerOptions.schema);
      }
      if(state.workers.length>=1000)throw new Error('Total worker limit is 1000');
      const modelExplicit=Object.hasOwn(workerOptions,'model');
      const effortExplicit=Object.hasOwn(workerOptions,'effort');
      let model=workerOptions.model??state.model;
      let effort=workerOptions.effort??state.effort;
      const readOnly=workerOptions.write===false||agentType==='Explore'||agentType==='Plan';
      const sandbox=readOnly?'read-only':permission.sandbox;
      // Deprecated compatibility alias: write:true retains isolated-edit behavior.
      const isolated=workerOptions.isolation==='worktree'||workerOptions.write===true;
      const hasExtendedContract=['phase','agentType','isolation'].some(key=>Object.hasOwn(workerOptions,key));
      const index=state.workers.length;
      const workerId=`worker-${index+1}`;
      const previousDecision=replayDecision;let finishDecision;
      if(asynchronousPreparation)replayDecision=new Promise(resolve=>{finishDecision=resolve;});
      const preparationStop=new AbortController();
      preparationStops.set(workerId,preparationStop);
      let worker=asynchronousPreparation?{id:workerId,label:workerOptions.label??`Agent ${index+1}`,phase:workerPhase,agentType,readOnly,isolation:isolated?'worktree':null,prompt,model,effort,status:'preparing',options:workerOptions,activity:[]}:null;
      if(worker){state.workers.push(worker);if(!state.phases.some(p=>p.name===workerPhase))state.phases.push({name:workerPhase});save();}
      const releaseLateWorkspace=workspace=>{
        if(!options.nativeWorkspace||typeof workspace?.workspaceId!=='string')return;
        Promise.resolve().then(()=>options.nativeWorkspace.release({runId:id,workerId,workspaceId:workspace.workspaceId})).catch(error=>{if(!ownsRun){void appendFile(path.join(runDirectory(stateDir,id),`workspace-release-${state.attempt}.log`),`${JSON.stringify({workerId,workspaceId:workspace.workspaceId,error:error.message})}\n`,{encoding:'utf8',mode:0o600}).catch(()=>{});return;}if(worker){try{if(readRun(stateDir,id).attempt!==state.attempt)return;}catch{return;}worker.workspaceReleaseError=error.message;try{save();}catch{}}});
      };
      const awaitPreparation=async pendingPreparation=>{
        let rejectCancellation;
        const cancellation=new Promise((_,reject)=>{rejectCancellation=reject;});
        const onWorkflowAbort=()=>rejectCancellation(controller.signal.reason??new Error('Workflow interrupted'));
        const onWorkerAbort=()=>rejectCancellation(preparationStop.signal.reason??new Error('Worker stopped'));
        if(controller.signal.aborted)onWorkflowAbort();
        else controller.signal.addEventListener('abort',onWorkflowAbort,{once:true});
        if(preparationStop.signal.aborted)onWorkerAbort();
        else preparationStop.signal.addEventListener('abort',onWorkerAbort,{once:true});
        try{return await Promise.race([Promise.resolve(pendingPreparation),cancellation]);}
        catch(error){
          if(controller.signal.aborted||preparationStop.signal.aborted)Promise.resolve(pendingPreparation).then(releaseLateWorkspace,()=>{});
          throw error;
        } finally {
          controller.signal.removeEventListener('abort',onWorkflowAbort);
          preparationStop.signal.removeEventListener('abort',onWorkerAbort);
        }
      };
      const releaseWorkspace=async workspace=>{
        if(!options.nativeWorkspace||typeof workspace?.workspaceId!=='string')return;
        try{await options.nativeWorkspace.release({runId:id,workerId,workspaceId:workspace.workspaceId});}
        catch(error){if(worker){worker.workspaceReleaseError=error.message;try{save();}catch{}}}
      };
      let workspace=null,resolvedRole=null;
      try{
        workspace=options.nativeWorkspace?await awaitPreparation(options.nativeWorkspace.prepare({runId:id,workerId,authorityRef:options.authorityRef,authorityDigest:options.authorityDigest,isolation:isolated?'worktree':null,requestedCwd:cwd,previous:prior[index]?.workspace??null,agentType})):null;
        if(resolveRoles){
          resolvedRole=await awaitPreparation(connected().then(client=>client.resolveRole(agentType,cwd)));
          model=resolvedRole?.model??model;effort=resolvedRole?.effort??effort;
          Object.assign(worker,{model,effort,...(resolvedRole?{roleDigest:resolvedRole.digest}:{})});
        }
      }
      catch(error){const stopped=worker?.stopRequested&&!controller.signal.aborted;Promise.resolve(previousDecision).then(()=>{replay=false;finishDecision?.();},()=>{replay=false;finishDecision?.();});if(worker){worker.status=controller.signal.aborted?'stopped':'failed';worker.error=stopped?'Worker stopped':error.message;worker.output=null;save();}if(stopped){preparationStops.delete(workerId);return null;}preparationStops.delete(workerId);throw error;}
      if(workspace&&((workspace.workspaceId!==null&&typeof workspace.workspaceId!=='string')||(workspace.isolated&&typeof workspace.workspaceId!=='string')||typeof workspace.isolated!=='boolean'||typeof workspace.cwd!=='string'||!path.isAbsolute(workspace.cwd)||!Number.isInteger(workspace.authorityGeneration)||typeof workspace.roleDigest!=='string'||!workspace.roleDigest)){
        const validationError=new Error('Invalid native workspace response');
        await releaseWorkspace(workspace);
        preparationStops.delete(workerId);
        if(worker){worker.status='failed';worker.error=validationError.message;worker.output=null;save();}
        Promise.resolve(previousDecision).then(()=>{replay=false;finishDecision?.();},()=>{replay=false;finishDecision?.();});
        throw validationError;
      }
      const signature=options.nativeWorkspace
        ?digest({prompt,options:workerOptions,model,effort,phase:workerPhase,agentType,roleDigest:workspace.roleDigest,readOnly,isolation:isolated?'worktree':null,authorityDigest:options.authorityDigest,workspace:{workspaceId:workspace.workspaceId,cwd:workspace.cwd,isolated:workspace.isolated,baseCommit:workspace.baseCommit??null}})
        :digest(hasExtendedContract||resolvedRole?{prompt,options:workerOptions,model,effort,permission,phase:workerPhase,agentType,sandbox,isolated,...(resolvedRole?{roleDigest:resolvedRole.digest}:{})}:{prompt,options:workerOptions,model,effort,permission});
      const cached=prior[index];
      if(asynchronousPreparation)try{await awaitPreparation(previousDecision);}catch(error){await releaseWorkspace(workspace);const stopped=worker?.stopRequested&&!controller.signal.aborted;Promise.resolve(previousDecision).then(()=>{replay=false;finishDecision?.();},()=>{replay=false;finishDecision?.();});preparationStops.delete(workerId);if(worker){worker.status=controller.signal.aborted?'stopped':'failed';worker.error=stopped?'Worker stopped':error.message;worker.output=null;save();}if(stopped)return null;throw error;}
      if(replay&&!controller.signal.aborted&&!worker?.stopRequested&&!worker?.restart&&cached?.signature===signature&&cached.status==='completed') {
        const replayed={...cached,phase:workerPhase,cached:true};if(worker)state.workers[index]=replayed;else state.workers.push(replayed);save();
        preparationStops.delete(workerId);
        finishDecision?.();
        if(workspace?.workspaceId!=null)try{await options.nativeWorkspace.release({runId:id,workerId,workspaceId:workspace.workspaceId});}catch(error){replayed.workspaceReleaseError=error.message;save();}
        return replayed.output;
      }
      replay=false;
      const recoverable=authorityMatches&&cached?.signature===signature&&!options.restart;
      if(pendingRecovery.size&&!recoverable){const recoveryError=new Error('Recovery blocked: unresolved worker termination prevents replacing this call');await releaseWorkspace(workspace);preparationStops.delete(workerId);finishDecision?.();throw recoveryError;}
      preparationStops.delete(workerId);
      finishDecision?.();
      const details={signature,status:'queued',...(options.nativeWorkspace?{readOnly,workspace:{workspaceId:workspace.workspaceId,cwd:workspace.cwd,isolated:workspace.isolated,baseCommit:workspace.baseCommit??null,roleDigest:workspace.roleDigest},workspaceId:workspace.workspaceId,authorityGeneration:workspace.authorityGeneration,roleDigest:workspace.roleDigest,...(workspace.baseCommit?{baseCommit:workspace.baseCommit}:{}),...(workspace.isolated?{worktree:workspace.cwd}:{})}:{sandbox,...(recoverable&&cached.worktree?{worktree:cached.worktree,baseCommit:cached.baseCommit}:{})})};
      if(worker)Object.assign(worker,details);else{worker={id:workerId,label:workerOptions.label??`Agent ${index+1}`,phase:workerPhase,agentType,isolation:isolated?'worktree':null,prompt,model,effort,options:workerOptions,activity:[],...details};state.workers.push(worker);}
      if(!state.phases.some(p=>p.name===workerPhase))state.phases.push({name:workerPhase});
      save();
      const operation=(async()=>{
        if(running>=concurrency)await new Promise(resolve=>queue.push(resolve));
        if(controller.signal.aborted||worker.stopRequested){worker.status=controller.signal.aborted?'stopped':'failed';if(worker.stopRequested)worker.error='Worker stopped';worker.output=null;save();if(workspace?.workspaceId!=null)try{await options.nativeWorkspace.release({runId:id,workerId:worker.id,workspaceId:workspace.workspaceId});}catch(error){worker.workspaceReleaseError=error.message;save();}return null;}
        running++;
        let aborter;
        try {
          const adapter=await connected();
          do {
            worker.restart=false;worker.stopRequested=false;
            aborter=new AbortController();active.set(worker.id,aborter);
            if(controller.signal.aborted)aborter.abort();
            worker.status='running';worker.startedAt=now();worker.launchIntent=true;save();
            let workerCwd=workspace?.cwd??cwd;
            if(!options.nativeWorkspace&&isolated&&!worker.worktree) {
              workerCwd=path.join(runDirectory(stateDir,id),`worktree-${index+1}${old?`-attempt-${state.attempt}`:''}`);
              worker.baseCommit=execFileSync('git',['rev-parse','HEAD'],{cwd,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
              execFileSync('git',['worktree','add','--detach',workerCwd,'HEAD'],{cwd,stdio:'pipe'});
              worker.worktree=workerCwd;save();
            } else workerCwd=worker.worktree??cwd;
            let prefixToken;
            try {
              const prefixKey=digest({model,effort,agentType,tools:{readOnly,...(options.nativeWorkspace?{authorityDigest:options.authorityDigest}:{sandbox})},schema:workerOptions.schema??null,cwd:workerCwd});
              prefixToken=await prefixStagger.enter(prefixKey,{signal:aborter.signal});
              if(state.launchCount>=1000)throw new Error('Total worker launch limit is 1000');
              state.launchCount++;save();
              const recoverThread=recoverable&&cached.status==='interrupted'?cached.threadId:undefined;
              let threadId=recoverThread;
              let result;
              const attempts=validate?structuredOutputAttempts():1;
              worker.validationAttempts=[];
              for(let attempt=1;attempt<=attempts;attempt++) {
                const repair=attempt===1?prompt:`Repair the previous response to satisfy the output schema. Return only the corrected structured output. Validation error: ${worker.validationAttempts.at(-1).error}`;
                worker.launchIntent=true;delete worker.turnId;save();
                try {
                  result=await adapter.run({runId:id,workerId:worker.id,authorityRef:options.authorityRef,authorityDigest:options.authorityDigest,authorityGeneration:workspace?.authorityGeneration,prompt:repair,model,effort,modelExplicit,effortExplicit,cwd:workerCwd,...(options.nativeWorkspace?{readOnly}:{sandbox,resolvedRole,approvalPolicy:permission.approvalPolicy,onApproval:request=>requestApproval({...request,stateDir,runId:id,attempt:state.attempt,workerId:worker.id})}),schema:workerOptions.schema,agentType,workspace:worker.workspace,signal:aborter.signal,threadId,timeoutMs:options.timeoutMs,onUpdate:update=>{if(update.firstResponseStarted)prefixStagger.started(prefixToken);Object.assign(worker,update,{rawStatus:update.status,status:normalizeWorkerStatus(update.status)});if(update.usage?.total){worker.rawUsage=update.usage;worker.usage=normalizeUsage(update.usage);}if(update.turnId)worker.launchIntent=false;save();}});
                } catch(error) {
                  if(error.code!=='INVALID_STRUCTURED_OUTPUT')throw error;
                  worker.validationAttempts.push({attempt,error:error.message});threadId=error.threadId??threadId;save();
                  if(attempt===attempts)throw error;
                  continue;
                }
                threadId=result.threadId??threadId;
                const output=result.output??result.text;
                if(!validate||validate(output))break;
                const error=new Error(`Output schema validation failed: ${ajv.errorsText(validate.errors)}`);
                error.code='INVALID_STRUCTURED_OUTPUT';error.threadId=threadId;
                worker.validationAttempts.push({attempt,error:error.message});save();
                if(attempt===attempts)throw error;
              }
              const output=result.output??result.text;
              pendingRecovery.delete(worker.id);state.uncertainWorkers=pendingRecovery.size>0;
              Object.assign(worker,result,{output,status:'completed',launchIntent:false});
              if(result.usage?.total){worker.rawUsage=result.usage;worker.usage=normalizeUsage(result.usage);}
              delete worker.error;
            } catch(error) {
              worker.status=controller.signal.aborted?'stopped':'failed';worker.error=error.message;worker.output=null;
              if(!/unresolved|not confirmed|disconnect|connection closed/i.test(error.message))worker.launchIntent=false;
              if(/unresolved|not confirmed|disconnect|connection closed/i.test(error.message)) {state.uncertainWorkers=true;worker.status='interrupted';worker.restart=false;pendingRecovery.set(worker.id,{...worker});}
              if(['INVALID_STRUCTURED_OUTPUT','INVALID_WORKER_CONFIGURATION'].includes(error.code))throw error;
            } finally {prefixStagger.finish(prefixToken);}
          } while(worker.restart&&!controller.signal.aborted);
          worker.endedAt=now();save();return worker.output??null;
        } finally {
          active.delete(worker.id);running--;queue.shift()?.();
          if(workspace?.workspaceId!=null)try{await options.nativeWorkspace.release({runId:id,workerId:worker.id,workspaceId:workspace.workspaceId});}catch(error){worker.workspaceReleaseError=error.message;save();}
        }
      })();
      pending.add(operation);operation.finally(()=>pending.delete(operation)).catch(()=>{});
      return operation;
    };
    // Preparation can outlive VM cancellation. Drain the complete host call,
    // including role/workspace resolution, before releasing runtime ownership.
    const invoke=(type,payload)=>{
      const request=call(type,payload);pending.add(request);
      request.finally(()=>pending.delete(request)).catch(()=>{});
      return request;
    };
    try {
      state.result=await executeScript(source,{args:state.args,call:invoke,signal:controller.signal,cpuMs:options.cpuMs,timeoutMs:options.timeoutMs});
      await Promise.allSettled([...pending]);
      state.status=interrupted==='restart'?'stopped':interrupted??'completed';
    } catch(error) {
      state.error=error.message;
      controller.abort(error);
      for(const aborter of active.values())aborter.abort();
      while(queue.length)queue.shift()();
      await Promise.allSettled([...pending]);
      state.status=interrupted==='restart'?'stopped':interrupted??'failed';
    }
    for(const [workerId,unresolved] of pendingRecovery){
      const index=state.workers.findIndex(worker=>worker.id===workerId);
      const retained={...unresolved,status:'interrupted'};
      if(index<0)state.workers.push(retained);else state.workers[index]=retained;
    }
    state.uncertainWorkers=pendingRecovery.size>0;
    if(state.uncertainWorkers)state.status='interrupted';
    state.endedAt=now();save();
    options.signal?.removeEventListener('abort',abort);
  } finally {
    clearInterval(timer);
    if(client&&!options.client){
      try {await client.close();if(state){state.serverExitedAt=now();state.serverPid=null;state.serverPgid=null;state.launchIntent=null;save();}}
      catch(error){if(state){state.status='interrupted';state.error=error.message;save();}}
    }
    ownsRun=false;
    release();
  }
  if(interrupted==='restart'&&!state.uncertainWorkers)return runWorkflow({...options,runId:id,resume:true,restart:true});
  return state;
}
