import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {tmpdir} from 'node:os';
import {spawn} from 'node:child_process';
import {createPeer} from '../src/bridge/protocol.mjs';

test('Codex-owned bridge runs without plugins, plugin roots, or commands on PATH', async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(),'codex-workflow-runtime-'));
  const home = path.join(directory,'home');
  const cwd = path.join(directory,'project');
  await fs.mkdir(home);
  await fs.mkdir(cwd);
  const env = {...process.env,CODEX_HOME:home,PATH:path.join(directory,'no-programs')};
  delete env.ULTRACODE_PLUGIN_ROOT;
  delete env.CODEX_PLUGIN_ROOT;
  const child = spawn(process.execPath,[path.resolve(import.meta.dirname,'../bin/workflow.mjs'),
    'bridge','--stdio','--cwd',cwd,'--state-dir',path.join(home,'workflows')],{cwd,env,stdio:'pipe'});
  const closed = new Promise(resolve => child.once('close',(code,signal) => resolve({code,signal})));
  let stderr = '';
  child.stderr.on('data',chunk => { stderr += chunk; });
  const peer = createPeer({input:child.stdout,output:child.stdin,onRequest:async method => {
    throw new Error(`Unexpected worker request: ${method}`);
  }});
  try {
    await peer.request('hello',{protocolVersion:1,models:[],plugins:[]});
    assert.deepEqual(await peer.request('listSavedWorkflows',{cwd}),{workflows:[]});
    const source = "export const meta={name:'native-owned',description:'Native runtime proof'}; return 'native-owned';";
    const validated = await peer.request('validateSource',{source});
    assert.equal(validated.meta.name,'native-owned');
    const launched = await peer.request('runSource',{source,authorityRef:'test-parent',authorityDigest:'test-authority'});
    let state;
    for (let attempt=0;attempt<100;attempt++) {
      state = await peer.request('inspectRun',{runId:launched.runId});
      if (state.status !== 'running') break;
      await new Promise(resolve => setTimeout(resolve,10));
    }
    assert.equal(state.status,'completed');
    assert.equal(state.result,'native-owned');
    assert.deepEqual(state.workers,[]);
    assert.equal((await peer.request('shutdown',{})).stopped,true);
    child.stdin.end();
    assert.deepEqual(await closed,{code:0,signal:null},stderr);
  } finally {
    peer.close();
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await closed;
    await fs.rm(directory,{recursive:true,force:true});
  }
});
