import fs from 'node:fs';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {setTimeout as delay} from 'node:timers/promises';
import {atomicJSON,digest,readRun,runDirectory} from './store.mjs';

const methods=new Set(['item/commandExecution/requestApproval','item/fileChange/requestApproval','item/permissions/requestApproval']);
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const directory=(stateDir,runId)=>path.join(runDirectory(stateDir,runId),'approvals');
const files=(stateDir,runId,id)=>{
  if(!uuid.test(id))throw new Error('Invalid approval ID');
  const base=path.join(directory(stateDir,runId),id);
  return {request:`${base}.request.json`,response:`${base}.response.json`,done:`${base}.done.json`};
};
function active(stateDir,record) {
  const state=readRun(stateDir,record.runId);
  const worker=state.workers?.find(worker=>worker.id===record.workerId);
  if(state.attempt!==record.attempt||state.status!=='running'||worker?.status!=='running'||worker.stopRequested||worker.restart||worker.threadId!==record.params.threadId||worker.turnId!==record.params.turnId)return false;
  try {
    const control=JSON.parse(fs.readFileSync(path.join(runDirectory(stateDir,record.runId),'control.json'),'utf8'));
    if(!control.workerId||control.workerId===record.workerId)return false;
  } catch(error) {if(error.code!=='ENOENT')throw error;}
  return true;
}
function choices(record) {
  if(record.method==='item/permissions/requestApproval')return [
    {permissions:record.params.permissions,scope:'turn'},
    {permissions:record.params.permissions,scope:'session'},
    {permissions:{},scope:'turn'},
  ];
  const offered=record.method==='item/commandExecution/requestApproval'?record.params.availableDecisions:null;
  const decisions=offered??['accept','acceptForSession','decline','cancel'];
  if(!Array.isArray(decisions)||!decisions.length||decisions.length>50)throw new Error('Invalid offered approval decisions');
  return decisions.map(decision=>({decision}));
}
export function approvalForm(record) {
  const options=choices(record);
  const message=`workflow worker permission request\nRun: ${record.runId}\nAttempt: ${record.attempt}\nWorker: ${record.workerId}\n${record.method}\n${JSON.stringify(record.params,null,2)}${record.item?`\nTool details:\n${JSON.stringify(record.item,null,2)}`:''}\n\nChoices:\n${options.map((value,i)=>`decision-${i}: ${JSON.stringify(value)}`).join('\n')}`;
  if(Buffer.byteLength(message)>512*1024)throw new Error('Approval prompt exceeds 512 KiB');
  return {mode:'form',message,requestedSchema:{type:'object',properties:{decision:{type:'string',title:'Permission decision',enum:options.map((_,i)=>`decision-${i}`)}},required:['decision']}};
}
function resultFor(record,response) {
  if(response?.action==='decline'||response?.action==='cancel')return record.method==='item/permissions/requestApproval'?{permissions:{},scope:'turn'}:{decision:response.action};
  const content=response?.content;
  if(response?.action!=='accept'||!content||typeof content!=='object'||Array.isArray(content)||Object.keys(content).length!==1||typeof content.decision!=='string')throw new Error('Invalid approval response');
  const options=choices(record);const index=options.findIndex((_,i)=>`decision-${i}`===content.decision);
  if(index<0)throw new Error('Invalid approval response');
  return options[index];
}
function validResult(record,result) {
  return [...choices(record),resultFor(record,{action:'decline'}),resultFor(record,{action:'cancel'})].some(choice=>digest(choice)===digest(result));
}
export function validApprovalResult(method,params,result) {
  return methods.has(method)&&validResult({method,params},result);
}
export function pendingApprovals(stateDir,runId) {
  let names;try {names=fs.readdirSync(directory(stateDir,runId));} catch(error) {if(error.code==='ENOENT')return [];throw error;}
  return names.filter(name=>name.endsWith('.request.json')).sort().flatMap(name=>{
    const record=JSON.parse(fs.readFileSync(path.join(directory(stateDir,runId),name),'utf8'));
    const file=files(stateDir,runId,record.id);
    if(record.runId!==runId||name!==`${record.id}.request.json`||fs.existsSync(file.done)||fs.existsSync(file.response)||!active(stateDir,record))return [];
    return [record];
  });
}
export function answerApproval(stateDir,runId,record,response) {
  if(record.runId!==runId)return false;
  const file=files(stateDir,runId,record.id);
  const original=JSON.parse(fs.readFileSync(file.request,'utf8'));
  if(digest(original)!==digest(record)||fs.existsSync(file.done)||fs.existsSync(file.response)||!active(stateDir,original))return false;
  const result=resultFor(original,response);
  const value={requestDigest:digest(original),result};
  const temporary=`${file.response}.${randomUUID()}.tmp`;
  atomicJSON(temporary,value);
  try {fs.linkSync(temporary,file.response);} catch(error) {if(error.code==='EEXIST')return false;throw error;}
  finally {fs.unlinkSync(temporary);}
  const fd=fs.openSync(path.dirname(file.response),'r');try {fs.fsyncSync(fd);} finally {fs.closeSync(fd);}
  return true;
}
export async function requestApproval({stateDir,runId,attempt,workerId,method,params,item,signal,pollMs=100}) {
  if(!methods.has(method)||!Number.isInteger(attempt)||!['threadId','turnId','itemId'].every(key=>typeof params?.[key]==='string'&&params[key]))throw new Error('Invalid worker approval request');
  if(method==='item/permissions/requestApproval'&&(!params.permissions||typeof params.permissions!=='object'))throw new Error('Invalid requested permissions');
  signal?.throwIfAborted();
  const record=JSON.parse(JSON.stringify({id:randomUUID(),runId,attempt,workerId,method,params,...(item?{item}:{}),createdAt:new Date().toISOString()}));
  if(!active(stateDir,record))throw new Error('Approval request is no longer active');
  approvalForm(record); // Validate before making the request visible to another process.
  const file=files(stateDir,runId,record.id);atomicJSON(file.request,record);
  let status='cancelled';
  try {
    for(;;){
      signal?.throwIfAborted();
      if(!active(stateDir,record))throw new Error('Approval request is no longer active');
      let response;try {response=JSON.parse(fs.readFileSync(file.response,'utf8'));} catch(error) {if(error.code!=='ENOENT')throw error;}
      if(response){
        if(response.requestDigest!==digest(record)||!validResult(record,response.result))throw new Error('Invalid persisted approval response');
        signal?.throwIfAborted();if(!active(stateDir,record))throw new Error('Approval request is no longer active');
        status='answered';return response.result;
      }
      try {await delay(pollMs,undefined,{signal});} catch(error) {throw signal?.reason??error;}
    }
  } finally {atomicJSON(file.done,{status,at:new Date().toISOString()});}
}
