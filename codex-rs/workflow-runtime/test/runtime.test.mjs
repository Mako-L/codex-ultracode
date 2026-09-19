import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, symlink, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync,spawn } from 'node:child_process';
const runtime = await import('../src/workflow/runtime.mjs').catch(() => ({}));
const store = await import('../src/workflow/store.mjs').catch(() => ({}));
const source = body => `export const meta={name:'probe',description:'Runtime probe'}; ${body}`;
async function fixture() {
  const cwd = await mkdtemp(path.join(tmpdir(), 'ultracode-test-'));
  let active = 0, peak = 0, calls = [];
  const client = {
    start: async () => {}, close: async () => {},
    run: async ({prompt, signal, onUpdate}) => {
      calls.push(prompt); active++; peak = Math.max(peak, active);
      onUpdate?.({threadId:`thread-${calls.length}`,turnId:'turn-1',model:'gpt-5.6-sol',modelProvider:'openai'});
      try {
        await new Promise((resolve, reject) => { const timer = setTimeout(resolve, 35); signal?.addEventListener('abort', () => {clearTimeout(timer); reject(new Error('interrupted'));}, {once:true}); });
        if (prompt === 'bad') throw new Error('provider unavailable');
        return {output:prompt,model:'gpt-5.6-sol',modelProvider:'openai',usage:{totalTokens:7}};
      } finally {active--;}
    },
  };
  return {cwd,stateDir:path.join(cwd,'.ultracode'),model:'gpt-5.6-sol',effort:'low',client,calls,peak:()=>peak};
}
test('runtime is implemented', () => assert.equal(typeof runtime.runWorkflow,'function'));

test('persisted untrusted approval policy cannot silently change on replay',async()=>{
  const f=await fixture();const run=await runtime.runWorkflow({...f,source:source('return await agent("x");'),permission:{sandbox:'read-only',approvalPolicy:'untrusted'}});
  assert.equal(run.status,'completed',run.error);assert.equal(run.permission.approvalPolicy,'untrusted');
  for(const approvalPolicy of ['never','on-request'])await assert.rejects(()=>runtime.runWorkflow({...f,runId:run.id,resume:true,permission:{sandbox:'read-only',approvalPolicy}}),/approval ceiling/);
});

test('full-access parent reaches editing workers while read-only roles remain narrowed',async()=>{
  const f=await fixture(),seen=[];
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{seen.push(options.sandbox);return {output:'ok'};};
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'danger-full-access',approvalPolicy:'never'},source:source('await agent("edit",{write:true}); return await agent("inspect",{agentType:"Explore"});')});
  assert.equal(run.status,'completed',run.error);assert.deepEqual(seen,['danger-full-access','read-only']);
});

test('workspace-write persisted ceiling cannot widen to full access',async()=>{
  const f=await fixture();
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("x");')});
  await assert.rejects(()=>runtime.runWorkflow({...f,runId:run.id,resume:true,permission:{sandbox:'danger-full-access',approvalPolicy:'never'}}),/permission ceiling/);
});
test('custom role preparation preserves parallel order and invalidates replay on role edits',async()=>{
  const f=await fixture();let revision=1;const starts=[];
  f.client.resolveRole=async(name)=>{await new Promise(resolve=>setTimeout(resolve,name==='slow'?30:1));return {name,digest:`${name}-${revision}`,model:'gpt-5.6-luna',effort:'low',config:{features:{shell_tool:false}}};};
  f.client.run=async input=>{starts.push(input);return {output:input.prompt};};
  const options={...f,source:source('return await parallel([()=>agent("first",{agentType:"slow"}),()=>agent("second",{agentType:"fast"})]);')};
  const initial=await runtime.runWorkflow(options);
  assert.equal(initial.status,'completed');assert.deepEqual(initial.workers.map(w=>w.id),['worker-1','worker-2']);
  assert.deepEqual(initial.workers.map(w=>w.prompt),['first','second']);
  assert.ok(starts.every(call=>call.model==='gpt-5.6-luna'&&call.resolvedRole.config.features.shell_tool===false));
  const replay=await runtime.runWorkflow({...options,runId:initial.id,resume:true});
  assert.equal(replay.status,'completed');assert.ok(replay.workers.every(w=>w.cached));assert.equal(starts.length,2);
  revision++;
  const changed=await runtime.runWorkflow({...options,runId:initial.id,resume:true});
  assert.equal(changed.status,'completed');assert.ok(changed.workers.every(w=>!w.cached));assert.equal(starts.length,4);
});
test('controls received during role preparation cannot be discarded by cached replay',async t=>{
  for(const control of ['stop','pause'])await t.test(control,async()=>{
    const f=await fixture();let block=false,release,requested=false,starts=0;
    f.client.resolveRole=async()=>{if(block)await new Promise(resolve=>{release=resolve;});return {name:'custom',digest:'same-role',config:{}};};
    f.client.run=async()=>{starts++;return {output:'cached-result'};};
    const options={...f,source:source('return await agent("first",{agentType:"custom"});')};
    const first=await runtime.runWorkflow(options);assert.equal(first.status,'completed');block=true;
    const resumed=await runtime.runWorkflow({...options,runId:first.id,resume:true,onUpdate:state=>{
      const worker=state.workers[0];
      if(worker?.status==='preparing'&&!requested){requested=true;store.requestControl(f.stateDir,first.id,{type:control,...(control==='pause'?{}:{workerId:worker.id})});if(control==='pause')setTimeout(()=>release?.(),100);}
      if(worker?.stopRequested||worker?.restart){if(release){const done=release;release=null;done();}}
    }});
    assert.notEqual(resumed.workers[0].cached,true);
    assert.equal(starts,1);assert.equal(resumed.workers[0].status,control==='pause'?'stopped':'failed');
  });
});
test('a paused run can be stopped without a live supervisor',async()=>{
  const f=await fixture();
  store.writeRun(f.stateDir,{id:'paused-control',status:'paused',workers:[{id:'worker-1',status:'stopped'}]});
  store.requestControl(f.stateDir,'paused-control',{type:'stop'});
  assert.equal(store.readRun(f.stateDir,'paused-control').status,'stopped');
  assert.equal(store.consumeControl(f.stateDir,'paused-control'),null);
});
test('resume and restart preserve concurrency for fresh launches and allow an explicit override',async()=>{
  const f=await fixture();f.prefixStaggerMs=0;
  const first=await runtime.runWorkflow({...f,concurrency:1,source:source('return await pipeline(args,x=>agent(x));'),args:['a','b']});
  assert.equal(first.concurrency,1);
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true,args:['c','d']});
  assert.equal(resumed.concurrency,1);assert.equal(f.peak(),1);
  assert.deepEqual(resumed.result,['c','d']);assert.ok(resumed.workers.every(worker=>!worker.cached));
  const restarted=await runtime.runWorkflow({...f,runId:first.id,restart:true});
  assert.equal(restarted.concurrency,1);assert.equal(f.peak(),1);
  assert.deepEqual(restarted.result,['c','d']);assert.ok(restarted.workers.every(worker=>!worker.cached));
  const overridden=await runtime.runWorkflow({...f,runId:first.id,resume:true,concurrency:2,args:['e','f']});
  assert.equal(overridden.concurrency,2);assert.equal(f.peak(),2);
  assert.deepEqual(f.calls,['a','b','c','d','c','d','e','f']);
});
test('invalid concurrency releases ownership without replacing a stored run',async()=>{
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  const persisted=store.readRun(f.stateDir,first.id);
  await assert.rejects(runtime.runWorkflow({...f,runId:first.id,resume:true,concurrency:0}),/Concurrency must be 1–16/);
  assert.deepEqual(store.readRun(f.stateDir,first.id),persisted);
  const release=store.acquireRun(f.stateDir,first.id);release();
});
test('stopping one paused worker records failure and preserves the completed replay prefix',async()=>{
  const f=await fixture();let requested=false;
  const first=await runtime.runWorkflow({...f,source:source('await agent("first"); return await agent("second");'),onUpdate:state=>{
    if(!requested&&state.workers[1]?.turnId){requested=true;store.requestControl(f.stateDir,state.id,{type:'pause'});}
  }});
  assert.equal(first.status,'paused');assert.equal(first.workers[0].status,'completed');
  const persisted=store.readRun(f.stateDir,first.id);
  store.requestControl(f.stateDir,first.id,{type:'stop',workerId:'worker-2'});
  const stopped=store.readRun(f.stateDir,first.id);
  assert.equal(stopped.status,'paused');assert.equal(stopped.workers[1].status,'failed');
  assert.deepEqual(stopped.workers[0],persisted.workers[0]);
  assert.throws(()=>store.requestControl(f.stateDir,first.id,{type:'stop',workerId:'worker-1'}),/completed worker/);
  assert.deepEqual(store.readRun(f.stateDir,first.id),stopped);
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true});
  assert.equal(resumed.status,'completed');assert.equal(resumed.workers[0].cached,true);
  assert.deepEqual(f.calls,['first','second','second']);
});
test('predeclared future phases are visible before workers launch', async () => {
  const f=await fixture();
  const declared=`export const meta={name:'probe',description:'Runtime probe',phases:['Now','Later']}; return 1;`;
  const run=await runtime.runWorkflow({...f,source:declared});
  assert.deepEqual(run.phases.map(item=>item.name),['Now','Later']);
});
test('each invocation persists a readable script without overwriting an edited previous script',async()=>{
  const f=await fixture(),original=source('return 1;');
  const first=await runtime.runWorkflow({...f,source:original});
  assert.equal(await readFile(first.scriptPath,'utf8'),original);
  const edited=source('return 2;');await writeFile(first.scriptPath,edited);
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true,source:edited});
  assert.equal(resumed.result,2);assert.notEqual(resumed.scriptPath,first.scriptPath);
  assert.equal(await readFile(first.scriptPath,'utf8'),edited);
  assert.equal(await readFile(resumed.scriptPath,'utf8'),edited);
  assert.equal(resumed.history.at(-1).scriptPath,first.scriptPath);
});
test('scheduler bounds concurrency and preserves failed calls as audit evidence', async () => {
  const f = await fixture();
  const run = await runtime.runWorkflow({...f,concurrency:2,source:source('phase("Audit"); return await pipeline(args,x=>agent(x));'),args:['a','bad','c','d']});
  assert.equal(f.peak(),2);
  assert.equal(run.status,'completed');
  assert.deepEqual(run.result,['a',null,'c','d']);
  assert.equal(run.workers[1].status,'failed');
  assert.equal(run.workers[1].error,'provider unavailable');
  assert.equal(run.workers[0].phase,'Audit');
  assert.equal(run.workers[0].usage.totalTokens,7);
});
test('matching workers wait for first response while disjoint prefixes start immediately',async()=>{
  const matching=await fixture(),started=[],updates=new Map(),finishes=new Map();
  matching.client.run=async({prompt,onUpdate})=>{started.push(prompt);updates.set(prompt,onUpdate);await new Promise(resolve=>finishes.set(prompt,resolve));return {output:prompt};};
  const running=runtime.runWorkflow({...matching,prefixStaggerMs:5000,source:source('return await parallel([()=>agent("a"),()=>agent("b")]);')});
  while(started.length<1)await new Promise(resolve=>setTimeout(resolve,5));await new Promise(resolve=>setTimeout(resolve,20));assert.deepEqual(started,['a']);
  updates.get('a')({turnId:'turn-a',status:'running'});await new Promise(resolve=>setTimeout(resolve,20));assert.deepEqual(started,['a']);
  updates.get('a')({turnId:'turn-a',status:'running',firstResponseStarted:true});while(started.length<2)await new Promise(resolve=>setTimeout(resolve,5));
  finishes.get('a')();finishes.get('b')();assert.deepEqual((await running).result,['a','b']);

  const disjoint=await fixture(),disjointStarted=[],release=[];disjoint.client.run=async({prompt})=>{disjointStarted.push(prompt);await new Promise(resolve=>release.push(resolve));return {output:prompt};};
  const separate=runtime.runWorkflow({...disjoint,prefixStaggerMs:5000,source:source('return await parallel([()=>agent("a",{effort:"low"}),()=>agent("b",{effort:"high"})]);')});
  while(disjointStarted.length<2)await new Promise(resolve=>setTimeout(resolve,5));release.forEach(resolve=>resolve());await separate;
});
test('whole-run stop aborts held matching workers without launching them',async()=>{
  const f=await fixture();let requested=false;
  const run=await runtime.runWorkflow({...f,prefixStaggerMs:5000,source:source('return await parallel([()=>agent("leader"),()=>agent("held")]);'),onUpdate:state=>{
    if(!requested&&state.workers[0]?.turnId){requested=true;store.requestControl(f.stateDir,state.id,{type:'stop'});}
  }});
  assert.equal(run.status,'stopped');assert.deepEqual(f.calls,['leader']);assert.ok(run.workers.every(worker=>worker.status==='stopped'));
});
test('replay reuses only the unchanged successful prefix before a failed call', async () => {
  const f = await fixture();
  const code = source('return await pipeline(args,x=>agent(x));');
  const first = await runtime.runWorkflow({...f,source:code,args:['a','bad','c']});
  const second = await runtime.runWorkflow({...f,source:code,args:['a','b','c'],runId:first.id,resume:true});
  assert.deepEqual(f.calls,['a','bad','c','b','c']);
  assert.deepEqual(second.result,['a','b','c']);
  assert.equal(second.id,first.id);
});
test('worker options cannot grant filesystem access', async () => {
  const f = await fixture();
  const run = await runtime.runWorkflow({...f,source:source('return await agent("x",{sandbox:"danger-full-access"});')});
  assert.equal(run.status,'failed');
  assert.match(run.error,/option|permission|sandbox/i);
  assert.equal(f.calls.length,0);
});
test('ordinary workers inherit parent cwd and workspace-write sandbox', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  const seen=[];
  f.client.run=async options=>{seen.push({cwd:options.cwd,sandbox:options.sandbox});if(options.prompt==='write')await writeFile(path.join(options.cwd,'shared.txt'),'visible');return {output:options.prompt==='read'?await readFile(path.join(options.cwd,'shared.txt'),'utf8'):'ok'};};
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await pipeline(["write","read"],x=>agent(x));'),concurrency:1});
  assert.deepEqual(seen,[{cwd:f.cwd,sandbox:'workspace-write'},{cwd:f.cwd,sandbox:'workspace-write'}]);
  assert.equal(run.workers.some(worker=>worker.worktree),false);
  assert.deepEqual(run.result,['ok','visible']);
});
test('parent can disable write isolation so edit workers use the same folder', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  let seen;
  f.client.run=async options=>{seen=options;await writeFile(path.join(options.cwd,'same-folder.txt'),'here');return {output:'ok'};};
  const run=await runtime.runWorkflow({...f,isolateWrites:false,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(seen.cwd,f.cwd);
  assert.equal(run.workers[0].isolation,null);
  assert.equal(run.workers.some(worker=>worker.worktree),false);
  assert.equal(await readFile(path.join(f.cwd,'same-folder.txt'),'utf8'),'here');
});
test('write worker can opt out of isolation with isolation none', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  let seen;
  f.client.run=async options=>{seen=options;return {output:'ok'};};
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true,isolation:"none"});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(seen.cwd,f.cwd);
  assert.equal(run.workers[0].isolation,null);
});
test('worktree isolation is opt-in and cannot widen read-only parent', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  let seen;
  f.client.run=async options=>{seen=options;return {output:'ok'};};
  const run=await runtime.runWorkflow({...f,source:source('return await agent("x",{isolation:"worktree"});')});
  assert.notEqual(seen.cwd,f.cwd);assert.equal(seen.sandbox,'read-only');assert.equal(run.workers[0].isolation,'worktree');
  const invalid=await fixture();
  const failed=await runtime.runWorkflow({...invalid,source:source('return await agent("x",{isolation:"container"});')});
  assert.equal(failed.status,'failed');assert.equal(invalid.calls.length,0);
});
test('native workspace hook owns isolation and contributes to replay identity',async()=>{
  const f=await fixture();const prepared=[];const released=[];
  const nativeWorkspace={
    prepare:async request=>{prepared.push(request);return {workspaceId:'native-1',cwd:f.cwd,isolated:true,baseCommit:'base',authorityGeneration:7,roleDigest:'role-v1'};},
    release:async request=>{released.push(request);},
  };
  const code=source('return await agent("edit",{isolation:"worktree"});');
  const first=await runtime.runWorkflow({...f,source:code,authorityRef:'auth-1',authorityDigest:'digest-1',nativeWorkspace});
  assert.equal(first.status,'completed');assert.equal(first.workers[0].workspaceId,'native-1');assert.equal(first.workers[0].authorityGeneration,7);
  assert.equal(prepared[0].isolation,'worktree');assert.equal(released.length,1);
  assert.equal(f.calls.length,1);
  await runtime.runWorkflow({...f,runId:first.id,resume:true,authorityRef:'auth-2',authorityDigest:'digest-2',nativeWorkspace});
  assert.equal(f.calls.length,2);
});
test('parallel native preparation reserves unique ordered workers and accepts shared null identity',async()=>{
  const f=await fixture(),waiting=new Map(),starts=[];
  f.client.run=async options=>{starts.push([options.workerId,options.prompt,options.readOnly,options.sandbox]);return {output:options.prompt};};
  const nativeWorkspace={prepare:request=>new Promise(resolve=>waiting.set(request.workerId,()=>resolve({workspaceId:null,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:2,roleDigest:'role-v1'}))),release:async()=>{}};
  const code=source('return await parallel([()=>agent("a"),()=>agent(args,{write:false})]);');
  const running=runtime.runWorkflow({...f,source:code,args:'b',authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  while(waiting.size<2)await new Promise(resolve=>setTimeout(resolve,5));
  waiting.get('worker-2')();waiting.get('worker-1')();
  const first=await running;
  assert.deepEqual(first.workers.map(worker=>worker.id),['worker-1','worker-2']);assert.deepEqual(first.result,['a','b']);
  assert.deepEqual(starts,[['worker-1','a',false,undefined],['worker-2','b',true,undefined]]);
  waiting.clear();const resuming=runtime.runWorkflow({...f,source:code,args:'changed',runId:first.id,resume:true,authorityRef:'auth-2',authorityDigest:'digest',nativeWorkspace});
  while(waiting.size<2)await new Promise(resolve=>setTimeout(resolve,5));
  waiting.get('worker-2')();waiting.get('worker-1')();
  const resumed=await resuming;
  assert.equal(resumed.workers[0].cached,true);assert.equal(resumed.workers[1].cached,undefined);
  assert.deepEqual(starts.map(([,prompt])=>prompt),['a','b','changed']);
});
test('native preparation failures stop downstream replay at completed prefix',async()=>{
  const f=await fixture(),starts=[],runId='preparation-failure-replay';
  f.client.run=async options=>{starts.push(options.prompt);return {output:options.prompt};};
  const workspace=workerId=>({workspaceId:`workspace-${workerId}`,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:2,roleDigest:'role-v1'});
  const code=source('return await parallel([()=>agent("first"),()=>agent("second"),()=>agent("third")]);');
  const nativeWorkspace={prepare:async request=>workspace(request.workerId),release:async()=>{}};
  const first=await runtime.runWorkflow({...f,runId,source:code,authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  assert.equal(first.status,'completed');
  starts.length=0;
  for(const [name,prepare] of [
    ['rejection',async request=>request.workerId==='worker-2'?Promise.reject(new Error('workspace preparation failed')):workspace(request.workerId)],
    ['malformed metadata',async request=>request.workerId==='worker-2'?{...workspace(request.workerId),roleDigest:''}:workspace(request.workerId)],
  ]) {
    starts.length=0;
    const resumed=await runtime.runWorkflow({...f,runId,source:code,resume:true,authorityRef:'auth',authorityDigest:'digest',nativeWorkspace:{prepare,release:async()=>{}}});
    assert.equal(resumed.status,'failed',name);
    assert.equal(resumed.workers[0].cached,true,name);
    assert.equal(resumed.workers[1].status,'failed',name);
    assert.equal(resumed.workers[2].cached,undefined,name);
    assert.deepEqual(starts,['third'],name);
  }
});

test('cancellation cancels pending native preparation and releases late workspace',async()=>{
  for(const control of [{type:'stop',workerId:'worker-1',status:'completed'},{type:'pause',workerId:'worker-1',status:'completed'},{type:'stop',status:'stopped'}]){
    const f=await fixture(),released=[],starts=[];let resolvePrepare;const runId=`pending-native-${control.type}-${control.workerId?'worker':'run'}`;
    f.client.run=async options=>{starts.push(options);return {output:options.prompt};};
    const nativeWorkspace={
      prepare:async()=>new Promise(resolve=>{resolvePrepare=()=>resolve({workspaceId:'late-workspace',cwd:f.cwd,isolated:true,baseCommit:'base',authorityGeneration:2,roleDigest:'role-v1'});}),
      release:async request=>{released.push(request);},
    };
    const running=runtime.runWorkflow({...f,runId,source:source('return await agent("pending",{isolation:"worktree"});'),authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
    await waitUntil(()=>resolvePrepare);
    store.requestControl(f.stateDir,runId,{...control});
    await waitUntil(()=>store.readRun(f.stateDir,runId).workers[0]?.stopRequested||store.readRun(f.stateDir,runId).status==='stopped');
    const settled=await Promise.race([running.then(()=>true),new Promise(resolve=>setTimeout(()=>resolve(false),1000))]);
    resolvePrepare();
    const result=await Promise.race([running,new Promise((_,reject)=>setTimeout(()=>reject(new Error('runtime did not settle')),1000))]);
    await waitUntil(()=>released.length===1);
    assert.equal(settled,true);
    assert.equal(result.status,control.status);
    assert.equal(result.workers[0].status,control.workerId?'failed':'stopped');
    assert.equal(starts.length,0);
    assert.deepEqual(released,[{runId,workerId:'worker-1',workspaceId:'late-workspace'}]);
  }
});

test('individual stop cancels pending non-native role resolution without launching worker',async()=>{
  const f=await fixture(),starts=[];let resolveRole;const runId='pending-role-stop';
  f.client.resolveRole=async()=>new Promise(resolve=>{resolveRole=()=>resolve({name:'custom',digest:'custom-v1',model:'gpt-5.6-sol',effort:'low',config:{}});});
  f.client.run=async options=>{starts.push(options);return {output:options.prompt};};
  const running=runtime.runWorkflow({...f,runId,source:source('return await agent("pending",{agentType:"custom"});')});
  await waitUntil(()=>resolveRole);
  store.requestControl(f.stateDir,runId,{type:'stop',workerId:'worker-1'});
  await waitUntil(()=>store.readRun(f.stateDir,runId).workers[0]?.stopRequested);
  const settled=await Promise.race([running.then(()=>true),new Promise(resolve=>setTimeout(()=>resolve(false),1000))]);
  resolveRole();
  const result=await running;
  assert.equal(settled,true);
  assert.equal(result.status,'completed');
  assert.equal(result.workers[0].status,'failed');
  assert.equal(starts.length,0);
});

test('whole stop cancels worker waiting for preparation order and releases workspace',async()=>{
  const f=await fixture(),released=[],starts=[];let resolveFirst,resolveSecond,secondPrepared=false;const runId='pending-native-order-stop';
  f.client.run=async options=>{starts.push(options);return {output:options.prompt};};
  const workspace=workerId=>({workspaceId:`late-${workerId}`,cwd:f.cwd,isolated:true,baseCommit:'base',authorityGeneration:2,roleDigest:'role-v1'});
  const nativeWorkspace={
    prepare:async request=>request.workerId==='worker-1'?new Promise(resolve=>{resolveFirst=()=>resolve(workspace(request.workerId));}):(secondPrepared=true,workspace(request.workerId)),
    release:async request=>{released.push(request);},
  };
  const running=runtime.runWorkflow({...f,runId,source:source('return await parallel([()=>agent("first"),()=>agent("second")]);'),authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  await waitUntil(()=>secondPrepared);
  store.requestControl(f.stateDir,runId,{type:'stop'});
  const settled=await Promise.race([running.then(()=>true),new Promise(resolve=>setTimeout(()=>resolve(false),1000))]);
  let result;
  try{result=await Promise.race([running,new Promise((_,reject)=>setTimeout(()=>reject(new Error('runtime did not settle')),1000))]);}
  finally{resolveFirst();resolveSecond?.();}
  await waitUntil(()=>released.length===2);
  assert.equal(settled,true);
  assert.equal(result.status,'stopped');
  assert.ok(result.workers.every(worker=>worker.status==='stopped'));
  assert.equal(starts.length,0);
  assert.deepEqual(released.map(request=>request.workspaceId).sort(),['late-worker-1','late-worker-2']);
});

test('late release failure cannot overwrite a resumed run',async()=>{
  const f=await fixture(),runId='late-release-resume';let resolvePrepare,releaseLate,lateReleaseStarted=false,prepares=0;
  const workspace=workspaceId=>({workspaceId,cwd:f.cwd,isolated:true,baseCommit:'base',authorityGeneration:2,roleDigest:'role-v1'});
  const nativeWorkspace={
    prepare:async()=>{if(++prepares===1)return new Promise(resolve=>{resolvePrepare=()=>resolve(workspace('late-workspace'));});return workspace('resumed-workspace');},
    release:async request=>{if(request.workspaceId==='late-workspace'){lateReleaseStarted=true;return new Promise((_,reject)=>{releaseLate=reject;});}},
  };
  const running=runtime.runWorkflow({...f,runId,source:source('return await agent("pending",{isolation:"worktree"});'),authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  await waitUntil(()=>resolvePrepare);
  store.requestControl(f.stateDir,runId,{type:'stop'});
  const settled=await Promise.race([running.then(()=>true),new Promise(resolve=>setTimeout(()=>resolve(false),1000))]);
  resolvePrepare();
  await waitUntil(()=>lateReleaseStarted);
  let releaseFailureSent=false;
  const rejectLate=()=>{if(!releaseFailureSent){releaseFailureSent=true;releaseLate(new Error('late release failed'));}};
  if(!settled)rejectLate();
  const stopped=await Promise.race([running,new Promise((_,reject)=>setTimeout(()=>reject(new Error('run did not settle')),1000))]);
  assert.equal(settled,true);
  assert.equal(store.consumeControl(f.stateDir,runId),null);
  const resumed=await runtime.runWorkflow({...f,runId,resume:true,source:source('return await agent("pending",{isolation:"worktree"});'),authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  assert.equal(stopped.status,'stopped');
  assert.equal(resumed.status,'completed',JSON.stringify(resumed));
  rejectLate();
  await new Promise(resolve=>setImmediate(resolve));
  const diagnostic=await readFile(path.join(f.stateDir,'runs',runId,`workspace-release-${stopped.attempt}.log`),'utf8');
  assert.match(diagnostic,/late release failed/);
  const persisted=store.readRun(f.stateDir,runId);
  assert.equal(persisted.status,'completed');
  assert.equal(persisted.attempt,resumed.attempt);
});

test('cancelled pending worker stops replay at completed prefix',async()=>{
  const f=await fixture(),starts=[],released=[],runId='pending-replay-stop';
  f.client.run=async options=>{starts.push(options.prompt);return {output:options.prompt};};
  const workspace=workerId=>({workspaceId:`workspace-${workerId}`,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:2,roleDigest:'role-v1'});
  const nativeWorkspace={prepare:async request=>workspace(request.workerId),release:async request=>{released.push(request);}};
  const code=source('return await parallel([()=>agent("first"),()=>agent("second"),()=>agent("third")]);');
  const first=await runtime.runWorkflow({...f,runId,source:code,authorityRef:'auth',authorityDigest:'digest',nativeWorkspace});
  assert.equal(first.status,'completed');
  starts.length=0;released.length=0;
  let resolveSecond;const resumedWorkspace={
    prepare:async request=>request.workerId==='worker-2'?new Promise(resolve=>{resolveSecond=()=>resolve(workspace(request.workerId));}):workspace(request.workerId),
    release:async request=>{released.push(request);},
  };
  const resumedRun=runtime.runWorkflow({...f,runId,source:code,resume:true,authorityRef:'auth',authorityDigest:'digest',nativeWorkspace:resumedWorkspace});
  await waitUntil(()=>resolveSecond);
  store.requestControl(f.stateDir,runId,{type:'stop',workerId:'worker-2'});
  await waitUntil(()=>store.readRun(f.stateDir,runId).workers[1]?.stopRequested);
  let resumed,resumeError;
  try{resumed=await Promise.race([resumedRun,new Promise((_,reject)=>setTimeout(()=>reject(new Error('resume did not settle')),1000))]);}
  catch(error){resumeError=error;}
  finally{resolveSecond();}
  if(resumeError)throw resumeError;
  await waitUntil(()=>released.length===3);
  assert.equal(resumed.status,'completed');
  assert.equal(resumed.workers[0].cached,true);
  assert.equal(resumed.workers[1].status,'failed');
  assert.equal(resumed.workers[2].cached,undefined);
  assert.deepEqual(starts,['third']);
});

test('explicit phase remains attached during concurrent global phase changes', async () => {
  const f=await fixture();
  const run=await runtime.runWorkflow({...f,source:source('const first=agent("a",{phase:"Pinned"}); await phase("Later"); return await Promise.all([first,agent("b")]);')});
  assert.deepEqual(run.workers.map(worker=>worker.phase),['Pinned','Later']);
  const invalid=await runtime.runWorkflow({...await fixture(),source:source('return await agent("x",{phase:"  "});')});
  assert.equal(invalid.status,'failed');assert.equal(invalid.workers.length,0);
});
test('built-in roles narrow sandbox and unknown roles fail before launch', async () => {
  const f=await fixture();const seen=[];
  f.client.run=async options=>{seen.push(options);return {output:'ok'};};
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await pipeline(["Explore","Plan"],agentType=>agent(agentType,{agentType}));')});
  assert.deepEqual(seen.map(call=>call.sandbox),['read-only','read-only']);
  assert.deepEqual(run.workers.map(worker=>worker.agentType),['Explore','Plan']);
  const bad=await fixture();const failed=await runtime.runWorkflow({...bad,source:source('return await agent("x",{agentType:"custom"});')});
  assert.equal(failed.status,'failed');assert.equal(bad.calls.length,0);
});
test('native workers forward canonical custom Codex agent roles and explicit reductions', async () => {
  const f=await fixture();const starts=[];
  f.client.run=async options=>{starts.push(options);return {output:'ok',model:'role-model',effort:'high',roleDigest:'role-v1'};};
  const nativeWorkspace={prepare:async()=>({workspaceId:null,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:1,roleDigest:'role-v1'}),release:async()=>{}};
  const run=await runtime.runWorkflow({...f,authorityRef:'auth',authorityDigest:'digest',nativeWorkspace,source:source('return await pipeline([false,true],write=>agent("inspect",{agentType:"  security-reviewer  ",model:"gpt-5.6-sol",effort:"low",write}));')});
  assert.equal(run.status,'completed');
  assert.deepEqual(starts.map(call=>[call.agentType,call.readOnly]),[['security-reviewer',true],['security-reviewer',false]]);
  assert.ok(starts.every(call=>call.modelExplicit===true&&call.effortExplicit===true));
  assert.deepEqual(run.workers.map(worker=>worker.agentType),['security-reviewer','security-reviewer']);
  assert.ok(run.workers.every(worker=>worker.model==='role-model'&&worker.effort==='high'&&worker.roleDigest==='role-v1'));
  const invalid=await runtime.runWorkflow({...await fixture(),authorityRef:'auth',authorityDigest:'digest',nativeWorkspace,source:source('return agent("x",{agentType:"   "});')});
  assert.equal(invalid.status,'failed');assert.match(invalid.error,/Invalid agent type/);
});
test('edited native role definitions invalidate replay before the cached result is returned',async()=>{
  const f=await fixture();let edited=false;
  const nativeWorkspace={prepare:async({agentType})=>({workspaceId:null,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:1,roleDigest:`${agentType}-${edited&&agentType==='changed'?'v2':'v1'}`}),release:async()=>{}};
  const options={...f,nativeWorkspace,authorityRef:'auth',authorityDigest:'digest',source:source('const out=[]; for (const role of ["unchanged","changed","unchanged"]) out.push(await agent(role,{agentType:role})); return out;')};
  const first=await runtime.runWorkflow(options);assert.equal(first.status,'completed');
  const replay=await runtime.runWorkflow({...options,runId:first.id,resume:true});
  assert.ok(replay.workers.every(worker=>worker.cached));assert.equal(f.calls.length,3);
  edited=true;
  const changed=await runtime.runWorkflow({...options,runId:first.id,resume:true});
  assert.equal(changed.status,'completed');assert.equal(changed.workers[0].cached,true);
  assert.equal(changed.workers[1].cached,undefined);assert.equal(changed.workers[2].cached,undefined);
  assert.equal(changed.workers[1].roleDigest,'changed-v2');
  assert.deepEqual(f.calls,['unchanged','changed','unchanged','changed','unchanged']);
});
test('provably contradictory schemas fail before worker creation', async () => {
  for(const schema of [false,{type:'object',required:['missing'],properties:{},additionalProperties:false},{type:'object',required:['child'],properties:{child:{type:'object',required:['missing'],properties:{},additionalProperties:false}}}]) {
    const f=await fixture();
    const run=await runtime.runWorkflow({...f,source:source(`return await agent("x",{schema:${JSON.stringify(schema)}});`)});
    assert.equal(run.status,'failed');assert.equal(run.workers.length,0);assert.equal(f.calls.length,0);
  }
  const valid=await fixture();
  const run=await runtime.runWorkflow({...valid,source:source('return await agent("x",{schema:{required:["missing"],additionalProperties:false}});')});
  assert.equal(run.status,'completed');assert.equal(valid.calls.length,1);
  const unicode=await fixture();
  unicode.client.run=async options=>{unicode.calls.push(options.prompt);return {output:{a:1}};};
  const unicodeRun=await runtime.runWorkflow({...unicode,source:source('return await agent("x",{schema:{type:"object",required:["a"],additionalProperties:false,patternProperties:{"\\\\u{61}":{}}}});')});
  assert.equal(unicodeRun.status,'completed');assert.equal(unicode.calls.length,1);
});
test('structured output retries on same thread and preserves validation evidence', async () => {
  const f=await fixture();const calls=[];
  f.client.run=async options=>{calls.push(options);return {output:calls.length===1?{}:{count:2},threadId:'repair-thread'};};
  const run=await runtime.runWorkflow({...f,source:source('return await agent("x",{schema:{type:"object",required:["count"]}});')});
  assert.deepEqual(run.result,{count:2});assert.equal(calls.length,2);assert.equal(calls[1].threadId,'repair-thread');
  assert.equal(run.workers.length,1);assert.equal(run.workers[0].validationAttempts.length,1);
});
test('structured output retry bound throws last validation error; API failures remain null', async () => {
  const previous=process.env.MAX_STRUCTURED_OUTPUT_RETRIES;process.env.MAX_STRUCTURED_OUTPUT_RETRIES='2';
  try {
    const f=await fixture();let attempts=0;f.client.run=async()=>{attempts++;return {output:{attempts},threadId:'same'};};
    const failed=await runtime.runWorkflow({...f,source:source('return await agent("x",{schema:{type:"object",required:["count"]}});')});
    assert.equal(failed.status,'failed');assert.match(failed.error,/required property 'count'/);assert.equal(attempts,2);assert.equal(failed.workers[0].validationAttempts.length,2);
    const api=await fixture();api.client.run=async()=>{throw new Error('provider unavailable');};
    const apiRun=await runtime.runWorkflow({...api,source:source('return await agent("x",{schema:{type:"object"}});')});
    assert.equal(apiRun.result,null);assert.equal(apiRun.workers[0].status,'failed');
  } finally {if(previous===undefined)delete process.env.MAX_STRUCTURED_OUTPUT_RETRIES;else process.env.MAX_STRUCTURED_OUTPUT_RETRIES=previous;}
});
test('structured output defaults to 5 attempts and cancellation stops repairs', async () => {
  const previous=process.env.MAX_STRUCTURED_OUTPUT_RETRIES;delete process.env.MAX_STRUCTURED_OUTPUT_RETRIES;
  try {
    const exhausted=await fixture();let attempts=0;exhausted.client.run=async()=>{attempts++;return {output:{},threadId:'same'};};
    const failed=await runtime.runWorkflow({...exhausted,source:source('return await agent("x",{schema:{type:"object",required:["count"]}});')});
    assert.equal(failed.status,'failed');assert.equal(attempts,5);
    const cancelled=await fixture();let repairAttempts=0;let requested=false;
    cancelled.client.run=async({signal})=>{
      repairAttempts++;
      if(repairAttempts===1)return {output:{},threadId:'same'};
      await new Promise((_,reject)=>signal.addEventListener('abort',()=>reject(new Error('interrupted')),{once:true}));
    };
    const stopped=await runtime.runWorkflow({...cancelled,source:source('return await agent("x",{schema:{type:"object",required:["count"]}});'),onUpdate:state=>{
      if(!requested&&state.workers[0]?.validationAttempts?.length===1){requested=true;store.requestControl(cancelled.stateDir,state.id,{type:'stop',workerId:'worker-1'});}
    }});
    assert.equal(stopped.workers[0].status,'failed');assert.equal(repairAttempts,2);
  } finally {if(previous===undefined)delete process.env.MAX_STRUCTURED_OUTPUT_RETRIES;else process.env.MAX_STRUCTURED_OUTPUT_RETRIES=previous;}
});

test('configuration errors abort orchestration instead of returning an API-failure null',async()=>{
  const f=await fixture();
  f.client.run=async()=>{throw Object.assign(new Error('Unavailable Codex model: missing'),{code:'INVALID_WORKER_CONFIGURATION'});};
  const run=await runtime.runWorkflow({...f,source:source('await agent("first"); return await agent("must not launch");')});
  assert.equal(run.status,'failed');assert.match(run.error,/Unavailable Codex model/);
  assert.equal(run.workers.length,1);assert.equal(run.workers[0].status,'failed');
  assert.equal(run.workers[0].launchIntent,false);
});
test('unresolved repair launch remains blocked from resume', async () => {
  const f=await fixture();let attempts=0;
  f.client.run=async options=>{
    attempts++;
    if(attempts===1){options.onUpdate({threadId:'repair-thread',turnId:'turn-1'});return {output:{},threadId:'repair-thread',turnId:'turn-1'};}
    throw new Error('turn/start outcome unresolved: timed out');
  };
  const first=await runtime.runWorkflow({...f,source:source('return await agent("x",{schema:{type:"object",required:["count"]}});')});
  assert.equal(first.status,'interrupted');assert.equal(first.workers[0].launchIntent,true);assert.equal(first.workers[0].turnId,undefined);
  await assert.rejects(()=>runtime.runWorkflow({...f,runId:first.id,resume:true}),/unresolved worker launch intent/i);
});
test('legacy completed and interrupted worker signatures remain resumable', async () => {
  const completedFixture=await fixture();const code=source('return await agent("same");');
  const completed=await runtime.runWorkflow({...completedFixture,source:code});
  completed.workers[0].signature=store.digest({prompt:'same',options:{},model:completedFixture.model,effort:completedFixture.effort,permission:completed.permission});
  store.writeRun(completedFixture.stateDir,completed);
  const replayed=await runtime.runWorkflow({...completedFixture,runId:completed.id,resume:true});
  assert.equal(replayed.workers[0].cached,true);assert.deepEqual(completedFixture.calls,['same']);

  const interruptedFixture=await fixture();
  execFileSync('git',['init'],{cwd:interruptedFixture.cwd});execFileSync('git',['config','user.name','Test'],{cwd:interruptedFixture.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:interruptedFixture.cwd});
  await writeFile(path.join(interruptedFixture.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:interruptedFixture.cwd});execFileSync('git',['commit','-m','base'],{cwd:interruptedFixture.cwd});
  let fail=true;const cwds=[];
  interruptedFixture.client.run=async options=>{cwds.push(options.cwd);options.onUpdate({threadId:'legacy-thread',turnId:'turn'});if(fail){fail=false;throw new Error('connection closed');}return {output:'ok'};};
  const editCode=source('return await agent("edit",{write:true});');
  const interrupted=await runtime.runWorkflow({...interruptedFixture,source:editCode,permission:{sandbox:'workspace-write',approvalPolicy:'never'}});
  interrupted.workers[0].signature=store.digest({prompt:'edit',options:{write:true},model:interruptedFixture.model,effort:interruptedFixture.effort,permission:interrupted.permission});
  store.writeRun(interruptedFixture.stateDir,interrupted);
  const resumed=await runtime.runWorkflow({...interruptedFixture,runId:interrupted.id,resume:true});
  assert.equal(resumed.workers[0].worktree,interrupted.workers[0].worktree);assert.deepEqual(cwds,[interrupted.workers[0].worktree,interrupted.workers[0].worktree]);
});
test('mixed pause replay reuses completed prefix and preserves three-worker order', async () => {
  const f = await fixture();
  let pauseRequested = false;
  let interrupted = false;
  const mixedStates = [];
  f.client.run = async ({prompt, signal, onUpdate}) => {
    f.calls.push(prompt);
    onUpdate({threadId:`thread-${f.calls.length}`,turnId:'turn-1',status:'inProgress'});
    if (prompt === 'active' && !interrupted) {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('Mixed-state pause was not received')),10_000);
        const abort = () => { clearTimeout(timer); interrupted = true; reject(new Error('interrupted')); };
        if (signal.aborted) abort();
        else signal.addEventListener('abort', abort, {once:true});
      });
    }
    return {output:prompt};
  };
  const paused = await runtime.runWorkflow({
    ...f,
    concurrency:1,
    source:source('return await pipeline(["completed","active","queued"],x=>agent(x));'),
    onUpdate:state => {
      const statuses = state.workers.map(worker => worker.status);
      if (!pauseRequested && statuses.join(',') === 'completed,running,queued' && state.workers[1].turnId) {
        pauseRequested = true;
        mixedStates.push(statuses);
        store.requestControl(f.stateDir,state.id,{type:'pause'});
      }
    },
  });
  assert.deepEqual(mixedStates,[['completed','running','queued']]);
  assert.equal(paused.status,'paused');
  assert.equal(paused.launchCount,2);
  assert.deepEqual(f.calls,['completed','active']);
  assert.ok(paused.workers.every(worker => !['running','queued'].includes(worker.status)));
  const completed = paused.workers[0];
  const resumed = await runtime.runWorkflow({...f,runId:paused.id,resume:true});
  assert.equal(resumed.status,'completed');
  assert.equal(resumed.launchCount,4);
  assert.deepEqual(f.calls,['completed','active','active','queued']);
  assert.deepEqual(resumed.result,['completed','active','queued']);
  assert.deepEqual(resumed.workers.map(worker => worker.prompt),['completed','active','queued']);
  assert.deepEqual(resumed.workers[0],{...completed,cached:true});
});

test('pause confirms worker termination; concurrent resume cannot duplicate work', async () => {
  const f = await fixture();
  let id;
  const running = runtime.runWorkflow({...f,source:source('return await pipeline(["a","b","c"],x=>agent(x));'),concurrency:1,onUpdate:state=>{id=state.id;}});
  while (!id) await new Promise(r=>setTimeout(r,5));
  await assert.rejects(()=>runtime.runWorkflow({...f,runId:id,resume:true}),/owned|running|lock/i);
  store.requestControl(f.stateDir,id,{type:'pause'});
  const paused = await running;
  assert.equal(paused.status,'paused');
  assert.ok(paused.workers.every(w=>w.status !== 'running'));
  const resumed = await runtime.runWorkflow({...f,runId:id,resume:true});
  assert.equal(resumed.status,'completed');
});
test('resume rejects a wider permission ceiling', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  await assert.rejects(()=>runtime.runWorkflow({...f,runId:first.id,resume:true,permission:{sandbox:'workspace-write',approvalPolicy:'never'}}),/permission|widen/i);
});
test('explicit null args replace persisted args', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return args;'),args:{old:true}});
  const second=await runtime.runWorkflow({...f,runId:first.id,resume:true,args:null});
  assert.equal(second.result,null);
});
test('resume reuses interrupted thread only for the same call signature', async () => {
  const f=await fixture();
  let first=true, resumedThread, changedThread='unset';
  f.client.run=async options=>{
    if(first){first=false;options.onUpdate({threadId:'thread-recover',turnId:'turn-old'});throw new Error('connection closed');}
    if(options.prompt==='same')resumedThread=options.threadId;
    else changedThread=options.threadId;
    return {output:options.prompt,threadId:options.threadId??'thread-new',turnId:'turn-new',model:f.model,modelProvider:'openai'};
  };
  const firstRun=await runtime.runWorkflow({...f,source:source('return await agent(args);'),args:'same'});
  assert.equal(firstRun.status,'interrupted');
  const resumed=await runtime.runWorkflow({...f,runId:firstRun.id,resume:true});
  assert.equal(resumedThread,'thread-recover');
  await runtime.runWorkflow({...f,runId:firstRun.id,resume:true,args:'changed'});
  assert.equal(changedThread,undefined);
});
test('editing resume reuses worktree for same signature and isolates changed call', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'base.txt'),'base\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  let fail=true;const workerCwds=[];
  f.client.run=async options=>{workerCwds.push(options.cwd);options.onUpdate({threadId:'thread-edit',turnId:'turn-edit'});if(fail){fail=false;throw new Error('worker failed');}return {output:'ok'};};
  const code=source('return await agent(args,{write:true});');
  const first=await runtime.runWorkflow({...f,source:code,args:'same',permission:{sandbox:'workspace-write',approvalPolicy:'never'}});
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true});
  assert.equal(resumed.workers[0].worktree,first.workers[0].worktree);
  await runtime.runWorkflow({...f,runId:first.id,resume:true,args:'changed'});
  assert.notEqual(workerCwds[2],workerCwds[0]);
});
test('resume blocks unknown worker launch gap', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  first.workers=[{id:'worker-1',launchIntent:true,status:'running'}];
  store.writeRun(f.stateDir,first);
  await assert.rejects(()=>runtime.runWorkflow({...f,runId:first.id,resume:true}),/unresolved worker launch intent/i);
});
test('explicit restart blocks persisted unresolved termination even with known turn identity', async () => {
  const f=await fixture();
  let starts=0;
  f.client.run=async({onUpdate})=>{
    starts++;
    onUpdate({threadId:'known-thread',turnId:'known-turn',status:'running'});
    throw new Error('worker termination not confirmed');
  };
  const first=await runtime.runWorkflow({...f,source:source('return await agent("uncertain");')});
  assert.equal(first.status,'interrupted');
  assert.equal(first.uncertainWorkers,true);
  assert.equal(first.workers[0].turnId,'known-turn');
  const reopenedClient={...f.client,run:async()=>{starts++;return {output:'duplicate'};}};
  await assert.rejects(
    ()=>runtime.runWorkflow({...f,client:reopenedClient,runId:first.id,resume:true,restart:true}),
    /unresolved worker termination/i,
  );
  assert.equal(starts,1);
});

for(const change of ['source','authority','omitted call'])test(`unresolved recovery survives ${change} changes and can resume the original thread`, async () => {
  const f=await fixture();
  const nativeWorkspace={
    prepare:async()=>({workspaceId:null,cwd:f.cwd,isolated:false,baseCommit:null,authorityGeneration:1,roleDigest:'role'}),
    release:async()=>({eligible:true}),
  };
  let starts=0;
  const originalSource=source('await agent("prefix"); return await agent("uncertain");');
  const options={...f,nativeWorkspace,authorityRef:'auth',authorityDigest:'original',source:originalSource};
  f.client.run=async({prompt,onUpdate})=>{
    starts++;
    if(prompt==='prefix')return {threadId:'prefix-thread',turnId:'prefix-turn',output:'prefix'};
    onUpdate({threadId:'original-thread',turnId:'original-turn',status:'running'});
    throw new Error('worker termination not confirmed');
  };
  const first=await runtime.runWorkflow(options);
  assert.equal(first.status,'interrupted');
  const resumedThreads=[];
  const client={...f.client,run:async({threadId})=>{starts++;resumedThreads.push(threadId);return {threadId,turnId:'recovered-turn',output:'recovered'};}};
  const changed=change==='authority'?{authorityDigest:'changed'}:{source:source(change==='source'?'return await agent("replacement");':'return "skipped";')};
  await runtime.runWorkflow({...options,client,runId:first.id,resume:true,...changed}).catch(error=>{
    assert.match(error.message,/unresolved worker termination/i);
  });
  assert.equal(starts,2,'changed recovery must not launch a replacement for an unresolved worker');
  const retained=store.readRun(f.stateDir,first.id);
  assert.equal(retained.status,'interrupted');
  assert.equal(retained.uncertainWorkers,true);
  assert.equal(retained.workers.find(worker=>worker.id==='worker-2').threadId,'original-thread');
  assert.equal(retained.workers.find(worker=>worker.id==='worker-2').turnId,'original-turn');
  const recovered=await runtime.runWorkflow({...options,client,runId:first.id,resume:true});
  assert.equal(recovered.status,'completed');
  assert.equal(recovered.uncertainWorkers,false);
  assert.deepEqual(resumedThreads,['original-thread']);
  assert.equal(starts,3);
});

test('unresolved identities survive the persisted checkpoint before recovery replays workers', async () => {
  const f=await fixture();
  const originalSource=source('await agent("prefix"); return await agent("uncertain");');
  f.client.run=async({prompt,onUpdate})=>{
    if(prompt==='prefix')return {threadId:'prefix-thread',turnId:'prefix-turn',output:'prefix'};
    onUpdate({threadId:'checkpoint-thread',turnId:'checkpoint-turn',status:'running'});
    throw new Error('worker termination not confirmed');
  };
  const first=await runtime.runWorkflow({...f,source:originalSource});
  let checkpoint;
  const threads=[];
  const client={...f.client,run:async({threadId})=>{threads.push(threadId);return {threadId,turnId:'finished-turn',output:'done'};}};
  await runtime.runWorkflow({...f,client,runId:first.id,resume:true,onUpdate:state=>{
    if(!checkpoint&&state.uncertainWorkers&&state.workers.length===0)checkpoint=structuredClone(state);
  }});
  assert.ok(checkpoint);
  store.writeRun(f.stateDir,checkpoint);
  const recovered=await runtime.runWorkflow({...f,client,runId:first.id,resume:true});
  assert.equal(recovered.status,'completed');
  assert.equal(recovered.uncertainWorkers,false);
  assert.deepEqual(threads,['checkpoint-thread','checkpoint-thread']);
});

test('global launch cap persists across attempts', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  first.launchCount=1000;
  store.writeRun(f.stateDir,first);
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true,source:source('return await agent("new");')});
  assert.equal(resumed.launchCount,1000);
  assert.equal(resumed.workers[0].status,'failed');
  assert.match(resumed.workers[0].error,/launch limit/i);
  assert.equal(f.calls.length,0);
});
test('worker stop handles active and queued workers without launching queued work', async () => {
  const f=await fixture();
  let controlled=false;
  const run=await runtime.runWorkflow({...f,concurrency:1,source:source('return await pipeline(["active","queued"],x=>agent(x));'),onUpdate:state=>{
    if(!controlled&&state.workers[0]?.status==='running'&&state.workers[1]?.status==='queued'){
      controlled=true;
      store.requestControl(f.stateDir,state.id,{type:'stop',workerId:'worker-2'});
    }
  }});
  assert.equal(run.workers[1].status,'failed');assert.equal(run.result[1],null);assert.equal(run.status,'completed');
  assert.deepEqual(f.calls,['active']);
  const resumed=await runtime.runWorkflow({...f,runId:run.id,resume:true});
  assert.equal(resumed.workers[0].cached,true);assert.equal(resumed.workers[1].status,'completed');
  assert.deepEqual(f.calls,['active','queued']);
});
test('active worker stop records failed without failing orchestration', async () => {
  const f=await fixture();
  let controlled=false;
  const run=await runtime.runWorkflow({...f,source:source('return await agent("active");'),onUpdate:state=>{
    if(!controlled&&state.workers[0]?.status==='running'){
      controlled=true;
      store.requestControl(f.stateDir,state.id,{type:'stop',workerId:'worker-1'});
    }
  }});
  assert.equal(run.workers[0].status,'failed');assert.equal(run.result,null);assert.equal(run.status,'completed');
});
test('completed and active whole-run restart execute fresh attempts', async () => {
  const completedFixture=await fixture();
  const first=await runtime.runWorkflow({...completedFixture,source:source('return await agent("again");')});
  const restarted=await runtime.runWorkflow({...completedFixture,runId:first.id,resume:true,restart:true});
  assert.deepEqual(completedFixture.calls,['again','again']);
  assert.equal(restarted.attempt,2);
  const activeFixture=await fixture();
  let requested=false;
  const active=await runtime.runWorkflow({...activeFixture,source:source('return await agent("active-restart");'),onUpdate:state=>{
    if(!requested&&state.workers[0]?.status==='running'){requested=true;store.requestControl(activeFixture.stateDir,state.id,{type:'restart'});}
  }});
  assert.deepEqual(activeFixture.calls,['active-restart','active-restart']);
  assert.equal(active.status,'completed');
  assert.equal(active.attempt,2);
});
test('normalizes inProgress protocol status for controls', async () => {
  const f=await fixture();
  f.client.run=async options=>{options.onUpdate({threadId:'thread',turnId:'turn',status:'inProgress'});return {output:'ok'};};
  const seen=[];
  const run=await runtime.runWorkflow({...f,source:source('return await agent("x");'),onUpdate:state=>seen.push(structuredClone(state))});
  assert.ok(seen.some(state=>state.workers[0]?.rawStatus==='inProgress'&&state.workers[0]?.status==='running'));
  assert.equal(run.workers[0].status,'completed');
});
test('invalid output schema fails before worker launch', async () => {
  const f=await fixture();
  const run=await runtime.runWorkflow({...f,source:source('return await agent("x",{schema:{type:"not-a-type"}});')});
  assert.equal(run.status,'failed');
  assert.equal(f.calls.length,0);
});
test('normalizes App Server token usage while preserving raw evidence', async () => {
  const f=await fixture();
  f.client.run=async options=>{
    const usage={total:{totalTokens:9,inputTokens:7,outputTokens:2},last:{totalTokens:9},modelContextWindow:380000};
    options.onUpdate({threadId:'thread-usage',turnId:'turn-usage',usage});
    return {output:'ok',threadId:'thread-usage',turnId:'turn-usage',model:f.model,modelProvider:'openai',usage};
  };
  const run=await runtime.runWorkflow({...f,source:source('return await agent("usage");')});
  assert.deepEqual(run.workers[0].usage,{totalTokens:9,inputTokens:7,outputTokens:2,modelContextWindow:380000});
  assert.equal(run.workers[0].rawUsage.last.totalTokens,9);
});
test('worker restart admission requires a selected running attempt',async()=>{
  const f=await fixture(),id='restart-admission';
  for(const status of ['preparing','queued','completed','failed','stopped','interrupted']){
    store.writeRun(f.stateDir,{id,status:'running',attempt:3,workers:[{id:'worker-1',status,attempt:2}]});
    assert.throws(()=>store.requestControl(f.stateDir,id,{type:'restart',workerId:'worker-1'}),/Only a running worker/);
    assert.equal(store.consumeControl(f.stateDir,id),null);
  }
  store.writeRun(f.stateDir,{id,status:'running',attempt:3,workers:[{id:'worker-1',status:'running',attempt:2}]});
  assert.throws(()=>store.requestControl(f.stateDir,id,{type:'restart',workerId:'missing'}),/Unknown worker/);
  store.requestControl(f.stateDir,id,{type:'restart',workerId:'worker-1',workerAttempt:99,runAttempt:99});
  const {id:commandId,...command}=store.consumeControl(f.stateDir,id);
  assert.ok(commandId);assert.deepEqual(command,{type:'restart',workerId:'worker-1',workerAttempt:2,runAttempt:3});
});

test('repeated worker restarts preserve the call and accumulate snapshots once', {timeout:5000}, async t=>{
  const f=await fixture();let clock=1000,starts=0,prepares=0,releases=0;
  t.mock.method(Date,'now',()=>clock);
  const requests=[],observed=[],restarted=new Set();
  const nativeWorkspace={prepare:async()=>{prepares++;return {workspaceId:'workspace',cwd:f.cwd,isolated:true,authorityGeneration:1,roleDigest:'role'};},release:async()=>{releases++;}};
  const item=id=>({id:String(id),type:'commandExecution'});
  f.client.run=async options=>{
    requests.push(options);const attempt=++starts;
    clock+=10;
    const identity={threadId:`thread-${attempt}`,turnId:`turn-${attempt}`,status:'running'};
    const activity=attempt===1?Array.from({length:128},(_,i)=>item(i)):[item(0)];
    options.onUpdate({...identity,usage:{total:{totalTokens:attempt*10},modelContextWindow:1000},activity});
    const capped=attempt===1?Array.from({length:128},(_,i)=>item(i+2)):activity;
    options.onUpdate({...identity,usage:{total:{totalTokens:attempt*10},modelContextWindow:1000},activity:capped});
    options.onUpdate({...identity,usage:{total:{totalTokens:attempt*10},modelContextWindow:1000},activity:capped});
    if(attempt<3){
      await new Promise(resolve=>options.signal.addEventListener('abort',resolve,{once:true}));
      clock+=20;
      options.onUpdate({...identity,status:'interrupted',usage:{total:{totalTokens:attempt*10+5},modelContextWindow:2000},activity:[item(attempt===1?'final':1),{id:'text',type:'agentMessage'}]});
      throw new Error('Native worker interrupted');
    }
    clock+=20;return {...identity,status:'completed',output:'done',usage:{total:{totalTokens:30},modelContextWindow:3000},activity};
  };
  const run=await runtime.runWorkflow({...f,nativeWorkspace,authorityRef:'auth',authorityDigest:'digest',prefixStaggerMs:0,source:source('return await agent("repeat",{label:"Original",model:"gpt-5.6-luna",effort:"low",isolation:"worktree"});'),onUpdate:state=>{
    const worker=state.workers[0];if(!worker?.startedAt)return;
    observed.push(worker);
    if(worker.status==='running'&&worker.turnId&&worker.attempt<3&&!restarted.has(worker.attempt)){
      restarted.add(worker.attempt);store.requestControl(f.stateDir,state.id,{type:'restart',workerId:worker.id});
    }
  }});
  assert.equal(run.status,'completed',run.error);assert.equal(run.result,'done');
  const worker=run.workers[0];
  assert.deepEqual({attempt:worker.attempt,reason:worker.lastAttemptReason,usage:worker.usage,toolCalls:worker.toolCalls,durationMs:worker.durationMs},{attempt:3,reason:'user-retry',usage:{totalTokens:70,modelContextWindow:3000},toolCalls:134,durationMs:90});
  assert.deepEqual(worker.rawUsage,{total:{totalTokens:30},modelContextWindow:3000});
  assert.equal(worker.durationUpdatedAt,new Date(clock).toISOString());
  assert.equal(new Set(observed.map(worker=>worker.startedAt)).size,1);
  assert.deepEqual(requests.map(({prompt,model,effort,cwd,workspace,threadId})=>({prompt,model,effort,cwd,workspace,threadId})),Array.from({length:3},()=>({prompt:'repeat',model:'gpt-5.6-luna',effort:'low',cwd:f.cwd,workspace:worker.workspace,threadId:undefined})));
  assert.equal(new Set(requests.map(request=>request.signal)).size,3);assert.equal(prepares,1);assert.equal(releases,1);
});

test('schema repair replaces same-thread totals and sums distinct threads',async t=>{
  for(const freshThread of [false,true])await t.test(String(freshThread),async()=>{
    const f=await fixture();let starts=0;
    f.client.run=async options=>{
      const attempt=++starts,threadId=freshThread?`thread-${attempt}`:'thread';
      const result={threadId,turnId:`turn-${attempt}`,status:'completed',output:attempt===1?'invalid':42,usage:{total:{totalTokens:attempt*10}},activity:[{id:'command',type:'commandExecution'}]};
      options.onUpdate(result);return result;
    };
    const run=await runtime.runWorkflow({...f,source:source('return await agent("structured",{schema:{type:"number"}});')});
    assert.equal(run.status,'completed');assert.equal(starts,2);assert.equal(run.workers[0].attempt,1);
    assert.deepEqual(run.workers[0].usage,{totalTokens:freshThread?30:20});assert.equal(run.workers[0].toolCalls,freshThread?2:1);
  });
});

test('recovery resumes the interrupted thread once before a fresh user restart', {timeout:5000}, async()=>{
  const f=await fixture(),threads=[];let starts=0,requested=false;
  f.client.run=async options=>{
    const start=++starts;threads.push(options.threadId);
    const threadId=start<3?'recovered-thread':'fresh-thread';
    const snapshot={threadId,turnId:`turn-${start}`,status:'running',usage:{total:{totalTokens:start*10}},activity:[{id:'command',type:'commandExecution'}]};
    options.onUpdate(snapshot);
    if(start===1)throw new Error('connection closed');
    if(start===2){await new Promise(resolve=>options.signal.addEventListener('abort',resolve,{once:true}));options.onUpdate({...snapshot,status:'interrupted'});throw new Error('Native worker interrupted');}
    return {...snapshot,status:'completed',output:'recovered'};
  };
  const first=await runtime.runWorkflow({...f,source:source('return await agent("same");'),prefixStaggerMs:0});
  assert.equal(first.status,'interrupted');
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true,prefixStaggerMs:0,onUpdate:state=>{
    const worker=state.workers[0];
    if(worker?.status==='running'&&worker.turnId==='turn-2'&&!requested){requested=true;store.requestControl(f.stateDir,state.id,{type:'restart',workerId:worker.id});}
  }});
  assert.equal(resumed.status,'completed',resumed.error);assert.equal(resumed.uncertainWorkers,false);
  assert.deepEqual(threads,[undefined,'recovered-thread',undefined]);
  assert.equal(resumed.workers[0].attempt,2);assert.equal(resumed.workers[0].startedAt,first.workers[0].startedAt);
  assert.deepEqual(resumed.workers[0].usage,{totalTokens:50});assert.equal(resumed.workers[0].toolCalls,2);
});

test('recovery retains unresolved identity through prelaunch failure and early restart', {timeout:5000}, async()=>{
  const f=await fixture();let starts=0,requested=false;
  f.client.run=async options=>{
    starts++;
    if(starts===1){options.onUpdate({threadId:'original',turnId:'original-turn',usage:{total:{totalTokens:10}}});throw new Error('connection closed');}
    assert.equal(options.threadId,'original');
    if(starts===2)throw Object.assign(new Error('role unavailable'),{code:'INVALID_WORKER_CONFIGURATION'});
    await new Promise(resolve=>setTimeout(resolve,60));
    assert.equal(options.signal.aborted,false,'restart must not replace recovery before its identity is acknowledged');
    return {threadId:'original',turnId:'recovered-turn',status:'completed',usage:{total:{totalTokens:20}},output:'done'};
  };
  const first=await runtime.runWorkflow({...f,source:source('return await agent("same");'),prefixStaggerMs:0});
  const failed=await runtime.runWorkflow({...f,runId:first.id,resume:true,prefixStaggerMs:0});
  assert.equal(failed.status,'interrupted');assert.equal(failed.workers[0].threadId,'original');
  assert.deepEqual(failed.workers[0].usage,{totalTokens:10});
  const recovered=await runtime.runWorkflow({...f,runId:first.id,resume:true,prefixStaggerMs:0,onUpdate:state=>{
    const worker=state.workers[0];
    if(worker?.status==='running'&&!requested){requested=true;store.requestControl(f.stateDir,state.id,{type:'restart',workerId:worker.id});}
  }});
  assert.equal(recovered.status,'completed',recovered.error);assert.equal(starts,3);
  assert.equal(recovered.workers[0].attempt,1);assert.deepEqual(recovered.workers[0].usage,{totalTokens:20});
});

test('worker restart consumption ignores commands for stale attempts and terminal targets', {timeout:5000}, async()=>{
  const f=await fixture();let starts=0;
  const controlFile=id=>path.join(store.runDirectory(f.stateDir,id),'control.json');
  f.client.run=async options=>{
    starts++;options.onUpdate({threadId:`thread-${starts}`,turnId:'turn',status:'running'});
    if(options.prompt==='first'){
      store.requestControl(f.stateDir,options.runId,{type:'restart',workerId:options.workerId});
      return {output:'first',status:'completed'};
    }
    await new Promise(resolve=>setTimeout(resolve,50));
    assert.equal(options.signal.aborted,false,'a completed target must not restart its sibling');
    for(const command of [
      {type:'restart',workerId:options.workerId,runAttempt:99,workerAttempt:1},
      {type:'restart',workerId:options.workerId,runAttempt:1,workerAttempt:99},
      {type:'restart',workerId:'missing',runAttempt:1,workerAttempt:1},
    ]){
      await writeFile(controlFile(options.runId),JSON.stringify(command));await new Promise(resolve=>setTimeout(resolve,40));
      assert.equal(options.signal.aborted,false);
    }
    return {output:'second',status:'completed'};
  };
  const run=await runtime.runWorkflow({...f,prefixStaggerMs:0,source:source('await agent("first");return await agent("second");')});
  assert.equal(run.status,'completed');assert.equal(starts,2);assert.ok(run.workers.every(worker=>worker.attempt===1));
});

test('workflow timeout stops active workers before returning', async () => {
  const f=await fixture();
  let started=false,stopped=false;
  f.client.run=async({signal})=>{
    started=true;
    try{await new Promise((_,reject)=>signal.addEventListener('abort',()=>reject(new Error('interrupted')),{once:true}));}
    finally{stopped=true;}
  };
  const run=await runtime.runWorkflow({...f,timeoutMs:500,source:source('return await agent("slow");')});
  assert.equal(started,true);assert.equal(stopped,true);
  assert.equal(run.status,'failed');
  assert.match(run.error,/time budget/i);
  assert.equal(run.workers[0].status,'stopped');
});
test('ownership blocks a live persisted server even without owner lock', async t => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  const child=spawn(process.execPath,['-e','setTimeout(()=>{},10000)']);
  t.after(()=>child.kill());
  first.serverPid=child.pid;
  store.writeRun(f.stateDir,first);
  assert.throws(()=>store.acquireRun(f.stateDir,first.id),/App Server .* remains live/i);
});
test('ownership blocks an unresolved server launch even without owner lock', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return 1;')});
  first.launchIntent={at:new Date().toISOString(),ownerPid:999999};
  first.serverPid=null;
  store.writeRun(f.stateDir,first);
  assert.throws(()=>store.acquireRun(f.stateDir,first.id),/unresolved App Server launch intent/i);
});
test('stale ownership takeover is serialized by recovery mutex', async () => {
  const f=await fixture();
  const id='stale-owner';
  const directory=store.runDirectory(f.stateDir,id);
  await mkdir(directory,{recursive:true});
  await writeFile(path.join(directory,'owner.json'),JSON.stringify({pid:999999,token:'old'}));
  await writeFile(path.join(directory,'owner.json.recovery'),'held');
  assert.throws(()=>store.acquireRun(f.stateDir,id),/recovery already in progress/i);
});
test('remaining land conflicts start a merge worker in the project folder', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    if(String(options.prompt).startsWith('Resolve the remaining merge conflicts')) {
      assert.equal(options.cwd,f.cwd);
      await writeFile(path.join(f.cwd,'value.txt'),'resolved\n');
      return {output:'merged'};
    }
    await writeFile(path.join(options.cwd,'value.txt'),'theirs\n');
    return {output:'ok'};
  };
  await writeFile(path.join(f.cwd,'value.txt'),'mine\n');
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(run.land.status,'merged');
  assert.equal(run.workers.at(-1).label,'Merge');
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'resolved\n');
});
test('failed write workers do not land isolated edits', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    await writeFile(path.join(options.cwd,'value.txt'),'after\n');
    throw new Error('provider unavailable');
  };
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(run.result,null);
  assert.equal(run.land.status,'empty');
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'before\n');
});
test('same-folder write workers skip land because the files are already there', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    await writeFile(path.join(options.cwd,'same.txt'),'here\n');
    return {output:'ok'};
  };
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true,isolation:"none"});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(run.workers[0].worktree,undefined);
  assert.equal(run.land.status,'empty');
  assert.equal(await readFile(path.join(f.cwd,'same.txt'),'utf8'),'here\n');
});
test('runtime auto-merges clean overlaps without a merge worker', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'keep\nshared\nend\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    await writeFile(path.join(options.cwd,'value.txt'),'keep\nshared\nworker\n');
    return {output:'ok'};
  };
  await writeFile(path.join(f.cwd,'value.txt'),'parent\nshared\nend\n');
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(run.land.status,'landed',run.land.error);
  assert.equal(run.workers.some(worker=>worker.label==='Merge'),false);
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'parent\nshared\nworker\n');
});
test('failed merge workers leave the parent conflict file untouched', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    if(String(options.prompt).startsWith('Resolve the remaining merge conflicts'))throw new Error('merge failed');
    await writeFile(path.join(options.cwd,'value.txt'),'theirs\n');
    return {output:'ok'};
  };
  await writeFile(path.join(f.cwd,'value.txt'),'mine\n');
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.equal(run.land.status,'conflicted');
  assert.equal(run.land.merge.status,'failed');
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'mine\n');
});
test('resume after a merge does not launch a second merge worker', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  let merges=0;
  f.client.run=async options=>{
    if(String(options.prompt).startsWith('Resolve the remaining merge conflicts')) {
      merges++;
      await writeFile(path.join(f.cwd,'value.txt'),'resolved\n');
      return {output:'merged'};
    }
    await writeFile(path.join(options.cwd,'value.txt'),'theirs\n');
    return {output:'ok'};
  };
  await writeFile(path.join(f.cwd,'value.txt'),'mine\n');
  const first=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(first.land.status,'merged');
  const resumed=await runtime.runWorkflow({...f,runId:first.id,resume:true});
  assert.equal(resumed.status,'completed',resumed.error);
  assert.equal(merges,1);
  assert.equal(resumed.land.status,'empty');
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'resolved\n');
});
test('completed write workers land isolated edits in the main folder', async () => {
  const f=await fixture();
  execFileSync('git',['init'],{cwd:f.cwd});execFileSync('git',['config','user.name','Test'],{cwd:f.cwd});execFileSync('git',['config','user.email','test@example.invalid'],{cwd:f.cwd});
  await writeFile(path.join(f.cwd,'value.txt'),'before\n');execFileSync('git',['add','.'],{cwd:f.cwd});execFileSync('git',['commit','-m','base'],{cwd:f.cwd});
  f.client.run=async options=>{
    await mkdir(path.join(options.cwd,'src'),{recursive:true});
    await writeFile(path.join(options.cwd,'src','add.js'),'export const add=(a,b)=>a+b;\n');
    await writeFile(path.join(options.cwd,'value.txt'),'after\n');
    return {output:'ok'};
  };
  const run=await runtime.runWorkflow({...f,permission:{sandbox:'workspace-write',approvalPolicy:'never'},source:source('return await agent("edit",{write:true});')});
  assert.equal(run.status,'completed',run.error);
  assert.ok(run.workers[0].worktree);
  assert.notEqual(run.workers[0].worktree,f.cwd);
  assert.equal(run.land.status,'landed',run.land.error);
  assert.equal(await readFile(path.join(f.cwd,'value.txt'),'utf8'),'after\n');
  assert.equal(await readFile(path.join(f.cwd,'src','add.js'),'utf8'),'export const add=(a,b)=>a+b;\n');
});
test('save refuses symlink destinations and traversal', async () => {
  const f=await fixture();
  const first=await runtime.runWorkflow({...f,source:source('return args;'),args:1});
  await assert.rejects(()=>store.saveWorkflow(f.stateDir,first.id,'../escape',{cwd:f.cwd}),/name/i);
  const saved=await store.saveWorkflow(f.stateDir,first.id,'mac-temp-path',{cwd:f.cwd});
  const savedScript=await import('../src/workflow/script.mjs');
  assert.equal(savedScript.parseScript(await readFile(saved,'utf8')).meta.name,'mac-temp-path');
  assert.equal(savedScript.parseScript(await readFile(saved,'utf8')).body,savedScript.parseScript(first.source).body);
  const unsafe=await fixture();
  const unsafeRun=await runtime.runWorkflow({...unsafe,source:source('return 1;')});
  await mkdir(path.join(unsafe.cwd,'.codex'),{recursive:true});
  await symlink(await mkdtemp(path.join(tmpdir(),'ultracode-outside-')),path.join(unsafe.cwd,'.codex','workflows'));
  await assert.rejects(()=>store.saveWorkflow(unsafe.stateDir,unsafeRun.id,'probe',{cwd:unsafe.cwd}),/symlink/i);
});
test('more than 1,000 logs succeed while durable retention is capped', async () => {
  const f=await fixture();
  let updates=0;
  const run=await runtime.runWorkflow({...f,source:source('for(let i=0;i<1001;i++) await log(i); return "done";'),onUpdate:()=>{updates++;}});
  assert.equal(run.status,'completed');
  assert.equal(run.result,'done');
  assert.equal(run.logs.length,1000);
  assert.equal(run.logs.at(-1).value,999);
  assert.ok(updates>1000);
});
const waitUntil=async predicate=>{for(let i=0;i<200;i++){if(predicate())return;await new Promise(resolve=>setTimeout(resolve,5));}throw new Error('condition did not settle');};
