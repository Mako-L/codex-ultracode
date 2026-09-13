import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

// Removing the isolated interpreter would expose host capabilities or hang these checks.
const scriptModule = await import('../src/workflow/script.mjs').catch(() => ({}));
test('script interpreter is implemented', () => assert.equal(typeof scriptModule.executeScript, 'function'));
const script = (body) => `export const meta = {name:'audit',description:'Audit'};\n${body}`;
test('structured arguments and asynchronous fan-out converge in input order', async () => {
  const { executeScript } = scriptModule;
  const result = await executeScript(script('return await pipeline(args, n => agent(String(n)));'), {
    args: [3, 1, 2], call: async (type, payload) => type === 'agent' ? Number(payload.prompt) * 2 : null,
  });
  assert.deepEqual(result, [6, 2, 4]);
});
test('pipeline streams each item through every stage', async () => {
  let releaseSlow;
  let sawFastSecond;
  const slow = new Promise(resolve => { releaseSlow = resolve; });
  const fastSecond = new Promise(resolve => { sawFastSecond = resolve; });
  const execution = scriptModule.executeScript(script(`return await pipeline([1,2],
    n => agent('first:' + n),
    n => log('second:' + n).then(() => n)
  );`), {call: async (type, payload) => {
    if (payload.prompt === 'first:1') return slow;
    if (payload.prompt === 'first:2') return 2;
    if (type === 'log' && payload.value === 'second:2') sawFastSecond();
    return null;
  }});
  await Promise.race([
    fastSecond,
    new Promise((_, reject) => setTimeout(() => reject(new Error('fast item did not reach its second stage')), 500)),
  ]).finally(() => releaseSlow(1));
  assert.deepEqual(await execution, [1, 2]);
});
test('pipeline preserves null stage values and validates stages', async () => {
  assert.equal(await scriptModule.executeScript(script(`return (await pipeline([null], value => value, value => value === null ? 'kept' : 'lost'))[0];`)), 'kept');
  await assert.rejects(() => scriptModule.executeScript(script('return pipeline([1]);')), /stage/i);
  await assert.rejects(() => scriptModule.executeScript(script('return pipeline([1], 1);')), /stage/i);
});
test('parallel requires functions', async () => {
  assert.deepEqual(await scriptModule.executeScript(script('return parallel([() => 1, async () => 2]);')), [1, 2]);
  await assert.rejects(() => scriptModule.executeScript(script('return parallel([Promise.resolve(1)]);')), /function/i);
});
test('metadata and syntax validation precede any worker call', async () => {
  let calls = 0;
  for (const source of [script('await agent("x"); import("fs")'), script('await agent("x"); }'), 'export const meta={}; return 1;', `const x=1; export const meta={name:'x',description:'x'}; return x;`]) {
    await assert.rejects(() => scriptModule.executeScript(source, {call: async () => { calls++; }}));
  }
  assert.equal(calls, 0);
});
test('metadata is the first statement but may follow comments', () => {
  assert.equal(scriptModule.parseScript(`// workflow\n/* metadata */\nexport const meta={name:'x',description:'x'}; return 1;`).meta.name, 'x');
  assert.throws(() => scriptModule.parseScript(`const x=1; export const meta={name:'x',description:'x'}; return x;`), /requires export const meta/i);
});
test('metadata name must be a string', () => {
  assert.throws(() => scriptModule.parseScript(`export const meta={name:1,description:'bad'}; return 1;`), /metadata/i);
});
test('metadata accepts bounded unique predeclared phases', () => {
  const parsed=scriptModule.parseScript(`export const meta={name:'phases',description:'Phases',phases:['Research','Review']}; return 1;`);
  assert.deepEqual(parsed.meta.phases,['Research','Review']);
  assert.throws(()=>scriptModule.parseScript(`export const meta={name:'phases',description:'Phases',phases:['Same','Same']}; return 1;`),/phases/i);
});
test('fulfilled scripts still enforce timeout while detached calls drain', async () => {
  let release;
  const pending=new Promise(resolve=>{release=resolve;});
  await assert.rejects(()=>scriptModule.executeScript(script('agent("wait"); return "done";'),{call:()=>pending,timeoutMs:20}),/time budget/i);
  release(null);
});
test('1,000 agents may each emit phase and log events', async () => {
  const calls=[];
  const result=await scriptModule.executeScript(script(`
    const values=[];
    for(let i=0;i<1000;i++) {
      await phase('phase:'+i);
      values.push(await agent(String(i)));
      await log('log:'+i);
    }
    return values;
  `),{cpuMs:100_000,call:async(type,payload)=>{
    calls.push([type,payload]);
    return type==='agent'?Number(payload.prompt):null;
  }});
  assert.deepEqual(result,Array.from({length:1000},(_,i)=>i));
  assert.equal(calls.length,3000);
  for(let i=0;i<1000;i++)assert.deepEqual(calls.slice(i*3,i*3+3),[
    ['phase',{name:`phase:${i}`}],
    ['agent',{prompt:String(i),options:{}}],
    ['log',{value:`log:${i}`}],
  ]);
});
test('host capabilities and nondeterminism are unavailable', async () => {
  const result = await scriptModule.executeScript(script('return [typeof process, typeof require, typeof fetch, typeof Date, typeof Math.random, Function("return typeof process")()];'));
  assert.deepEqual(result, ['undefined', 'undefined', 'undefined', 'function', 'function', 'undefined']);
});
test('explicit dates are deterministic', async () => {
  const result = await scriptModule.executeScript(script(`return [
    new Date(0).toISOString(),
    Date.parse('2000-01-01T00:00:00Z'),
    Date.UTC(2000,0,1),
  ];`));
  assert.deepEqual(result, ['1970-01-01T00:00:00.000Z', 946684800000, 946684800000]);
  const isolation = await scriptModule.executeScript(script(`return [
    Date.prototype.constructor === Date,
    Object.getPrototypeOf(Date) === Function.prototype,
    typeof OriginalDate,
    typeof NativeDate,
  ];`));
  assert.deepEqual(isolation, [true, true, 'undefined', 'undefined']);
});
test('clock and randomness access reject, including constructor recovery', async () => {
  for (const expression of [
    'Date.now()',
    'new Date()',
    'Date()',
    'Math.random()',
    'new Date.prototype.constructor()',
    'Date.prototype.constructor()',
    'new (Object.getPrototypeOf(new Date(0)).constructor)()',
  ]) await assert.rejects(() => scriptModule.executeScript(script(`return ${expression};`)), /deterministic|prohibited/i);
});
test('infinite loops and huge fan-outs terminate', async () => {
  await assert.rejects(() => scriptModule.executeScript(script('while(true) {}'), {cpuMs: 25}), /interrupt|CPU|budget/i);
  await assert.rejects(() => scriptModule.executeScript(script('return await pipeline(Array(4097).fill(1), x => x);')), /4096/);
});
test('failed agent values remain in pipeline results', async () => {
  const result = await scriptModule.executeScript(script('return await pipeline([1,2], x => agent(String(x)));'), {
    call: async (type, p) => p.prompt === '1' ? null : 'ok',
  });
  assert.deepEqual(result, [null, 'ok']);
});
test('workflow heap is not capped below the reference process VM', async () => {
  assert.equal(await scriptModule.executeScript(script('const values=Array(5_000_000).fill(1); return values.length;')),5_000_000);
});
test('reference runtime bounds omit a per-workflow heap and whole-run deadline', async () => {
  const source=await readFile(new URL('../src/workflow/script.mjs',import.meta.url),'utf8');
  assert.doesNotMatch(source,/setMemoryLimit/);
  assert.match(source,/cpuMs\s*=\s*30_000/);
  assert.doesNotMatch(source,/timeoutMs\s*=\s*30\s*\*\s*60_000/);
  assert.doesNotMatch(source,/cpuUsed\s*\+/);
});
