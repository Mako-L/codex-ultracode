import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdir,mkdtemp,readFile,stat,writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {PassThrough} from 'node:stream';
import {execFileSync} from 'node:child_process';
import {createBridgeServer,startBridge} from '../src/bridge/server.mjs';
import {createNativeAdapter} from '../src/bridge/adapter.mjs';
import {createPeer} from '../src/bridge/protocol.mjs';
import {parseScript} from '../src/workflow/script.mjs';
import {workflowConsentPresentation} from '../src/workflow/consent.mjs';
import {requestControl} from '../src/workflow/store.mjs';

const source=`export const meta={name:'native-probe',description:'Native probe'}; return await agent('hello',{isolation:'worktree'});`;
const missing=async file=>assert.rejects(()=>stat(file),error=>error.code==='ENOENT');
async function fixture(workerStart=async params=>({threadId:'thread-1',sessionId:'session-1',turnId:'turn-1',status:'completed',output:'native',text:'native',usage:{totalTokens:3},activity:[],model:params.model,effort:params.effort,error:null}),workerInterrupt=async()=>({status:'interrupted'})) {
  const cwd=await mkdtemp(path.join(tmpdir(),'ultracode-bridge-'));const calls=[];let server;
  const host={
    request:async(method,params,options)=>{
      calls.push([method,params,options]);
      if(method==='workspace.prepare')return {workspaceId:'workspace-1',cwd,isolated:params.isolation==='worktree',baseCommit:'base',authorityGeneration:4,roleDigest:'role-v1'};
      if(method==='worker.start')return workerStart(params,server);
      if(method==='worker.interrupt')return workerInterrupt(params,server);
      if(method==='workspace.release')return {eligible:true};
      throw new Error(`Unexpected host request: ${method}`);
    },
    notify:async event=>calls.push(['notify',event]),
  };
  server=createBridgeServer({cwd,stateDir:path.join(cwd,'.ultracode-native'),host});
  return {cwd,calls,server,host};
}
const wait=async(server,runId)=>{for(let i=0;i<200;i++){const state=await server.handle('inspectRun',{runId});if(state.status!=='running')return state;await new Promise(resolve=>setTimeout(resolve,5));}throw new Error('run did not finish');};

test('model catalog requires authoritative hello data',async()=>{
  const f=await fixture();await assert.rejects(()=>f.server.handle('models',{}),/catalog unavailable/i);
  const models=[{model:'gpt-5.6-luna',defaultEffort:'medium',supportedEfforts:['low','medium','high']}];
  await f.server.handle('hello',{protocolVersion:1,models});assert.deepEqual(await f.server.handle('models',{}),models);
});

test('source validation supplies consent metadata without launching or storing a run',async()=>{
  const f=await fixture();
  const validated=await f.server.handle('validateSource',{source,args:{ticket:4}});
  assert.deepEqual(validated.meta,parseScript(source).meta);assert.match(validated.digest,/^[a-f0-9]{64}$/);
  assert.deepEqual(validated.consent,workflowConsentPresentation(source,{...parseScript(source),args:{ticket:4}}));
  assert.deepEqual(await f.server.handle('listRuns',{}),{runs:[]});assert.deepEqual(f.calls,[]);
  await assert.rejects(f.server.handle('validateSource',{source:source+' const invalid = ;'}),/Unexpected token/);
  assert.deepEqual(f.calls,[]);
});

test('bridge reads and atomically persists workflow effort',async()=>{
  const f=await fixture();
  assert.deepEqual(await f.server.handle('getSettings',{}),{});
  await f.server.handle('hello',{protocolVersion:1,models:[{model:'capable',supportedEfforts:['xhigh']},{model:'limited',supportedEfforts:['low']}]});
  await assert.rejects(f.server.handle('setEffort',{effort:'ultracode',model:'limited'}),/does not support xhigh/);
  assert.deepEqual(await f.server.handle('setEffort',{effort:'ultracode',model:'capable'}),{effort:'ultracode',orchestrationEnabled:true});
  assert.deepEqual(await f.server.handle('getSettings',{}),{effort:'ultracode',orchestrationEnabled:true});
  assert.deepEqual(await f.server.handle('setEffort',{effort:'max'}),{effort:'max',orchestrationEnabled:false});
  assert.deepEqual(await f.server.handle('getSettings',{}),{effort:'max',orchestrationEnabled:false});
  await assert.rejects(f.server.handle('setEffort',{effort:'impossible'}),/Invalid effort/);
});

test('native run uses host workspace and worker without JS worktree mutation',async()=>{
  const f=await fixture();
  await assert.rejects(()=>f.server.handle('runSource',{source,args:null,model:'gpt-5.6-luna',effort:'low',authorityDigest:'digest'}),/authorityRef/i);
  const started=await f.server.handle('runSource',{source,args:null,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth-1',authorityDigest:'digest-1'});
  assert.equal(await readFile(started.scriptPath,'utf8'),source);
  assert.equal(path.dirname(started.scriptPath),started.transcriptDir);
  if(process.platform!=='win32')assert.equal((await stat(started.scriptPath)).mode&0o777,0o600);
  const state=await wait(f.server,started.runId);
  assert.equal(state.scriptPath,started.scriptPath);
  assert.equal(state.status,'completed');assert.equal(state.result,'native');assert.equal(state.authorityDigest,'digest-1');
  assert.deepEqual(f.calls.filter(([method])=>method!=='notify').map(([method])=>method),['workspace.prepare','worker.start','workspace.release']);
  const start=f.calls.find(([method])=>method==='worker.start')[1];
  assert.equal(start.runId,started.runId);assert.equal(start.workerId,'worker-1');assert.equal(start.authorityRef,'auth-1');assert.equal(start.authorityGeneration,4);
  assert.equal(f.calls.find(([method])=>method==='worker.start')[2].timeoutMs,null);
  assert.deepEqual(start.workspace,{workspaceId:'workspace-1',cwd:f.cwd,isolated:true,baseCommit:'base',roleDigest:'role-v1'});
  await missing(path.join(f.cwd,'.git'));
});

test('invalid native workspace responses release allocated workspaces',async()=>{
  for(const mutate of [
    workspace=>{workspace.authorityGeneration='invalid';},
    workspace=>{workspace.roleDigest='';},
    workspace=>{workspace.cwd='relative/cwd';},
    workspace=>{delete workspace.cwd;},
    workspace=>{workspace.cwd=42;},
  ]){
    const f=await fixture();
    const original=f.host.request;
    const releases=[];
    f.host.request=async(method,params,options)=>{
      if(method==='workspace.prepare'){
        const workspace={workspaceId:'allocated-workspace',cwd:f.cwd,isolated:true,baseCommit:'base',authorityGeneration:4,roleDigest:'role-v1'};
        mutate(workspace);
        return workspace;
      }
      if(method==='workspace.release'){
        releases.push(params);
        return {eligible:true};
      }
      return original(method,params,options);
    };
    const runId='invalid-workspace';
    await f.server.handle('runSource',{runId,source,authorityRef:'auth',authorityDigest:'digest'});
    const state=await wait(f.server,runId);
    assert.equal(state.status,'failed');
    assert.match(state.error,/Invalid native workspace response/);
    assert.deepEqual(releases,[{runId,workerId:'worker-1',workspaceId:'allocated-workspace'}]);
  }
});

test('invalid native workspace response preserves validation error when release fails',async()=>{
  const f=await fixture();
  const original=f.host.request;
  f.host.request=async(method,params,options)=>{
    if(method==='workspace.prepare')return {workspaceId:'allocated-workspace',cwd:'relative/cwd',isolated:true,baseCommit:'base',authorityGeneration:4,roleDigest:'role-v1'};
    if(method==='workspace.release')throw new Error('release failed');
    return original(method,params,options);
  };
  const runId='invalid-workspace-release-failure';
  await f.server.handle('runSource',{runId,source,authorityRef:'auth',authorityDigest:'digest'});
  const state=await wait(f.server,runId);
  assert.match(state.error,/Invalid native workspace response/);
  assert.equal(state.workers[0].workspaceReleaseError,'release failed');
});

test('native worker inherits parent model and effort unless explicitly overridden',async()=>{
  const f=await fixture();const inherited=await f.server.handle('runSource',{source,authorityRef:'auth',authorityDigest:'digest'});await wait(f.server,inherited.runId);
  let start=f.calls.find(([method])=>method==='worker.start')[1];assert.equal(start.model,null);assert.equal(start.effort,null);
  assert.equal(start.modelExplicit,false);assert.equal(start.effortExplicit,false);
  const explicit=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});await wait(f.server,explicit.runId);
  start=f.calls.filter(([method])=>method==='worker.start').at(-1)[1];assert.equal(start.model,'gpt-5.6-luna');assert.equal(start.effort,'low');
  assert.equal(start.modelExplicit,false);assert.equal(start.effortExplicit,false);
  const workerExplicit=source.replace("{isolation:'worktree'}","{isolation:'worktree',model:'gpt-5.6-sol',effort:'high'}");
  const direct=await f.server.handle('runSource',{source:workerExplicit,model:'parent-model',effort:'low',authorityRef:'auth',authorityDigest:'digest'});await wait(f.server,direct.runId);
  start=f.calls.filter(([method])=>method==='worker.start').at(-1)[1];assert.equal(start.model,'gpt-5.6-sol');assert.equal(start.effort,'high');assert.equal(start.modelExplicit,true);assert.equal(start.effortExplicit,true);
});

test('authority digest change invalidates replay',async()=>{
  const f=await fixture();
  const first=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth-1',authorityDigest:'digest-1'});await wait(f.server,first.runId);
  await f.server.handle('resumeRun',{runId:first.runId,authorityRef:'auth-2',authorityDigest:'digest-1'});await wait(f.server,first.runId);
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,1);
  await f.server.handle('resumeRun',{runId:first.runId,authorityRef:'auth-3',authorityDigest:'digest-2'});await wait(f.server,first.runId);
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,2);
});

test('a reopened session bridge resumes saved results only with fresh native authority',async()=>{
  const f=await fixture();
  const first=await f.server.handle('runSource',{source,authorityRef:'auth-1',authorityDigest:'digest'});await wait(f.server,first.runId);
  await f.server.shutdown();
  const reopened=createBridgeServer({cwd:f.cwd,stateDir:path.join(f.cwd,'.ultracode-native'),host:f.host});
  await assert.rejects(reopened.handle('resumeRun',{runId:first.runId}),/authority is unavailable/);
  await reopened.handle('resumeRun',{runId:first.runId,authorityRef:'auth-2',authorityDigest:'digest'});
  const resumed=await wait(reopened,first.runId);
  assert.equal(resumed.status,'completed');assert.equal(resumed.workers[0].cached,true);
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,1);
  await reopened.handle('resumeRun',{runId:first.runId,authorityRef:'auth-3',authorityDigest:'new-model',model:'gpt-5.6-sol',effort:'high'});
  const changed=await wait(reopened,first.runId);
  assert.equal(changed.model,'gpt-5.6-sol');assert.equal(changed.effort,'high');
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,2);
  await assert.rejects(reopened.handle('resumeRun',{runId:'missing',authorityRef:'auth-2',authorityDigest:'digest'}),/Nothing to resume/);
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,2);
});

test('native launch concurrency bounds simultaneous workers and persists for replay',async()=>{
  let active=0,peak=0,launches=0;
  const f=await fixture(async()=>{
    const id=++launches;
    peak=Math.max(peak,++active);
    await new Promise(resolve=>setTimeout(resolve,20));
    active--;
    return {threadId:`thread-${id}`,turnId:'turn-1',status:'completed',output:'done',text:'done',usage:{},activity:[]};
  });
  const started=await f.server.handle('runSource',{
    source:`export const meta={name:'bounded-native',description:'Bound native workers'}; return await pipeline(['one','two','three'],task=>agent(task));`,
    concurrency:1,authorityRef:'auth',authorityDigest:'digest',
  });
  const completed=await wait(f.server,started.runId);
  assert.equal(completed.status,'completed');
  assert.equal(completed.concurrency,1);
  assert.equal(peak,1);
  assert.equal(launches,3);
  await f.server.handle('resumeRun',{runId:started.runId,authorityRef:'auth',authorityDigest:'digest'});
  const resumed=await wait(f.server,started.runId);
  assert.equal(resumed.concurrency,1);
  assert.equal(launches,3);
});

test('unknown native worker outcome remains interrupted and blocks replay',async()=>{
  const error=Object.assign(new Error('worker start outcome unresolved'),{outcomeUnresolved:true});
  const f=await fixture(async()=>{throw error;});
  const started=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  const state=await wait(f.server,started.runId);
  assert.equal(state.status,'interrupted');assert.equal(state.workers[0].launchIntent,true);
  await assert.rejects(f.server.handle('resumeRun',{runId:started.runId,authorityRef:'auth',authorityDigest:'digest'}),/unresolved worker launch intent/i);
  const blocked=await wait(f.server,started.runId);
  assert.equal(blocked.status,'failed');assert.match(blocked.error,/unresolved worker launch intent/i);
  assert.equal(f.calls.filter(([method])=>method==='worker.start').length,1);
});

test('prepareSave returns immutable data without writing destination',async()=>{
  const f=await fixture();const started=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});await wait(f.server,started.runId);
  const prepared=await f.server.handle('prepareSave',{runId:started.runId,name:'saved-probe',scope:'project'});
  assert.equal(parseScript(prepared.source).meta.name,'saved-probe');assert.equal(parseScript(prepared.source).body,parseScript(source).body);assert.match(prepared.digest,/^[a-f0-9]{64}$/);assert.equal(prepared.relativePath,path.join('.codex','workflows','saved-probe.js'));
  await missing(path.join(f.cwd,prepared.relativePath));
});

test('pause interrupts the exact native worker and settles paused',async()=>{
  let finish,acknowledged=false;
  const terminal=new Promise(resolve=>{finish=resolve;});
  const f=await fixture(async(params,server)=>{
    server.handleEvent({event:'worker.updated',runId:params.runId,workerId:params.workerId,threadId:'thread-pause',sessionId:'session-pause',turnId:'turn-pause',status:'running',text:'working',usage:null,activity:[],revision:1});
    await terminal;
    return {threadId:'thread-pause',sessionId:'session-pause',turnId:'turn-pause',status:'interrupted',error:'Native worker interrupted'};
  },async()=>{finish();await new Promise(resolve=>setTimeout(resolve,20));acknowledged=true;return {status:'interrupted'};});
  const started=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  for(let i=0;i<100&&!f.calls.some(([method])=>method==='worker.start');i++)await new Promise(resolve=>setTimeout(resolve,5));
  await f.server.handle('pauseRun',{runId:started.runId});
  const state=await wait(f.server,started.runId);
  assert.equal(state.status,'paused');assert.equal(state.workers[0].status,'stopped');assert.equal(acknowledged,true);
  const interrupted=f.calls.find(([method])=>method==='worker.interrupt')[1];
  assert.deepEqual({runId:interrupted.runId,workerId:interrupted.workerId,threadId:interrupted.threadId,turnId:interrupted.turnId},{runId:started.runId,workerId:'worker-1',threadId:'thread-pause',turnId:'turn-pause'});
  const revision=f.calls.filter(([method])=>method==='notify').at(-1)[1].revision;
  await f.server.handle('stopRun',{runId:started.runId,workerId:'worker-1'});
  assert.equal((await f.server.handle('inspectRun',{runId:started.runId})).workers[0].status,'failed');
  const stopped=await f.server.handle('stopRun',{runId:started.runId});
  assert.equal(stopped.status,'stopped');
  assert.equal((await f.server.handle('inspectRun',{runId:started.runId})).status,'stopped');
  assert.ok(f.calls.filter(([method])=>method==='notify').at(-1)[1].revision>revision);
});

test('native worker restart waits for termination and interrupt acknowledgement', {timeout:5000}, async t=>{
  let finish, acknowledge, interrupted;
  const terminal=new Promise(resolve=>{finish=resolve;});
  const acknowledgement=new Promise(resolve=>{acknowledge=resolve;});
  const interruptRequested=new Promise(resolve=>{interrupted=resolve;});
  t.after(()=>{finish();acknowledge();});
  let starts=0;
  const f=await fixture(async(params,server)=>{
    const attempt=++starts;
    const identity={threadId:`thread-${attempt}`,turnId:`turn-${attempt}`};
    server.handleEvent({event:'worker.updated',runId:params.runId,workerId:params.workerId,...identity,status:'running',revision:1});
    if(attempt===1){await terminal;return {...identity,status:'interrupted'};}
    return {...identity,status:'completed',output:'restarted'};
  },async()=>{interrupted();await acknowledgement;return {status:'interrupted'};});
  const run=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  for(let i=0;i<100&&starts===0;i++)await new Promise(resolve=>setTimeout(resolve,5));
  assert.equal(starts,1);
  await f.server.handle('restartWorker',{runId:run.runId,workerId:'worker-1'});
  await interruptRequested;
  assert.equal(starts,1);
  finish();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(starts,1,'a pending interrupt acknowledgement must prevent overlapping attempts');
  acknowledge();
  const state=await wait(f.server,run.runId);
  assert.equal(state.status,'completed');
  assert.equal(starts,2);
  assert.equal(state.workers.length,1);
  assert.equal(state.workers[0].id,'worker-1');
  assert.equal(state.workers[0].turnId,'turn-2');
  assert.equal(state.workers[0].output,'restarted');
  const request=f.calls.find(([method])=>method==='worker.interrupt')[1];
  assert.equal(request.threadId,'thread-1');
  assert.equal(request.turnId,'turn-1');
});

for(const scope of ['worker','run'])test(`native ${scope} restart cannot relaunch an unresolved attempt`, {timeout:5000}, async t=>{
  let finish;
  const terminal=new Promise(resolve=>{finish=resolve;});
  t.after(()=>finish());
  let starts=0;
  const f=await fixture(async(params,server)=>{
    if(++starts>1)return {threadId:'duplicate',turnId:'duplicate',status:'completed',output:'unsafe duplicate'};
    server.handleEvent({event:'worker.updated',runId:params.runId,workerId:params.workerId,threadId:'uncertain-thread',turnId:'uncertain-turn',status:'running',revision:1});
    await terminal;
    throw Object.assign(new Error('worker start outcome unresolved'),{outcomeUnresolved:true});
  },async()=>{finish();throw new Error('interrupt rejected: active turn mismatch');});
  const run=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  for(let i=0;i<100&&starts===0;i++)await new Promise(resolve=>setTimeout(resolve,5));
  assert.equal(starts,1);
  if(scope==='worker')await f.server.handle('restartWorker',{runId:run.runId,workerId:'worker-1'});
  else requestControl(path.join(f.cwd,'.ultracode-native'),run.runId,{type:'restart'});
  const state=await wait(f.server,run.runId);
  assert.equal(starts,1,'unknown termination must not permit a duplicate live worker');
  assert.equal(state.status,'interrupted');
  assert.equal(state.workers[0].threadId,'uncertain-thread');
  assert.equal(state.workers[0].turnId,'uncertain-turn');
  assert.equal(state.workers[0].status,'interrupted');
});

test('native adapter forwards final interrupted accounting before a failed acknowledgement', {timeout:5000}, async()=>{
  const controller=new AbortController(),updates=[];
  let finish,acknowledge,interruptStarted;
  const terminal=new Promise(resolve=>{finish=resolve;});
  const interrupted=new Promise(resolve=>{interruptStarted=resolve;});
  const acknowledgement=new Promise((_,reject)=>{acknowledge=()=>reject(new Error('close not confirmed'));});
  const adapter=createNativeAdapter({host:{request:async method=>{
    if(method==='worker.start')return terminal;
    assert.equal(method,'worker.interrupt');interruptStarted();return acknowledgement;
  }}});
  const running=adapter.run({runId:'run',workerId:'worker',signal:controller.signal,onUpdate:update=>updates.push(update)});
  adapter.handleEvent({event:'worker.updated',runId:'run',workerId:'worker',threadId:'thread',turnId:'turn',status:'running',revision:1});
  controller.abort();await interrupted;
  const final={threadId:'thread',turnId:'turn',status:'interrupted',usage:{total:{totalTokens:23}},activity:[{id:'final-command',type:'commandExecution'}]};
  finish(final);await new Promise(resolve=>setImmediate(resolve));
  assert.deepEqual(updates.at(-1),final);
  acknowledge();await assert.rejects(running,/worker start outcome unresolved/);
});

test('native cancellation uses terminal identity when no progress event arrived', {timeout:5000}, async t=>{
  for(const rejected of [false,true])await t.test(rejected?'completed turn rejected':'delayed acknowledgement',async()=>{
    const controller=new AbortController(),updates=[],calls=[];
    let finish,acknowledge,rejectAcknowledgement,interruptStarted;
    const terminal=new Promise(resolve=>{finish=resolve;});
    const interrupted=new Promise(resolve=>{interruptStarted=resolve;});
    const acknowledgement=new Promise((resolve,reject)=>{acknowledge=resolve;rejectAcknowledgement=reject;});
    const adapter=createNativeAdapter({host:{request:async(method,params)=>{
      calls.push({method,params});
      if(method==='worker.start')return terminal;
      assert.equal(method,'worker.interrupt');interruptStarted();return acknowledgement;
    }}});
    const running=adapter.run({runId:'run',workerId:'worker',authorityRef:'auth',authorityGeneration:4,signal:controller.signal,onUpdate:update=>updates.push(update)});
    let settled=false;running.then(()=>{settled=true;},()=>{settled=true;});
    controller.abort();assert.equal(calls.length,1);
    const final={threadId:'terminal-thread',turnId:'terminal-turn',status:'completed',output:'too late',usage:{total:{totalTokens:17}},activity:[{id:'terminal-command',type:'commandExecution'}]};
    finish(final);await interrupted;await new Promise(resolve=>setImmediate(resolve));
    assert.equal(settled,false,'terminal completion must not bypass the pending interruption acknowledgement');
    assert.deepEqual(updates,[final]);
    assert.deepEqual(calls[1],{method:'worker.interrupt',params:{runId:'run',workerId:'worker',authorityRef:'auth',authorityGeneration:4,threadId:'terminal-thread',turnId:'terminal-turn'}});
    if(rejected)rejectAcknowledgement(new Error('no active turn to interrupt'));else acknowledge({status:'interrupted'});
    await assert.rejects(running,error=>{
      assert.equal(error.message,rejected?'no active turn to interrupt; worker start outcome unresolved':'Native worker interrupted');
      assert.equal(error.outcomeUnresolved,rejected?true:undefined);return true;
    });
  });
});

test('native cancellation without any terminal identity retains evidence and fails closed',async()=>{
  const controller=new AbortController(),updates=[];controller.abort();
  const final={status:'completed',usage:{total:{totalTokens:7}},activity:[]};
  const adapter=createNativeAdapter({host:{request:async method=>{assert.equal(method,'worker.start');return final;}}});
  await assert.rejects(adapter.run({runId:'run',workerId:'worker',signal:controller.signal,onUpdate:update=>updates.push(update)}),error=>error.outcomeUnresolved===true&&/identity unavailable/.test(error.message));
  assert.deepEqual(updates,[final]);
});

test('a rejected late interrupt prevents replacement but retains a resumable terminal identity', {timeout:5000}, async()=>{
  let finish,starts=0;
  const terminal=new Promise(resolve=>{finish=resolve;});
  const f=await fixture(async params=>{
    if(++starts===1){await terminal;return {threadId:'terminal-thread',turnId:'terminal-turn',status:'completed',output:'late',usage:{total:{totalTokens:17}}};}
    assert.equal(params.resumeThreadId,'terminal-thread');
    return {threadId:'terminal-thread',turnId:'recovered-turn',status:'completed',output:'recovered',usage:{total:{totalTokens:23}}};
  },async()=>{throw new Error('no active turn to interrupt');});
  const params={source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'};
  const run=await f.server.handle('runSource',params);
  for(let i=0;i<100&&starts===0;i++)await new Promise(resolve=>setTimeout(resolve,5));
  await f.server.handle('restartWorker',{runId:run.runId,workerId:'worker-1'});
  for(let i=0;i<100;i++){
    const state=await f.server.handle('inspectRun',{runId:run.runId});
    if(state.workers[0]?.restart)break;
    await new Promise(resolve=>setTimeout(resolve,5));
  }
  assert.equal((await f.server.handle('inspectRun',{runId:run.runId})).workers[0].restart,true);
  finish();
  const interrupted=await wait(f.server,run.runId);
  assert.equal(interrupted.status,'interrupted');assert.equal(starts,1);
  assert.equal(interrupted.workers[0].threadId,'terminal-thread');assert.equal(interrupted.workers[0].turnId,'terminal-turn');
  assert.deepEqual(interrupted.workers[0].usage,{totalTokens:17});
  await f.server.handle('resumeRun',{runId:run.runId,authorityRef:'auth',authorityDigest:'digest'});
  const recovered=await wait(f.server,run.runId);
  assert.equal(recovered.status,'completed',recovered.error);assert.equal(recovered.result,'recovered');assert.equal(starts,2);
  assert.deepEqual(recovered.workers[0].usage,{totalTokens:23});
});

test('native adapter interrupts over the real bridge when no timeout is configured', {timeout:5000}, async t=>{
  const requests=new PassThrough();
  const responses=new PassThrough();
  let finish,updated,interrupted,adapter;
  const terminal=new Promise(resolve=>{finish=resolve;});
  const running=new Promise(resolve=>{updated=resolve;});
  const interruptReceived=new Promise(resolve=>{interrupted=resolve;});
  const host=createPeer({input:requests,output:responses,onRequest:async(method,params)=>{
    if(method==='worker.start'){
      await host.notify({event:'worker.updated',runId:params.runId,workerId:params.workerId,threadId:'protocol-thread',turnId:'protocol-turn',status:'running',revision:1});
      await terminal;
      return {status:'interrupted'};
    }
    assert.equal(method,'worker.interrupt');
    assert.equal(params.threadId,'protocol-thread');
    assert.equal(params.turnId,'protocol-turn');
    interrupted();finish();return {status:'interrupted'};
  }});
  const peer=createPeer({input:responses,output:requests,onEvent:event=>adapter.handleEvent(event)});
  adapter=createNativeAdapter({host:peer});
  const controller=new AbortController();
  const result=adapter.run({runId:'protocol-run',workerId:'worker-1',prompt:'wait',signal:controller.signal,onUpdate:()=>updated()}).catch(error=>error);
  t.after(async()=>{finish();await Promise.all([peer.close(),host.close()]);await result;});
  await running;
  controller.abort();
  let timer;
  try{
    await Promise.race([interruptReceived,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('interrupt never reached the native host')),1000);})]);
  }finally{clearTimeout(timer);}
  assert.match((await result).message,/Native worker interrupted/);
});

test('stopRun forwards a validated worker id',async()=>{
  let finish;const terminal=new Promise(resolve=>{finish=resolve;});
  const f=await fixture(async(params,server)=>{server.handleEvent({event:'worker.updated',runId:params.runId,workerId:params.workerId,threadId:'thread-stop',turnId:'turn-stop',status:'running',revision:1});await terminal;return {threadId:'thread-stop',turnId:'turn-stop',status:'interrupted'};},async()=>{finish();return {status:'interrupted'};});
  const run=await f.server.handle('runSource',{source,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  for(let i=0;i<100&&!f.calls.some(([method])=>method==='worker.start');i++)await new Promise(resolve=>setTimeout(resolve,5));
  await assert.rejects(f.server.handle('stopRun',{runId:run.runId,workerId:''}),/workerId is required/);
  await assert.rejects(f.server.handle('stopRun',{runId:run.runId,workerId:'missing'}),/Unknown worker/);
  await f.server.handle('stopRun',{runId:run.runId,workerId:'worker-1'});
  await wait(f.server,run.runId);
  const interrupted=f.calls.find(([method])=>method==='worker.interrupt')[1];
  assert.equal(interrupted.workerId,'worker-1');
});

test('native adapter ignores stale worker updates',async()=>{
  let finish;const completion=new Promise(resolve=>{finish=resolve;});const updates=[];
  const adapter=createNativeAdapter({host:{request:async method=>method==='worker.start'?completion:{status:'interrupted'}}});
  const running=adapter.run({runId:'run',workerId:'worker-1',authorityRef:'auth',authorityGeneration:1,prompt:'x',model:'model',effort:'low',agentType:'general-purpose',workspace:{workspaceId:'shared',cwd:'/repo',isolated:false},onUpdate:update=>updates.push(update)});
  adapter.handleEvent({event:'worker.updated',runId:'run',workerId:'worker-1',threadId:'thread',turnId:'turn',status:'running',text:'new',firstResponseStarted:true,revision:2});
  adapter.handleEvent({event:'worker.updated',runId:'run',workerId:'worker-1',threadId:'thread',turnId:'turn',status:'running',text:'old',revision:1});
  finish({threadId:'thread',turnId:'turn',status:'completed',output:'done'});await running;
  assert.equal(updates.length,1);assert.equal(updates[0].text,'new');assert.equal(updates[0].firstResponseStarted,true);
});

test('saved catalog launches by opaque workflow identity',async()=>{
  const f=await fixture();const directory=path.join(f.cwd,'.codex','workflows');await mkdir(directory,{recursive:true});await writeFile(path.join(directory,'probe.js'),source);
  const listed=await f.server.handle('listSavedWorkflows',{cwd:f.cwd});
  const workflow=listed.workflows.find(item=>item.name==='native-probe');assert.equal(workflow.scope,'project');assert.notEqual(workflow.workflowId,'probe');
  const started=await f.server.handle('runSaved',{workflowId:workflow.workflowId,model:'gpt-5.6-luna',effort:'low',authorityRef:'auth',authorityDigest:'digest'});
  assert.equal((await wait(f.server,started.runId)).result,'native');
});

test('duplicate run and active resume preserve original owned job',async()=>{
  let finish;const terminal=new Promise(resolve=>{finish=resolve;});
  const f=await fixture(async(params,server)=>{server.handleEvent({event:'worker.updated',runId:params.runId,workerId:params.workerId,threadId:'thread',turnId:'turn',status:'running',revision:1});await terminal;return {threadId:'thread',turnId:'turn',status:'interrupted',error:'stopped'};},async()=>{finish();return {status:'interrupted'};});
  const params={runId:'same-run',source,authorityRef:'auth',authorityDigest:'digest'};
  await f.server.handle('runSource',params);
  for(let i=0;i<100&&!f.calls.some(([method])=>method==='worker.start');i++)await new Promise(resolve=>setTimeout(resolve,5));
  await assert.rejects(()=>f.server.handle('runSource',params),/active|conflict/i);
  await assert.rejects(()=>f.server.handle('resumeRun',{runId:'same-run',authorityRef:'auth',authorityDigest:'digest'}),/active|conflict/i);
  await f.server.handle('shutdown',{});
  assert.equal(f.calls.filter(([method])=>method==='worker.interrupt').length,1);
});

test('resume preserves omitted args and accepts explicit null',async()=>{
  const f=await fixture();const argsSource=`export const meta={name:'args',description:'Args'}; return args;`;
  const first=await f.server.handle('runSource',{source:argsSource,args:{kept:true},authorityRef:'auth',authorityDigest:'digest'});assert.deepEqual((await wait(f.server,first.runId)).result,{kept:true});
  await f.server.handle('resumeRun',{runId:first.runId,authorityRef:'auth',authorityDigest:'digest'});assert.deepEqual((await wait(f.server,first.runId)).result,{kept:true});
  await f.server.handle('resumeRun',{runId:first.runId,args:null,authorityRef:'auth',authorityDigest:'digest'});assert.equal((await wait(f.server,first.runId)).result,null);
});

test('catalog includes distinct ancestor and personal workflows with nearest precedence',async()=>{
  const root=await mkdtemp(path.join(tmpdir(),'ultracode-catalog-')),nested=path.join(root,'pkg'),home=path.join(root,'home');
  execFileSync('git',['init'],{cwd:root,stdio:'ignore'});await mkdir(path.join(root,'.codex','workflows'),{recursive:true});await mkdir(path.join(nested,'.codex','workflows'),{recursive:true});await mkdir(path.join(home,'workflows'),{recursive:true});
  const flow=name=>`export const meta={name:'${name}',description:'${name}'}; return '${name}';`;
  await writeFile(path.join(root,'.codex','workflows','ancestor.js'),flow('ancestor'));
  await writeFile(path.join(root,'.codex','workflows','same.js'),flow('same'));
  await writeFile(path.join(nested,'.codex','workflows','nearest.js'),flow('nearest'));
  await writeFile(path.join(nested,'.codex','workflows','same.js'),flow('same'));
  await writeFile(path.join(home,'workflows','personal.js'),flow('personal'));
  const host={request:async()=>{},notify:async()=>{}};const server=createBridgeServer({cwd:nested,stateDir:path.join(root,'state'),codexHome:home,host});
  const names=(await server.handle('listSavedWorkflows',{cwd:nested})).workflows.map(item=>item.name).sort();
  assert.deepEqual(names,['ancestor','nearest','personal','same']);
});

test('input EOF shuts down owned inflight runtime',async()=>{
  const input=new PassThrough(),output=new PassThrough();output.resume();let aborted=false;
  const runtime=async options=>{options.onUpdate({id:options.runId,status:'running',workers:[],source:options.source});await new Promise(resolve=>options.signal.addEventListener('abort',()=>{aborted=true;resolve();},{once:true}));return {id:options.runId,status:'stopped',workers:[],source:options.source};};
  const {server}=startBridge({input,output,cwd:await mkdtemp(path.join(tmpdir(),'ultracode-eof-')),runtime});
  await server.handle('runSource',{source:`export const meta={name:'eof',description:'EOF'}; return 1;`,authorityRef:'auth',authorityDigest:'digest'});
  input.end();for(let i=0;i<100&&!aborted;i++)await new Promise(resolve=>setTimeout(resolve,5));
  assert.equal(aborted,true);
});
