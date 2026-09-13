import fs from 'node:fs';
import path from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { homedir } from 'node:os';
import { execFileSync } from 'node:child_process';
import {renameWorkflowSource} from './script.mjs';

export const digest = value => createHash('sha256').update(typeof value === 'string' ? value : JSON.stringify(value)).digest('hex');
export function runDirectory(stateDir, id) {
  if (!/^[a-zA-Z0-9-]{1,80}$/.test(id)) throw new Error('Invalid run ID');
  return path.join(path.resolve(stateDir), 'runs', id);
}
export function atomicJSON(file, data) {
  fs.mkdirSync(path.dirname(file), {recursive:true,mode:0o700});
  const temp = `${file}.${randomUUID()}.tmp`;
  const fd = fs.openSync(temp,'wx',0o600);
  try { fs.writeFileSync(fd,JSON.stringify(data,null,2)+'\n'); fs.fsyncSync(fd); } finally {fs.closeSync(fd);}
  fs.renameSync(temp,file);
  const directory=fs.openSync(path.dirname(file),'r');
  try {fs.fsyncSync(directory);} finally {fs.closeSync(directory);}
}
export function readRun(stateDir,id) { return JSON.parse(fs.readFileSync(path.join(runDirectory(stateDir,id),'state.json'),'utf8')); }
export function writeRun(stateDir,state) { atomicJSON(path.join(runDirectory(stateDir,state.id),'state.json'),state); }
export function writeRunScript(stateDir,id,source) {
  const directory=runDirectory(stateDir,id);fs.mkdirSync(directory,{recursive:true,mode:0o700});
  const file=path.join(directory,`workflow-${randomUUID()}.js`);
  fs.writeFileSync(file,source,{flag:'wx',mode:0o600});
  return file;
}
export function listRuns(stateDir) {
  const root=path.join(stateDir,'runs');
  if (!fs.existsSync(root)) return [];
  return fs.readdirSync(root).flatMap(id=>{try{return [readRun(stateDir,id)];}catch{return [];}}).sort((a,b)=>b.createdAt.localeCompare(a.createdAt));
}
export function processAlive(pid) {
  if (!Number.isInteger(pid) || pid < 1) return false;
  try { process.kill(pid,0); return true; } catch(error) { return error.code !== 'ESRCH'; }
}
export function processGroupAlive(pgid) {
  if(process.platform==='win32')return processAlive(pgid);
  if(!Number.isInteger(pgid)||pgid<1)return false;
  try {process.kill(-pgid,0);return true;} catch(error){return error.code!=='ESRCH';}
}
export function acquireRun(stateDir,id) {
  const dir=runDirectory(stateDir,id);
  fs.mkdirSync(dir,{recursive:true,mode:0o700});
  const file=path.join(dir,'owner.json');
  const recovery=`${file}.recovery`;
  const token=randomUUID();
  let existing;
  try {existing=readRun(stateDir,id);}catch{}
  if((existing?.serverPgid&&processGroupAlive(existing.serverPgid))||(existing?.serverPid&&processAlive(existing.serverPid)))throw new Error(`Recovery blocked: owned App Server ${existing.serverPid??existing.serverPgid} remains live`);
  if(existing?.launchIntent&&!existing.serverPid)throw new Error('Recovery blocked: unresolved App Server launch intent');
  try {
    const fd=fs.openSync(file,'wx',0o600);
    fs.writeFileSync(fd,JSON.stringify({pid:process.pid,token,createdAt:new Date().toISOString()}));
    fs.fsyncSync(fd); fs.closeSync(fd);
  } catch(error) {
    if(error.code !== 'EEXIST') throw error;
    let recoveryFd;
    try {recoveryFd=fs.openSync(recovery,'wx',0o600);} catch(lockError) {
      if(lockError.code==='EEXIST')throw new Error('Run ownership recovery already in progress');
      throw lockError;
    }
    try {
      let owner;
      try {owner=JSON.parse(fs.readFileSync(file,'utf8'));}catch{throw new Error('Unresolved ownership lock; inspect owner before recovery');}
      if(processAlive(owner.pid))throw new Error(`Run owned by running supervisor ${owner.pid}`);
      let state;
      try {state=readRun(stateDir,id);}catch{}
      if((state?.serverPgid&&processGroupAlive(state.serverPgid))||(state?.serverPid&&processAlive(state.serverPid)))throw new Error(`Recovery blocked: owned App Server ${state.serverPid??state.serverPgid} remains live`);
      if(state?.launchIntent&&!state.serverPid)throw new Error('Recovery blocked: unresolved App Server launch intent');
      const confirm=JSON.parse(fs.readFileSync(file,'utf8'));
      if(confirm.token!==owner.token||confirm.pid!==owner.pid)throw new Error('Run owner changed during recovery');
      fs.renameSync(file,`${file}.abandoned-${token}`);
    } finally {
      fs.closeSync(recoveryFd);
      try{fs.unlinkSync(recovery);}catch{}
    }
    return acquireRun(stateDir,id);
  }
  return () => {
    try {if(JSON.parse(fs.readFileSync(file,'utf8')).token===token) fs.unlinkSync(file);}catch{}
  };
}
export function requestControl(stateDir,id,command) {
  if(!['pause','stop','restart'].includes(command.type)) throw new Error('Invalid control action');
  const state=readRun(stateDir,id);
  if(command.type==='stop'&&state.status==='paused') {
    const release=acquireRun(stateDir,id);
    try {
      const current=readRun(stateDir,id);
      if(current.status!=='paused')throw new Error('Run changed before stop acquired ownership');
      if(command.workerId){
        const worker=current.workers.find(w=>w.id===command.workerId);if(!worker)throw new Error('Unknown worker');
        if(worker.status==='completed')throw new Error('Cannot stop completed worker');
        Object.assign(worker,{status:'failed',output:null,error:'Worker stopped by user'});
      }
      else current.status='stopped';
      current.updatedAt=new Date().toISOString();writeRun(stateDir,current);
    }finally{release();}
    return;
  }
  if(['pause','stop'].includes(command.type)&&state.status!=='running')throw new Error(`Cannot ${command.type} terminal run ${id}`);
  if(command.type==='restart'&&state.status!=='running')throw new Error(`Terminal run ${id} must be restarted by a new supervisor`);
  if(command.type==='restart'&&command.workerId) {
    const worker=state.workers.find(worker=>worker.id===command.workerId);
    if(!worker)throw new Error('Unknown worker');
    if(worker.status!=='running'||worker.restart||worker.stopRequested)throw new Error('Only a running worker can be restarted');
    command={...command,runAttempt:state.attempt??1,workerAttempt:worker.attempt??1};
  }
  atomicJSON(path.join(runDirectory(stateDir,id),'control.json'),{...command,id:randomUUID()});
}
export function consumeControl(stateDir,id) {
  const file=path.join(runDirectory(stateDir,id),'control.json');
  try { const command=JSON.parse(fs.readFileSync(file)); fs.unlinkSync(file); return command; } catch(error) {if(error.code==='ENOENT')return null;throw error;}
}
function lstat(file) {
  try {return fs.lstatSync(file);} catch(error) {if(error.code==='ENOENT')return null;throw error;}
}
function gitRoot(cwd) {
  try {
    return fs.realpathSync(execFileSync('git',['rev-parse','--show-toplevel'],{cwd,encoding:'utf8',stdio:['ignore','pipe','ignore']}).trim());
  } catch {return null;}
}
function projectRoots(cwd) {
  const start=fs.realpathSync(path.resolve(cwd));
  const repository=gitRoot(start);
  const boundary=repository??start;
  const roots=[];
  for(let current=start;;current=path.dirname(current)) {
    roots.push(current);
    if(current===boundary)break;
    const parent=path.dirname(current);
    if(parent===current)break;
  }
  return {roots,boundary};
}
function personalRoot({codexHome,userHome=homedir()}={}) {
  return path.resolve(codexHome??process.env.CODEX_HOME??path.join(userHome,'.codex'));
}
function assertProjectDirectory(directory) {
  const config=path.dirname(directory);
  const configStat=lstat(config);
  if(configStat?.isSymbolicLink())throw new Error('Unsafe symlink save destination');
  const workflowsStat=lstat(directory);
  if(workflowsStat?.isSymbolicLink())throw new Error('Unsafe symlink save destination');
}
export function workflowSaveDirectory(cwd,options={}) {
  if(options.scope==='user') {
    const root=personalRoot(options);
    if(!lstat(root))fs.mkdirSync(root,{recursive:true,mode:0o700});
    const canonicalRoot=fs.realpathSync(root);
    const workflows=path.join(canonicalRoot,'workflows');
    if(!lstat(workflows))fs.mkdirSync(workflows,{mode:0o700});
    return fs.realpathSync(workflows);
  }
  const {roots,boundary}=projectRoots(cwd);
  for(const root of roots) {
    const directory=path.join(root,'.codex','workflows');
    assertProjectDirectory(directory);
    if(lstat(directory)?.isDirectory())return directory;
  }
  return path.join(boundary,'.codex','workflows');
}
export function resolveWorkflowSource(name,cwd,options={}) {
  if(!name)throw new Error('A workflow path or saved name is required');
  const direct=path.resolve(cwd,name);
  if(fs.existsSync(direct)&&fs.statSync(direct).isFile())return direct;
  if(!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))throw new Error('Workflow not found');
  const {roots,boundary}=projectRoots(cwd);
  for(const root of roots) {
    const file=path.join(root,'.codex','workflows',`${name}.js`);
    if(!fs.existsSync(file))continue;
    const resolved=fs.realpathSync(file);
    const relative=path.relative(boundary,resolved);
    if(relative!==''&&!relative.startsWith(`..${path.sep}`)&&relative!=='..'&&!path.isAbsolute(relative))return file;
  }
  const personal=path.join(personalRoot(options),'workflows',`${name}.js`);
  if(fs.existsSync(personal))return personal;
  throw new Error(`Workflow not found: ${name}`);
}

// Discovery reads the files it enumerates, rather than resolving their basenames as
// arbitrary local paths. The order gives the nearest project metadata name precedence.
export function workflowSourceCandidates(cwd,options={}) {
  const {roots,boundary}=projectRoots(cwd),candidates=[];
  const within=(root,file)=>{const relative=path.relative(root,file);return relative===''||(!relative.startsWith(`..${path.sep}`)&&relative!=='..'&&!path.isAbsolute(relative));};
  const collect=(location,scope,root,namespace)=>{
    let files;
    try {const stat=fs.statSync(location);files=stat.isDirectory()?fs.readdirSync(location).sort().map(name=>path.join(location,name)):[location];}catch{return;}
    for(const file of files) {
      if(!file.endsWith('.js'))continue;
      try {const canonical=fs.realpathSync(file);if(!fs.statSync(canonical).isFile()||(root&&!within(root,canonical)))continue;candidates.push({file:canonical,scope,namespace});}catch{}
    }
  };
  for(const root of roots)collect(path.join(root,'.codex','workflows'),'project',boundary);
  collect(path.join(personalRoot(options),'workflows'),'user');
  for(const plugin of options.plugins??[]) {
    if(!plugin||typeof plugin.name!=='string'||!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(plugin.name)||typeof plugin.root!=='string'||!path.isAbsolute(plugin.root))continue;
    let root;try{root=fs.realpathSync(plugin.root);}catch{continue;}
    const extra=Array.isArray(plugin.workflows)?plugin.workflows:plugin.workflows===undefined?[]:[plugin.workflows];
    for(const component of new Set(['workflows',...extra])) {
      if(typeof component!=='string'||path.isAbsolute(component))continue;
      const location=path.resolve(root,component);if(within(root,location))collect(location,'plugin',root,plugin.name);
    }
  }
  return candidates;
}

export async function saveWorkflow(stateDir,id,name,{cwd,scope='project',userHome=homedir(),codexHome}={}) {
  if(typeof name!=='string'||!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))throw new Error('Invalid workflow name');
  if(!['project','user'].includes(scope))throw new Error('Invalid save scope');
  const state=readRun(stateDir,id);
  const saveCwd=cwd??state.cwd;
  const directory=workflowSaveDirectory(saveCwd,{scope,codexHome,userHome});
  if(scope==='project') {
    assertProjectDirectory(directory);
    const config=path.dirname(directory);
    if(!lstat(config))fs.mkdirSync(config,{mode:0o700});
    assertProjectDirectory(directory);
    if(!lstat(directory))fs.mkdirSync(directory,{mode:0o700});
  }
  const file=path.join(directory,`${name}.js`);
  if(lstat(file)?.isSymbolicLink())throw new Error('Unsafe symlink save destination');
  // Exclusive creation protects existing reusable workflows from accidental overwrite.
  fs.writeFileSync(file,renameWorkflowSource(state.source,name),{flag:'wx',mode:0o600});
  return file;
}
