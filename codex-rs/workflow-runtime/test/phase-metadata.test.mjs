import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {parseScript} from '../src/workflow/script.mjs';
import {runWorkflow} from '../src/workflow/runtime.mjs';

test('phase objects omit non-string annotations and filter invalid titles', () => {
  const source = phases => `export const meta=${JSON.stringify({name:'phase-proof',description:'Phase validation',phases})}; return 1;`;
  assert.deepEqual(parseScript(source([{title:'Research',detail:42,model:false}])).meta.phases,[{title:'Research'}]);
  for (const phases of [[{}],[null],[{title:42}]]) {
    assert.equal(parseScript(source(phases)).meta.phases,undefined);
  }
});


const phases = [
  {title: 'Research', detail: 'Inspect the requested changes', model: 'gpt-5.6-luna'},
  {title: 'Review', detail: 'Check the resulting changes'},
];
const script = body => `export const meta=${JSON.stringify({name:'phase-proof',description:'Reference phase metadata',phases})}; ${body}`;

test('reference phase objects retain title, detail, and model metadata', () => {
  assert.deepEqual(parseScript(script('return 1;')).meta.phases, phases);
});

test('reference phase objects initialize named groups and phase calls reuse them', async t => {
  const cwd = await mkdtemp(path.join(tmpdir(), 'ultracode-phase-metadata-'));
  t.after(() => rm(cwd, {recursive:true,force:true}));
  const calls = [];
  const client = {
    start: async () => {},
    close: async () => {},
    run: async ({prompt}) => {
      calls.push(prompt);
      return {output:prompt,model:'gpt-5.6-sol',modelProvider:'openai',usage:{totalTokens:1}};
    },
  };
  const run = await runWorkflow({
    cwd,stateDir:path.join(cwd,'.ultracode'),model:'gpt-5.6-sol',effort:'low',client,
    source:script('await phase("Research"); return await agent("inspect");'),
  });
  assert.equal(run.status,'completed',run.error);
  assert.deepEqual(run.phases,[
    {name:'Research',detail:'Inspect the requested changes',model:'gpt-5.6-luna'},
    {name:'Review',detail:'Check the resulting changes'},
  ]);
  assert.deepEqual(calls,['inspect']);
  assert.deepEqual(run.workers.map(worker=>worker.phase),['Research']);
});

test('legacy string phase declarations remain compatible including duplicate names', () => {
  const legacy = `export const meta={name:'legacy',description:'Legacy phases',phases:['Research','Review']}; return 1;`;
  assert.deepEqual(parseScript(legacy).meta.phases,['Research','Review']);
  assert.deepEqual(parseScript(legacy.replace("'Review'","'Research'")).meta.phases,['Research','Research']);
});
