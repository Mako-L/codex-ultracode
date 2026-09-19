import fs from 'node:fs';
import path from 'node:path';
import {homedir} from 'node:os';
import {randomUUID} from 'node:crypto';
import {createPeer} from './protocol.mjs';
import {createNativeAdapter} from './adapter.mjs';
import {parseScript,renameWorkflowSource} from '../workflow/script.mjs';
import {workflowConsentPresentation} from '../workflow/consent.mjs';
import {runWorkflow} from '../workflow/runtime.mjs';
import {atomicJSON,digest,listRuns,readRun,requestControl,runDirectory} from '../workflow/store.mjs';
import {workflowCatalog} from '../workflow/catalog.mjs';

const terminal=new Set(['completed','failed','stopped','interrupted']);
const fail=(message,code='INVALID_REQUEST')=>Object.assign(new Error(message),{code});
const required=(value,name)=>{if(typeof value!=='string'||!value)throw fail(`${name} is required`);return value;};
const publicState=state=>structuredClone(state);

export function createBridgeServer({cwd=process.cwd(),stateDir=path.join(cwd,'.ultracode','native'),codexHome=process.env.CODEX_HOME??path.join(homedir(),'.codex'),host,runtime=runWorkflow}={}) {
  if(!host?.request||!host?.notify)throw new TypeError('Native host peer is required');
  cwd=path.resolve(cwd);stateDir=path.resolve(stateDir);
  const jobs=new Map(),saved=new Map(),revisions=new Map();let closing=false,models=null,plugins=[],webSearchAvailable=false;
  const adapter=createNativeAdapter({host});
  const settingsFile=path.join(stateDir,'settings.json');
  const readSettings=()=>{try{return JSON.parse(fs.readFileSync(settingsFile,'utf8'));}catch(error){if(error.code==='ENOENT')return {};throw error;}};
  const current=runId=>jobs.get(runId)?.state??readRun(stateDir,runId);
  const notify=state=>{const revision=(revisions.get(state.id)??0)+1;revisions.set(state.id,revision);return host.notify({event:'runChanged',runId:state.id,revision}).catch(()=>{});};
  const launch=(params,{resume=false,restart=false}={})=>{
    if(closing)throw fail('Bridge is shutting down','CONFLICT');
    const authorityRef=required(params.authorityRef,'authorityRef'),authorityDigest=required(params.authorityDigest,'authorityDigest');
    const runId=params.runId??randomUUID();
    const existing=jobs.get(runId);
    if(existing?.state.status==='running')throw fail(`Run ${runId} is already active`,'CONFLICT');
    if(!resume){try{readRun(stateDir,runId);throw fail(`Run ${runId} already exists`,'CONFLICT');}catch(error){if(error.code!=='ENOENT')throw error;}}
    const controller=new AbortController();
    const job={controller,authorityRef,authorityDigest,state:{id:runId,status:'running',workers:[],cwd,authorityDigest}};jobs.set(runId,job);
    let resolveStarted,rejectStarted;
    const started=new Promise((resolve,reject)=>{resolveStarted=resolve;rejectStarted=reject;});
    const acknowledge=state=>resolveStarted({runId,status:'running',...(state.scriptPath?{scriptPath:state.scriptPath,transcriptDir:runDirectory(stateDir,runId),workflowName:state.name}:{})});
    const nativeWorkspace={prepare:request=>adapter.prepare(request),release:request=>adapter.release(request)};
    const options={runId,resume,restart,source:params.source,model:params.model,effort:params.effort,cwd,stateDir,client:adapter,nativeWorkspace,authorityRef,authorityDigest,signal:controller.signal,onUpdate:state=>{job.state=state;acknowledge(state);notify(state);}};
    if(Object.hasOwn(params,'args'))options.args=params.args;
    if(Object.hasOwn(params,'concurrency'))options.concurrency=params.concurrency;
    if(Object.hasOwn(params,'isolateWrites')) {
      if(typeof params.isolateWrites!=='boolean')throw fail('isolateWrites must be boolean');
      options.isolateWrites=params.isolateWrites;
    }
    job.promise=Promise.resolve().then(()=>runtime(options)).then(state=>{job.state=state;acknowledge(state);return state;},error=>{job.state={...job.state,status:'failed',error:error.message};rejectStarted(error);return job.state;});
    return started;
  };
  const catalog=directory=>{
    saved.clear();
    return workflowCatalog(directory,{codexHome,plugins,webSearchAvailable}).map(record=>{saved.set(record.workflowId,record);const {file,...publicRecord}=record;return publicRecord;});
  };
  const handle=async(method,params={})=>{
    if(!params||typeof params!=='object'||Array.isArray(params))throw fail('Request params must be an object');
    if(method==='hello'){
      if(params.protocolVersion!==1)throw fail('Unsupported bridge protocol version','VERSION_MISMATCH');
      if(params.models!==undefined){if(!Array.isArray(params.models))throw fail('Invalid model catalog');models=structuredClone(params.models);}
      if(params.plugins!==undefined){if(!Array.isArray(params.plugins))throw fail('Invalid plugin catalog');plugins=structuredClone(params.plugins);saved.clear();}
      if(params.webSearchAvailable!==undefined){if(typeof params.webSearchAvailable!=='boolean')throw fail('Invalid web search availability');webSearchAvailable=params.webSearchAvailable;saved.clear();}
      return {protocolVersion:1,cwd};
    }
    if(method==='models'){if(!models)throw fail('Native model catalog unavailable','NOT_FOUND');return structuredClone(models);}
    if(method==='getSettings')return structuredClone(readSettings());
    if(method==='setEffort'){
      const effort=required(params.effort,'effort');
      if(!['low','medium','high','xhigh','max','ultra','ultracode'].includes(effort))throw fail('Invalid effort');
      if(effort==='ultracode'){
        const model=required(params.model,'model'),entry=models?.find(item=>item.model===model);
        if(!entry?.supportedEfforts?.includes('xhigh'))throw fail(`Model ${model} does not support xhigh effort`,'NOT_SUPPORTED');
      }
      const settings={...readSettings(),effort,orchestrationEnabled:effort==='ultracode'};
      atomicJSON(settingsFile,settings);return structuredClone(settings);
    }
    if(method==='listRuns')return {runs:listRuns(stateDir).map(publicState)};
    if(method==='inspectRun')return publicState(current(required(params.runId,'runId')));
    if(method==='listSavedWorkflows')return {workflows:catalog(path.resolve(params.cwd??cwd))};
    if(method==='validateSource'){
      const source=required(params.source,'source'),parsed=parseScript(source);
      return {meta:parsed.meta,digest:digest(source),consent:workflowConsentPresentation(source,{...parsed,...(Object.hasOwn(params,'args')?{args:params.args}:{})})};
    }
    if(method==='runSource') {parseScript(required(params.source,'source'));return launch(params);}
    if(method==='runSaved'||method==='readSavedSource') {
      const record=saved.get(required(params.workflowId,'workflowId'));if(!record)throw fail('Saved workflow not found','NOT_FOUND');
      const source=fs.readFileSync(record.file,'utf8');if(digest(source)!==record.digest)throw fail('Saved workflow changed','CONFLICT');
      if(method==='readSavedSource')return {workflowId:record.workflowId,source,digest:record.digest};
      parseScript(source);
      return launch({...params,source});
    }
    if(method==='pauseRun'||method==='stopRun') {
      const runId=required(params.runId,'runId'),state=current(runId);let workerId;
      if(Object.hasOwn(params,'workerId')&&params.workerId!==null){workerId=required(params.workerId,'workerId');if(!state.workers?.some(worker=>worker.id===workerId))throw fail('Unknown worker','NOT_FOUND');}
      requestControl(stateDir,runId,{type:method==='pauseRun'?'pause':'stop',...(workerId?{workerId}:{})});
      if(state.status==='paused'){const updated=readRun(stateDir,runId),job=jobs.get(runId);if(job)job.state=updated;await notify(updated);}
      return {runId,workerId:workerId??null,status:current(runId).status};
    }
    if(method==='resumeRun') {
      const runId=required(params.runId,'runId'),job=jobs.get(runId);let state;
      try{state=current(runId);}catch(error){if(error.code==='ENOENT')throw fail('Nothing to resume: saved workflow results are unavailable','NOT_FOUND');throw error;}
      const authorityRef=params.authorityRef??job?.authorityRef,authorityDigest=params.authorityDigest??job?.authorityDigest;
      if(!authorityRef||!authorityDigest)throw fail('Resume authority is unavailable','STALE_AUTHORITY');
      return launch({...params,runId,model:params.model??state.model,effort:params.effort??state.effort,authorityRef,authorityDigest},{resume:true});
    }
    if(method==='restartWorker') {
      const runId=required(params.runId,'runId'),workerId=required(params.workerId,'workerId'),job=jobs.get(runId);
      if(!job||(params.authorityRef&&params.authorityRef!==job.authorityRef)||(params.authorityDigest&&params.authorityDigest!==job.authorityDigest))throw fail('Worker restart authority is stale','STALE_AUTHORITY');
      requestControl(stateDir,runId,{type:'restart',workerId});return {runId,workerId,status:current(runId).status};
    }
    if(method==='prepareSave') {
      const state=current(required(params.runId,'runId')),name=required(params.name,'name');if(!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))throw fail('Invalid workflow name');
      if(!['project','user'].includes(params.scope))throw fail('Invalid save scope');
      const source=renameWorkflowSource(state.source,name);
      return {source,digest:digest(source),scope:params.scope,relativePath:params.scope==='project'?path.join('.codex','workflows',`${name}.js`):path.join('workflows',`${name}.js`)};
    }
    if(method==='shutdown') {closing=true;for(const job of jobs.values())if(!terminal.has(job.state.status))job.controller.abort(new Error('Native bridge shutdown'));await Promise.allSettled([...jobs.values()].map(job=>job.promise));return {stopped:true};}
    throw fail(`Unknown bridge method: ${method}`,'NOT_FOUND');
  };
  return {handle,handleEvent:event=>adapter.handleEvent(event),shutdown:()=>handle('shutdown',{})};
}

export function startBridge({input=process.stdin,output=process.stdout,cwd=process.cwd(),stateDir,runtime}={}) {
  let server;
  let peer;
  peer=createPeer({input,output,onRequest:async(method,params)=>{const result=await server.handle(method,params);if(method==='shutdown')setImmediate(()=>peer.close());return result;},onEvent:event=>server.handleEvent(event)});
  server=createBridgeServer({cwd,stateDir,host:peer,runtime});
  const disconnected=()=>server.shutdown().finally(()=>peer.close()).catch(()=>{});input.once('end',disconnected);input.once('close',disconnected);input.once('error',disconnected);
  return {server,peer};
}
