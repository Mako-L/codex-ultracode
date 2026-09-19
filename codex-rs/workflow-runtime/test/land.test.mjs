import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {tmpdir} from 'node:os';
import {execFileSync} from 'node:child_process';
import {landWorktrees,recordResolutions} from '../src/workflow/land.mjs';

function repo() {
  const cwd=fs.mkdtempSync(path.join(tmpdir(),'ultracode-land-'));
  const git=(directory=cwd,...args)=>execFileSync('git',args,{cwd:directory,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
  git(cwd,'init');git(cwd,'config','user.name','Test');git(cwd,'config','user.email','test@example.invalid');
  fs.writeFileSync(path.join(cwd,'value.txt'),'before\n');
  fs.writeFileSync(path.join(cwd,'safe.txt'),'base-safe\n');
  git(cwd,'add','.');git(cwd,'commit','-m','initial');
  const baseCommit=git(cwd,'rev-parse','HEAD');
  return {cwd,git,baseCommit};
}

test('lands isolated worker files into the parent folder',()=>{
  const {cwd,git,baseCommit}=repo();
  const worktree=path.join(cwd,'worker');git(cwd,'worktree','add','--detach',worktree);
  fs.mkdirSync(path.join(worktree,'src'));
  fs.writeFileSync(path.join(worktree,'src','add.js'),'export const add=(a,b)=>a+b;\n');
  fs.writeFileSync(path.join(worktree,'value.txt'),'after\n');
  const result=landWorktrees({cwd,workers:[{id:'worker-1',status:'completed',worktree,baseCommit}]});
  assert.equal(result.status,'landed',result.error);
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'after\n');
  assert.equal(fs.readFileSync(path.join(cwd,'src','add.js'),'utf8'),'export const add=(a,b)=>a+b;\n');
});

test('auto-merges clean overlapping edits into the parent folder',()=>{
  const {cwd,git}=repo();
  fs.writeFileSync(path.join(cwd,'value.txt'),'keep\nshared\nend\n');
  git(cwd,'add','.');git(cwd,'commit','-m','base-lines');
  const head=git(cwd,'rev-parse','HEAD');
  const worktree=path.join(cwd,'worker');git(cwd,'worktree','add','--detach',worktree);
  fs.writeFileSync(path.join(cwd,'value.txt'),'parent\nshared\nend\n');
  fs.writeFileSync(path.join(worktree,'value.txt'),'keep\nshared\nworker\n');
  const result=landWorktrees({cwd,workers:[{id:'worker-1',status:'completed',worktree,baseCommit:head}]});
  assert.equal(result.status,'landed',result.error);
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'parent\nshared\nworker\n');
  assert.equal(result.files[0].merged,true);
});

test('auto-merges two workers that changed different lines',()=>{
  const {cwd,git}=repo();
  fs.writeFileSync(path.join(cwd,'value.txt'),'keep\nshared\nend\n');
  git(cwd,'add','.');git(cwd,'commit','-m','base-lines');
  const head=git(cwd,'rev-parse','HEAD');
  const first=path.join(cwd,'worker-1');git(cwd,'worktree','add','--detach',first);
  const second=path.join(cwd,'worker-2');git(cwd,'worktree','add','--detach',second);
  fs.writeFileSync(path.join(first,'value.txt'),'one\nshared\nend\n');
  fs.writeFileSync(path.join(second,'value.txt'),'keep\nshared\ntwo\n');
  const result=landWorktrees({cwd,workers:[
    {id:'worker-1',status:'completed',worktree:first,baseCommit:head},
    {id:'worker-2',status:'completed',worktree:second,baseCommit:head},
  ]});
  assert.equal(result.status,'landed',result.error);
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'one\nshared\ntwo\n');
});

test('lands unique files from two workers and deletes a removed file',()=>{
  const {cwd,git,baseCommit}=repo();
  const first=path.join(cwd,'worker-1');git(cwd,'worktree','add','--detach',first);
  const second=path.join(cwd,'worker-2');git(cwd,'worktree','add','--detach',second);
  fs.writeFileSync(path.join(first,'left.txt'),'left\n');
  fs.writeFileSync(path.join(second,'right.txt'),'right\n');
  fs.unlinkSync(path.join(first,'value.txt'));
  const result=landWorktrees({cwd,workers:[
    {id:'worker-1',status:'completed',worktree:first,baseCommit},
    {id:'worker-2',status:'completed',worktree:second,baseCommit},
  ]});
  assert.equal(result.status,'landed',result.error);
  assert.equal(fs.existsSync(path.join(cwd,'value.txt')),false);
  assert.equal(fs.readFileSync(path.join(cwd,'left.txt'),'utf8'),'left\n');
  assert.equal(fs.readFileSync(path.join(cwd,'right.txt'),'utf8'),'right\n');
});

test('skips failed workers, same-folder checkouts, and empty runs',()=>{
  const {cwd,git,baseCommit}=repo();
  const worktree=path.join(cwd,'worker');git(cwd,'worktree','add','--detach',worktree);
  fs.writeFileSync(path.join(worktree,'value.txt'),'after\n');
  assert.equal(landWorktrees({cwd,workers:[]}).status,'empty');
  assert.equal(landWorktrees({cwd,workers:[{id:'worker-1',status:'failed',worktree,baseCommit}]}).status,'empty');
  assert.equal(landWorktrees({cwd,workers:[{id:'worker-1',status:'completed',worktree:cwd,baseCommit}]}).status,'empty');
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'before\n');
});

test('writes conflict evidence and leaves the parent file alone',()=>{
  const {cwd,git,baseCommit}=repo();
  const worktree=path.join(cwd,'worker');git(cwd,'worktree','add','--detach',worktree);
  const evidenceDir=path.join(cwd,'evidence');
  fs.writeFileSync(path.join(cwd,'value.txt'),'mine\n');
  fs.writeFileSync(path.join(worktree,'value.txt'),'theirs\n');
  const result=landWorktrees({cwd,workers:[{id:'worker-1',status:'completed',worktree,baseCommit}],evidenceDir});
  assert.equal(result.status,'conflicted',result.error);
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'mine\n');
  assert.equal(fs.readFileSync(path.join(result.conflicts[0].evidence,'parent'),'utf8'),'mine\n');
  assert.equal(fs.readFileSync(path.join(result.conflicts[0].evidence,'theirs-worker-1'),'utf8'),'theirs\n');
  assert.ok(fs.existsSync(path.join(result.conflicts[0].evidence,'attempt')));
});

test('skips already resolved parent files on a later land',()=>{
  const {cwd,git,baseCommit}=repo();
  const worktree=path.join(cwd,'worker');git(cwd,'worktree','add','--detach',worktree);
  fs.writeFileSync(path.join(cwd,'value.txt'),'resolved\n');
  fs.writeFileSync(path.join(worktree,'value.txt'),'theirs\n');
  const resolved=recordResolutions(cwd,[{path:'value.txt'}]);
  const result=landWorktrees({cwd,workers:[{id:'worker-1',status:'completed',worktree,baseCommit}],resolved});
  assert.equal(result.status,'empty');
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'resolved\n');
});

test('keeps parent edits and worker disagreements instead of overwriting',()=>{
  const {cwd,git,baseCommit}=repo();
  const first=path.join(cwd,'worker-1');git(cwd,'worktree','add','--detach',first);
  const second=path.join(cwd,'worker-2');git(cwd,'worktree','add','--detach',second);
  fs.writeFileSync(path.join(cwd,'value.txt'),'mine\n');
  fs.writeFileSync(path.join(first,'value.txt'),'one\n');
  fs.writeFileSync(path.join(second,'value.txt'),'two\n');
  fs.writeFileSync(path.join(first,'safe.txt'),'landed-safe\n');
  const result=landWorktrees({cwd,workers:[
    {id:'worker-1',status:'completed',worktree:first,baseCommit},
    {id:'worker-2',status:'completed',worktree:second,baseCommit},
  ]});
  assert.equal(result.status,'conflicted',result.error);
  assert.equal(fs.readFileSync(path.join(cwd,'value.txt'),'utf8'),'mine\n');
  assert.equal(fs.readFileSync(path.join(cwd,'safe.txt'),'utf8'),'landed-safe\n');
  assert.ok(result.conflicts.some(item=>item.path==='value.txt'));
});
