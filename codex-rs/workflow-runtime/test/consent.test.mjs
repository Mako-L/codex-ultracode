import test from 'node:test';
import assert from 'node:assert/strict';
import {workflowConsentPresentation} from '../src/workflow/consent.mjs';

const present=(body,options={})=>workflowConsentPresentation(body,{
  meta:{name:'probe',description:'Probe',...(options.phases?{phases:options.phases}:{})},body,
  ...(Object.hasOwn(options,'args')?{args:options.args}:{})
});

test('scanner preserves reference branch order and adjacent grouping',()=>{
  const body=`
    agent('first'); const ignored = "agent('string')";
    agent('second');
    while (ready && count < 3) agent('loop');
    for (let i=0; i<2; i++) { agent('for body'); }
    parallel([() => agent('parallel one'), () => agent('parallel two')]);
    parallel ([() => agent('spaced')]);
  `;
  assert.deepEqual(present(body).phases,[
    {title:'step',prompts:['first','second']},
    {title:'loop ready && count < 3',prompts:['loop']},
    {title:'loop let i=0',prompts:['for body']},
    {title:'parallel × N',prompts:['parallel one','parallel two']},
    {title:'step',prompts:['spaced']},
  ]);
});

test('scanner ignores comments and quoted bodies while extracting expression and template prompts',()=>{
  const body=`
    // agent('comment')
    /* agent('block') */
    const text = \`agent('template body')\`;
    agent(prepare('a,b', {nested:true}), {label:'one'});
    agent(\`hello   \${name} world\`);
    obj.agent('member');
    myagent('ignored');
  `;
  assert.deepEqual(present(body).phases,[{title:'step',prompts:[
    "prepare('a,b', {nested:true})",'hello ${…} world','member',
  ]}]);
});

test('parallel classification outranks loop and differing annotations split groups',()=>{
  const body=`
    while (outer) parallel([() => agent('parallel')]);
    while (one) agent('one');
    while (two) agent('two');
  `;
  assert.deepEqual(present(body).phases,[
    {title:'parallel × N',prompts:['parallel']},
    {title:'loop one',prompts:['one']},
    {title:'loop two',prompts:['two']},
  ]);
});

test('metadata phases override inferred rows by index and retain inferred tail prompts',()=>{
  const result=present("agent('one'); parallel([()=>agent('two')]); agent('three');",{phases:[
    {title:'  Research  ',detail:' inspect   inputs '},'Review',
  ]});
  assert.deepEqual(result.phases,[
    {title:'Research',detail:'inspect inputs',prompts:['one']},
    {title:'Review',prompts:['two']},
    {title:'step',prompts:['three']},
  ]);
  assert.equal(present('return 1;').phases,null);
  assert.deepEqual(present('return 1;',{phases:['Only metadata']}).phases,[{title:'Only metadata',prompts:[]}]);
});

test('args presentation uses compact literal serialization and multiline gutter hint',()=>{
  assert.equal(present('return 1;').args,undefined);
  assert.deepEqual(present('return 1;',{args:'raw\tvalue'}).args,{text:'raw value',needsGutter:false,withheld:false});
  assert.deepEqual(present('return 1;',{args:{b:2,a:'x',skip:undefined,list:[undefined,NaN]}}).args,
    {text:'{"b":2,"a":"x","list":[null,null]}',needsGutter:false,withheld:false});
  assert.deepEqual(present('return 1;',{args:'line one\nline two'}).args,
    {text:'line one\nline two',needsGutter:true,withheld:false});
  assert.deepEqual(present('',{args:'visible\u200binvisible'}).args,
    {text:'visible�invisible',needsGutter:false,withheld:false});
});

test('args presentation withholds unsafe or over-budget values',()=>{
  const marker='(value cannot be shown in full — approval withheld; one-time options only)';
  const withheld={text:marker,needsGutter:false,withheld:true};
  assert.deepEqual(present('',{args:1n}).args,withheld);
  assert.deepEqual(present('',{args:'\u001b[31mred'}).args,withheld);
  assert.deepEqual(present('',{args:'x'.repeat(200001)}).args,withheld);
  let deep={};let cursor=deep;for(let i=0;i<64;i++)cursor=cursor.next={};
  assert.deepEqual(present('',{args:deep}).args,withheld);
  assert.deepEqual(present('',{args:Array(10001).fill(null)}).args,withheld);
});

test('source presentation preserves safe bytes and withholds unsafe or oversized text',()=>{
  const safe="agent('ok');\nreturn 1;";
  assert.deepEqual(present(safe).source,{text:safe,withheld:false,originalLength:safe.length});
  assert.deepEqual(present('a\u200bb').source,{text:'a�b',withheld:false,originalLength:3});
  const unsafe='return 1;\u0000';
  assert.deepEqual(present(unsafe).source,{
    text:'(script of 10 characters cannot be shown in full — approval is unavailable; deny or send feedback)',
    withheld:true,originalLength:10,
  });
  const large='x'.repeat(200001);
  assert.deepEqual(present(large).source,{
    text:'(script of 200,001 characters cannot be shown in full — approval is unavailable; deny or send feedback)',
    withheld:true,originalLength:200001,
  });
});

test('presentation rejects malformed direct inputs',()=>{
  assert.throws(()=>workflowConsentPresentation(null,{meta:{},body:''}),/source must be a string/i);
  assert.throws(()=>workflowConsentPresentation('',{meta:{},body:null}),/body must be a string/i);
});
