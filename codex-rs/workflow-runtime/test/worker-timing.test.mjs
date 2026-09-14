import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {runWorkflow} from '../src/workflow/runtime.mjs';

test('worker timing preserves queue admission and records adapter progress', {timeout:5000}, async t => {
  const cwd=await mkdtemp(path.join(tmpdir(),'native-worker-timing-'));
  t.after(()=>rm(cwd,{recursive:true,force:true}));
  let clock=1_700_000_000_000;
  t.mock.method(Date,'now',()=>clock);
  const queued=Promise.withResolvers();
  let latest;
  const client={
    start:async()=>{},close:async()=>{},
    run:async({prompt,onUpdate})=>{
      if(prompt==='first'){
        await queued.promise;
        const first=latest.workers[0],second=latest.workers[1];
        assert.equal(first.lastProgressAt,clock);
        assert.equal(second.status,'queued');
        assert.equal(second.queuedAt,clock);
        assert.equal(second.lastProgressAt,undefined);
        clock+=31_000;
        onUpdate({threadId:'first-thread',activity:[{id:'progress',type:'command_execution'}]});
        assert.equal(latest.workers[0].lastProgressAt,clock);
        assert.equal(latest.workers[1].queuedAt,clock-31_000);
      }
      return {output:prompt};
    },
  };
  const options={cwd,stateDir:path.join(cwd,'.ultracode'),model:'gpt-5.6-luna',effort:'low',client,
    concurrency:1,prefixStaggerMs:0,
    source:'export const meta={name:"timing",description:"Worker timing"}; return await parallel([()=>agent("first"),()=>agent("second")]);',
    onUpdate:state=>{latest=structuredClone(state);if(state.workers[1]?.status==='queued')queued.resolve();},
  };
  const run=await runWorkflow(options);
  assert.equal(run.status,'completed',run.error);
  assert.deepEqual(run.result,['first','second']);
  assert.deepEqual(run.workers.map(worker=>worker.queuedAt),[clock-31_000,clock-31_000]);
  assert.equal(run.workers[1].lastProgressAt,clock);
  const replay=await runWorkflow({...options,runId:run.id,resume:true});
  assert.ok(replay.workers.every(worker=>worker.cached));
  assert.deepEqual(replay.workers.map(worker=>worker.queuedAt),run.workers.map(worker=>worker.queuedAt));
});
