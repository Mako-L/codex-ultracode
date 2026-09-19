import fs from 'node:fs';
import path from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
import {execFileSync,spawnSync} from 'node:child_process';

function gitText(directory,...args) {
  return execFileSync('git',args,{cwd:directory,encoding:'utf8',stdio:['ignore','pipe','pipe']});
}

function gitNames(directory,...args) {
  return gitText(directory,...args).split('\0').filter(Boolean);
}

function assertSafePath(file) {
  if(!file||path.isAbsolute(file)||file.split(/[\\/]/).includes('..'))throw new Error(`Unsafe land path: ${file}`);
}

function samePath(left,right) {
  try{return fs.realpathSync(left)===fs.realpathSync(right);}catch{return path.resolve(left)===path.resolve(right);}
}

function gitCheckout(directory) {
  try{gitText(directory,'rev-parse','--show-toplevel');return true;}catch{return false;}
}

function readFile(root,file) {
  const target=path.join(root,file);
  if(!fs.existsSync(target))return null;
  if(fs.lstatSync(target).isDirectory())throw new Error(`Land path is a directory: ${file}`);
  return fs.readFileSync(target);
}

function sameBuffer(left,right) {
  if(left===null||right===null)return left===right;
  return Buffer.compare(left,right)===0;
}

function fingerprint(content) {
  return content===null?'deleted':createHash('sha256').update(content).digest('hex');
}

function baseContent(cwd,base,file) {
  const shown=spawnSync('git',['show',`${base}:${file}`],{cwd,maxBuffer:32*1024*1024});
  return shown.status===0?shown.stdout:null;
}

function mergeBuffers(ours,base,theirs) {
  const dir=fs.mkdtempSync(path.join(tmpdir(),'ultracode-merge-'));
  try {
    const current=path.join(dir,'ours');
    const ancestor=path.join(dir,'base');
    const other=path.join(dir,'theirs');
    fs.writeFileSync(current,ours??Buffer.alloc(0));
    fs.writeFileSync(ancestor,base??Buffer.alloc(0));
    fs.writeFileSync(other,theirs??Buffer.alloc(0));
    const result=spawnSync('git',['merge-file','-p',current,ancestor,other],{maxBuffer:32*1024*1024});
    return {content:result.stdout,conflicted:result.status!==0};
  } finally {
    fs.rmSync(dir,{recursive:true,force:true});
  }
}

function autoMerge(change) {
  if(change.sides.some(side=>side.content===null)||change.parent===null&&change.sides.length===0)return {conflicted:true};
  if(change.conflict==='parent'&&change.sides.length===1)return mergeBuffers(change.parent,change.base,change.sides[0].content);
  if(change.conflict!=='workers')return {conflicted:true};
  let current=change.sides[0].content;
  for(const side of change.sides.slice(1)) {
    const merged=mergeBuffers(current,change.base,side.content);
    if(merged.conflicted)return merged;
    current=merged.content;
  }
  if(change.parent!==null&&!sameBuffer(change.parent,change.base))return mergeBuffers(change.parent,change.base,current);
  return {content:current,conflicted:false};
}

function writeEvidence(evidenceDir,file,change,attempt) {
  if(!evidenceDir)return null;
  const folder=path.join(evidenceDir,file.split(/[\\/]/).join('__'));
  fs.mkdirSync(folder,{recursive:true,mode:0o700});
  if(change.parent!==null)fs.writeFileSync(path.join(folder,'parent'),change.parent);
  if(change.base!==null)fs.writeFileSync(path.join(folder,'base'),change.base);
  for(const side of change.sides) {
    if(side.content!==null)fs.writeFileSync(path.join(folder,`theirs-${side.workerId}`),side.content);
  }
  if(attempt)fs.writeFileSync(path.join(folder,'attempt'),attempt);
  return folder;
}

export function mergePrompt(land,evidenceDir) {
  const items=land.conflicts.map(item=>`${item.path} (${item.reason})`).join(', ');
  return `Resolve the remaining merge conflicts for this workflow. The project folder is the destination. Conflict sides are under ${evidenceDir}. Write each resolved file into the project folder. Do not leave conflict markers. Conflicts: ${items}`;
}

export function recordResolutions(cwd,conflicts) {
  return (conflicts??[]).flatMap(item=>{
    const content=readFile(cwd,item.path);
    return content===null?[]:[{path:item.path,digest:fingerprint(content)}];
  });
}

export function landWorktrees({cwd,workers,evidenceDir,resolved}) {
  const report={status:'running',files:[],conflicts:[],resolutions:[],workers:[],startedAt:new Date().toISOString()};
  try {
    const editors=(workers??[]).filter(worker=>worker.worktree&&worker.status==='completed'&&fs.existsSync(worker.worktree)&&!samePath(worker.worktree,cwd)&&gitCheckout(worker.worktree));
    if(!editors.length) {
      report.status='empty';
      report.endedAt=new Date().toISOString();
      return report;
    }
    const known=new Set((resolved??[]).map(item=>`${item.path}:${item.digest}`));
    const planned=new Map();
    for(const worker of editors) {
      const base=typeof worker.baseCommit==='string'&&worker.baseCommit?worker.baseCommit:'HEAD';
      const names=[
        ...gitNames(worker.worktree,'diff','-z','--name-only','--no-renames','--diff-filter=ACDMR',base),
        ...gitNames(worker.worktree,'ls-files','-z','--others','--exclude-standard'),
      ];
      for(const file of names) {
        assertSafePath(file);
        const content=readFile(worker.worktree,file);
        const existing=planned.get(file);
        if(existing) {
          existing.sides.push({workerId:worker.id,content});
          if(!sameBuffer(existing.content,content))existing.conflict='workers';
          continue;
        }
        const dest=readFile(cwd,file);
        if(dest!==null&&known.has(`${file}:${fingerprint(dest)}`)) {
          planned.set(file,{workerId:worker.id,content,parent:dest,base:null,sides:[{workerId:worker.id,content}],skip:true,conflict:null});
          continue;
        }
        if(sameBuffer(dest,content)) {
          planned.set(file,{workerId:worker.id,content,parent:dest,base:null,sides:[{workerId:worker.id,content}],skip:true,conflict:null});
          continue;
        }
        const ancestor=baseContent(cwd,base,file);
        planned.set(file,{workerId:worker.id,content,parent:dest,base:ancestor,sides:[{workerId:worker.id,content}],skip:false,conflict:dest!==null&&!sameBuffer(dest,ancestor)?'parent':null});
      }
      report.workers.push(worker.id);
    }
    for(const [file,change] of planned) {
      if(change.skip)continue;
      if(change.conflict) {
        const merged=autoMerge(change);
        if(!merged.conflicted) {
          const dest=path.join(cwd,file);
          fs.mkdirSync(path.dirname(dest),{recursive:true});
          fs.writeFileSync(dest,merged.content);
          report.files.push({path:file,workerId:change.workerId,deleted:false,merged:true});
          report.resolutions.push({path:file,digest:fingerprint(merged.content)});
          continue;
        }
        const evidence=writeEvidence(evidenceDir,file,change,merged.content);
        report.conflicts.push({path:file,workerId:change.workerId,reason:change.conflict,evidence,sides:change.sides.map(side=>side.workerId)});
        continue;
      }
      const dest=path.join(cwd,file);
      if(change.content===null) {
        if(fs.existsSync(dest)&&!fs.lstatSync(dest).isDirectory())fs.unlinkSync(dest);
      } else {
        fs.mkdirSync(path.dirname(dest),{recursive:true});
        fs.writeFileSync(dest,change.content);
      }
      report.files.push({path:file,workerId:change.workerId,deleted:change.content===null});
      report.resolutions.push({path:file,digest:fingerprint(change.content)});
    }
    report.status=report.conflicts.length?'conflicted':report.files.length?'landed':'empty';
  } catch(error) {
    report.status='failed';
    report.error=error.message;
  }
  report.endedAt=new Date().toISOString();
  return report;
}
