import test from 'node:test';
import assert from 'node:assert/strict';
import {parseScript,renameWorkflowSource} from '../src/workflow/script.mjs';

const source = meta => `export const meta=${JSON.stringify(meta)}; return 1;`;
test('reference metadata accepts display names independently of safe save filenames', () => {
  for (const name of ['Audit_test','Review sources','../report','X'.repeat(65),' ']) {
    assert.equal(parseScript(source({name,description:'Report'})).meta.name,name);
  }
  assert.throws(() => renameWorkflowSource(source({name:'Audit_test',description:'Report'}),'../report'),/Invalid workflow name/);
  for (const name of ['',null,1]) assert.throws(() => parseScript(source({name,description:'Report'})),/metadata/);
});
test('reference descriptions and phase titles have no extra display-length or uniqueness restrictions', () => {
  const phases=[{title:''},{title:'Repeated'},{title:'Repeated'},{title:'x'.repeat(201)},...Array.from({length:101},(_,i)=>({title:String(i)}))];
  assert.deepEqual(parseScript(source({name:'proof',description:'x'.repeat(1001),phases})).meta.phases,phases);
  assert.equal(parseScript(source({name:'proof',description:' '})).meta.description,' ');
});
test('reference metadata filters malformed optional fields and phase entries', () => {
  const meta={name:'proof',description:'Report',title:12,whenToUse:false,phases:[null,{},7,{title:9},{title:'Keep',detail:3,model:false}],extra:'ignored'};
  assert.deepEqual(parseScript(source(meta)).meta,{name:'proof',description:'Report',phases:[{title:'Keep'}]});
  for(const phases of [false,{},[null,{title:9}],[]]) {
    assert.deepEqual(parseScript(source({name:'proof',description:'Report',title:'',phases})).meta,{name:'proof',description:'Report'});
  }
});
test('reference metadata keeps valid optional text and accepts negative numeric literals', () => {
  assert.deepEqual(parseScript('export const meta={name:"proof",description:"Report",title:"Display",whenToUse:"When requested",unused:-1}; return 1;').meta,
    {name:'proof',description:'Report',title:'Display',whenToUse:'When requested'});
});

test('reference literal metadata accepts static templates and ignores extra literal values', () => {
  assert.deepEqual(parseScript('export const meta={name:`Audit_test`,description:`Line one\\nLine two`,unused:/pattern/,count:1n}; return 1;').meta,
    {name:'Audit_test',description:'Line one\nLine two'});
  assert.throws(() => parseScript('export const meta={name:`audit-${1}`,description:"Report"}; return 1;'),/literal/i);
});
test('reference metadata rejects reserved object keys even when the field would be ignored', () => {
  for (const key of ['__proto__','constructor','prototype']) {
    assert.throws(() => parseScript(`export const meta={name:"proof",description:"Report",extra:{"${key}":1}}; return 1;`),/reserved/i);
  }
});
