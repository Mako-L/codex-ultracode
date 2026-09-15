import {supportedApprovalPolicy} from './permissions.mjs';
import { EventEmitter } from 'node:events';
import { spawn } from 'node:child_process';
import { realpathSync } from 'node:fs';
import path from 'node:path';
import {resolveConfiguredRole} from './roles.mjs';
import {validApprovalResult} from '../workflow/approvals.mjs';

const MAX_MESSAGE_BYTES = 8 * 1024 * 1024;
const MAX_STDERR_BYTES = 64 * 1024;
const SANDBOX_TYPES = {
  'read-only': 'readOnly',
  'workspace-write': 'workspaceWrite',
  'danger-full-access': 'dangerFullAccess',
};
const AGENT_ROLES = {
  'general-purpose': '',
  Explore: 'Role: Explore. Inspect and report only. Do not modify files or external state.',
  Plan: 'Role: Plan. Produce an implementation plan only. Do not modify files or external state.',
};

function errorMessage(value) {
  if (typeof value === 'string') return value;
  if (value?.message) return value.message;
  try { return JSON.stringify(value); } catch { return String(value); }
}

export class CodexClient extends EventEmitter {
  constructor({ cwd, command = 'codex', args, requestTimeoutMs = 30_000, launchToken, onSpawn } = {}) {
    super();
    if (!cwd) throw new TypeError('cwd is required');
    this.cwd = cwd;
    this.command = command;
    this.args = args ?? ['app-server', '--stdio'];
    this.requestTimeoutMs = requestTimeoutMs;
    this.launchToken = launchToken;
    this.onSpawn = onSpawn;
    this.nextId = 1;
    this.pending = new Map();
    this.turns = new Map();
    this.stderr = '';
    this.started = false;
    this.closed = false;
  }

  start() {
    if (this.closed) return Promise.reject(new Error('Codex client is closed'));
    if (this.started) return Promise.resolve(this);
    return this.starting ??= this.#start();
  }

  async #start() {
    const child = spawn(this.command, this.args, {
      cwd: this.cwd,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
      detached: process.platform !== 'win32',
      env: { ...process.env, ULTRACODE_WORKER_PROCESS: '1', ...(this.launchToken ? { ULTRACODE_OWNER_TOKEN: this.launchToken } : {}) },
    });
    this.child = child;
    this.startedAt = Date.now();
    try { this.onSpawn?.(child.pid); }
    catch (error) {
      this.#kill('SIGKILL');
      throw error;
    }
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', chunk => {
      this.stderr = (this.stderr + chunk).slice(-MAX_STDERR_BYTES);
    });
    child.once('error', error => this.#disconnect(error));
    child.once('exit', (code, signal) => {
      if (!this.closed) this.#disconnect(new Error(`Codex app-server exited (${signal ?? code})${this.stderr ? `: ${this.stderr.trim()}` : ''}`));
    });
    let stdout = Buffer.alloc(0);
    child.stdout.on('data', chunk => {
      stdout = Buffer.concat([stdout, chunk]);
      if (stdout.length > MAX_MESSAGE_BYTES && !stdout.includes(10)) {
        this.#disconnect(new Error('App Server message exceeds 8 MiB'));
        this.#kill('SIGKILL');
        return;
      }
      for (let newline; (newline = stdout.indexOf(10)) !== -1;) {
        const line = stdout.subarray(0, newline);
        stdout = stdout.subarray(newline + 1);
        this.#line(line.toString('utf8'));
      }
    });
    child.stdout.once('close', () => {
      if (!this.closed) this.#disconnect(new Error('Codex app-server disconnected'));
    });
    await this.#request('initialize', {
      clientInfo: { name: 'ultracode', version: '1.0.0' },
      capabilities: { experimentalApi: true },
    });
    this.#send({ method: 'initialized', params: {} });
    this.started = true;
    return this;
  }

  get pid() { return this.child?.pid ?? null; }

  async models() {
    await this.start();
    const models = [];
    let cursor = null;
    do {
      const result = await this.#request('model/list', { cursor });
      if (!Array.isArray(result?.data)) throw new Error('Invalid model/list response');
      models.push(...result.data.filter(model => !model.hidden));
      cursor = result.nextCursor ?? null;
    } while (cursor);
    return models;
  }

  async readThread(threadId) {
    if(typeof threadId!=='string'||!threadId)throw new TypeError('threadId is required');
    await this.start();
    return this.#request('thread/read',{threadId,includeTurns:false});
  }

  async readConfiguration(cwd=this.cwd) {
    await this.start();
    return this.#request('config/read',{cwd,includeLayers:true});
  }

  async resolveRole(name,cwd=this.cwd) {
    const response=await this.readConfiguration(cwd);
    const role=resolveConfiguredRole(response,name==='general-purpose'?'default':name);
    if(!role&&!Object.hasOwn(AGENT_ROLES,name))throw Object.assign(new Error(`Unsupported agent type: ${name}`),{code:'INVALID_WORKER_CONFIGURATION'});
    return role;
  }

  async run({ prompt, model, effort, cwd = this.cwd, sandbox = 'read-only', approvalPolicy = 'never', onApproval, schema, agentType = 'general-purpose', resolvedRole, signal, onUpdate, threadId } = {}) {
    if (typeof prompt !== 'string' || !prompt) throw new TypeError('prompt is required');
    if (signal?.aborted) throw signal.reason ?? new DOMException('Aborted', 'AbortError');
    const role=resolvedRole===undefined?await this.resolveRole(agentType,cwd):resolvedRole;
    if(!role&&!Object.hasOwn(AGENT_ROLES,agentType))throw new Error(`Unsupported agent type: ${agentType}`);
    model=role?.model??model;effort=role?.effort??effort;
    if (agentType === 'Explore' || agentType === 'Plan') sandbox='read-only';
    if (!SANDBOX_TYPES[sandbox]) throw new Error(`Unsupported sandbox: ${sandbox}`);
    if(!supportedApprovalPolicy(approvalPolicy)||approvalPolicy!=='never'&&typeof onApproval!=='function')throw new Error('Approval policy requires a supported approval relay');
    if (signal?.aborted) throw signal.reason ?? new DOMException('Aborted', 'AbortError');
    await this.start();

    const catalog = await this.models();
    const selected = catalog.find(entry => entry.model === model || entry.id === model);
    if (!selected) throw Object.assign(new Error(`Unavailable Codex model: ${model}`),{code:'INVALID_WORKER_CONFIGURATION'});
    const efforts = selected.supportedReasoningEfforts?.map(entry => typeof entry === 'string' ? entry : entry.reasoningEffort ?? entry.effort) ?? [];
    if (!efforts.includes(effort)) throw Object.assign(new Error(`Unsupported effort ${effort} for ${model}`),{code:'INVALID_WORKER_CONFIGURATION'});
    model = selected.model;

    const common = {
      cwd,
      model,
      modelProvider: 'openai',
      approvalPolicy,
      sandbox,
      config: { ...role?.config, model_reasoning_effort: effort, 'features.multi_agent': false, 'features.multi_agent_v2': false },
      developerInstructions: [role?.developerInstructions,AGENT_ROLES[agentType],'Do not launch other model processes, create sub-agents, or delegate work. Complete only this worker turn.'].filter(Boolean).join('\n'),
    };
    const response = threadId
      ? await this.#request('thread/resume', { ...common, threadId })
      : await this.#request('thread/start', {...common,threadSource:'ultracode-worker'});
    const actualThreadId = response?.thread?.id;
    this.#assertEffective(response, { model, effort, sandbox, cwd, approvalPolicy });
    if (!actualThreadId || (threadId && actualThreadId !== threadId)) throw new Error('App Server returned wrong thread');

    const state = {
      threadId: actualThreadId, turnId: undefined, model, modelProvider: 'openai', effort,
      usage: null, activity: [], status: 'starting', text: '', approvalError: null, firstResponseStarted: false,
      onUpdate,onApproval:approvalPolicy==='never'?null:onApproval,
      approvalController:new AbortController(),approvalRequests:new Map(),items:new Map(),
    };
    state.turnReady=new Promise(resolve=>{state.ready=resolve;});
    const completion = new Promise((resolve, reject) => Object.assign(state, { resolve, reject }));
    completion.catch(() => {});
    this.turns.set(actualThreadId, state);
    this.#update(state);

    let aborting = false;
    const abort = async () => {
      state.approvalController.abort(signal?.reason??new Error('Worker stopped'));
      if (aborting) return;
      if (!state.turnId) return;
      aborting = true;
      try {
        const deadline=Date.now()+this.requestTimeoutMs;
        const terminal=()=>['completed','failed','interrupted'].includes(state.status);
        while(!terminal()) {
          try {
            await this.#request('turn/interrupt', { threadId: state.threadId, turnId: state.turnId });
            break;
          }catch(error) {
            if(!/no active turn to interrupt/i.test(errorMessage(error)))throw error;
            // A queued turn can have an ID before it is interruptible. A completed
            // turn can also race the interrupt. Neither case proves termination.
            let current;
            try {current=await this.#request('thread/read',{threadId:state.threadId,includeTurns:true});}
            catch(observationError) {
              // A freshly allocated rollout may not be materialized yet. Keep
              // the cancellation unresolved and retry within the same deadline.
              if(Date.now()>=deadline)throw observationError;
            }
            const turn=current?.thread?.turns?.find(turn=>turn.id===state.turnId);
            if(turn&&['completed','failed','interrupted'].includes(turn.status)) {
              state.status=turn.status;
              state.text=turn.items?.filter(item=>item.type==='agentMessage').map(item=>item.text).join('')||state.text;
              this.#update(state);state.resolve(turn);return;
            }
            if(Date.now()>=deadline)throw new Error('active turn could not be reconciled');
            await new Promise(resolve=>setTimeout(resolve,50));
          }
        }
        let timeout;
        try {
          await Promise.race([completion,new Promise((_,reject)=>{
            timeout=setTimeout(()=>reject(new Error('completion event not confirmed')),Math.max(1,deadline-Date.now()));
          })]);
        }finally{clearTimeout(timeout);}
      } catch (error) {
        state.reject(new Error(`Turn interrupt unresolved: ${errorMessage(error)}`));
        this.turns.delete(state.threadId);
      }
    };
    signal?.addEventListener('abort', abort, { once: true });
    try {
      const started = await this.#request('turn/start', {
        threadId: actualThreadId,
        input: [{ type: 'text', text: prompt }],
        model,
        effort,
        cwd,
        approvalPolicy,
        sandboxPolicy: this.#sandboxPolicy(sandbox, cwd),
        ...(schema === undefined ? {} : { outputSchema: schema }),
      });
      const turnId = started?.turn?.id;
      if (!turnId) throw new Error('Invalid turn/start response');
      if (state.turnId && state.turnId !== turnId) throw new Error('App Server returned wrong turn');
      state.turnId = turnId;
      const notifications=state.pendingNotifications??[];
      delete state.pendingNotifications;
      for(const notification of notifications){
        if((notification.params.turnId??notification.params.turn?.id)===turnId)this.#notification(notification.method,notification.params);
      }
      state.ready();
      if (!['completed', 'failed', 'interrupted'].includes(state.status)) state.status = started.turn.status ?? 'running';
      this.#update(state);
      if (signal?.aborted) await abort();
      const turn = await completion;
      if (state.approvalError) throw state.approvalError;
      if (turn.status !== 'completed') throw new Error(`Turn ${turn.id} ${turn.status}${turn.error ? `: ${errorMessage(turn.error)}` : ''}`);
      const text = state.text || turn.items?.filter(item => item.type === 'agentMessage').map(item => item.text).join('') || '';
      let output = text;
      if (schema !== undefined) {
        try { output = JSON.parse(text); } catch (error) {
          const invalid=new Error(`Invalid structured output: ${error.message}`);
          invalid.code='INVALID_STRUCTURED_OUTPUT';invalid.threadId=state.threadId;
          throw invalid;
        }
      }
      return { output, text, threadId: state.threadId, turnId: state.turnId, model, modelProvider: 'openai', effort, usage: state.usage, activity: state.activity };
    } finally {
      state.approvalController.abort(new Error('Worker turn ended'));
      await Promise.allSettled([...state.approvalRequests.values()]);
      signal?.removeEventListener('abort', abort);
      try {
        // Interrupting a turn can leave its command processes running. Keep
        // this barrier inside run() so a restart cannot overlap the old tools.
        if (signal?.aborted && state.turnId) {
          try { await this.#request('thread/backgroundTerminals/clean', { threadId: actualThreadId }); }
          catch (error) { throw new Error(`Thread terminal cleanup unresolved: ${errorMessage(error)}`); }
        }
      } finally { this.turns.delete(actualThreadId); }
    }
  }

  async close() {
    if (this.closed) return;
    this.closed = true;
    const error = new Error('Codex client closed');
    this.#disconnect(error);
    if (this.child && (process.platform !== 'win32' ? this.#groupAlive() : this.child.exitCode === null && this.child.signalCode === null)) {
      this.#kill('SIGTERM');
      const exited = await this.#waitForExit(Math.min(this.requestTimeoutMs, 1_000));
      if (!exited) {
        this.#kill('SIGKILL');
        const killed = await this.#waitForExit(this.requestTimeoutMs);
        if (!killed) throw new Error('Owned Codex app-server termination unresolved');
      }
    }
  }

  #groupAlive() {
    if(!this.child?.pid)return false;
    if(process.platform==='win32')return this.child.exitCode===null&&this.child.signalCode===null;
    try {process.kill(-this.child.pid,0);return true;} catch(error){return error.code!=='ESRCH';}
  }

  async #waitForExit(timeoutMs) {
    const deadline=Date.now()+timeoutMs;
    while(this.#groupAlive()&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,20));
    return !this.#groupAlive();
  }

  #kill(signal) {
    if (!this.child?.pid) return;
    try {
      if (process.platform === 'win32') this.child.kill(signal);
      else process.kill(-this.child.pid, signal);
    } catch (error) {
      if (error.code !== 'ESRCH' && error.code !== 'EPERM') throw error;
    }
  }

  #sandboxPolicy(sandbox, cwd) {
    if (sandbox === 'read-only') return { type: 'readOnly', networkAccess: false };
    if (sandbox === 'workspace-write') return { type: 'workspaceWrite', writableRoots: [cwd], networkAccess: false };
    if (sandbox === 'danger-full-access') return { type: 'dangerFullAccess' };
    throw new Error(`Unsupported sandbox: ${sandbox}`);
  }

  #assertEffective(response, expected) {
    if (response?.model !== expected.model) throw new Error(`Effective model mismatch: ${response?.model}`);
    if (response?.modelProvider !== 'openai') throw new Error(`Effective provider mismatch: ${response?.modelProvider}`);
    if (response?.approvalPolicy !== expected.approvalPolicy) throw new Error(`Effective approval policy mismatch: ${errorMessage(response?.approvalPolicy)}`);
    if (response?.sandbox?.type !== SANDBOX_TYPES[expected.sandbox]) throw new Error(`Effective sandbox mismatch: ${response?.sandbox?.type}`);
    if (expected.sandbox !== 'danger-full-access' && response.sandbox.networkAccess === true) throw new Error('Effective network access exceeds the worker permission ceiling');
    if (expected.sandbox === 'workspace-write') {
      const roots = response.sandbox.writableRoots;
      const cwd = realpathSync(expected.cwd);
      if (!Array.isArray(roots) || roots.some(root => {
        if (typeof root !== 'string' || !path.isAbsolute(root)) return true;
        let resolved;
        try { resolved = realpathSync(root); } catch { resolved = path.resolve(root); }
        return resolved !== cwd && !resolved.startsWith(`${cwd}${path.sep}`);
      })) throw new Error('Effective writable roots exceed the worker permission ceiling');
    }
    if (response?.cwd !== expected.cwd) throw new Error(`Effective cwd mismatch: ${response?.cwd}`);
    if (response?.reasoningEffort != null && response.reasoningEffort !== expected.effort) throw new Error(`Effective effort mismatch: ${response.reasoningEffort}`);
  }

  #request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        const unresolved = ['thread/start', 'thread/resume', 'turn/start', 'turn/interrupt'].includes(method) ? ' outcome unresolved:' : '';
        reject(new Error(`${method}${unresolved} timed out after ${this.requestTimeoutMs}ms`));
      }, this.requestTimeoutMs);
      this.pending.set(id, { resolve, reject, timer, method });
      try { this.#send({ id, method, params }); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
    });
  }

  #send(message) {
    if (!this.child?.stdin.writable) throw new Error('Codex app-server is not writable');
    const line = JSON.stringify({ jsonrpc: '2.0', ...message });
    if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) throw new Error('App Server request exceeds 8 MiB');
    this.child.stdin.write(`${line}\n`);
  }

  #line(line) {
    if (Buffer.byteLength(line) > MAX_MESSAGE_BYTES) return this.#disconnect(new Error('App Server message exceeds 8 MiB'));
    let message;
    try { message = JSON.parse(line); } catch { return this.#disconnect(new Error('Invalid JSON from Codex app-server')); }
    if ('id' in message && !message.method) {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(`${pending.method} failed: ${errorMessage(message.error)}`));
      else pending.resolve(message.result);
      return;
    }
    if ('id' in message && message.method) return this.#serverRequest(message);
    this.#notification(message.method, message.params ?? {});
  }

  #serverRequest(message) {
    const state=this.turns.get(message.params?.threadId);
    if(!state?.onApproval||!['item/commandExecution/requestApproval','item/fileChange/requestApproval','item/permissions/requestApproval'].includes(message.method))return this.#deny(message);
    if(state.approvalRequests.has(message.id))return; // One responder per server request.
    const operation=this.#relayApproval(message,state).finally(()=>state.approvalRequests.delete(message.id));
    state.approvalRequests.set(message.id,operation);operation.catch(error=>this.emit('callbackError',error));
  }

  async #relayApproval(message,state) {
    const signal=state.approvalController.signal;
    const cancellation=message.method==='item/permissions/requestApproval'?{permissions:{},scope:'turn'}:{decision:'cancel'};
    let abort;
    const cancelled=new Promise((_,reject)=>{abort=()=>reject(signal.reason??new Error('Approval cancelled'));signal.addEventListener('abort',abort,{once:true});if(signal.aborted)abort();});
    let result=cancellation;
    const activity={type:'approval',method:message.method,itemId:message.params.itemId,status:'requested'};
    try {
      await Promise.race([state.turnReady,cancelled]);
      if(this.turns.get(state.threadId)!==state||state.turnId!==message.params.turnId||typeof message.params.itemId!=='string')throw new Error('Stale worker approval request');
      state.activity.push(activity);this.#update(state);
      result=await Promise.race([Promise.resolve().then(()=>state.onApproval({method:message.method,params:message.params,item:state.items.get(message.params.itemId),signal})),cancelled]);
      signal.throwIfAborted();
      if(this.turns.get(state.threadId)!==state||state.turnId!==message.params.turnId||!validApprovalResult(message.method,message.params,result))throw new Error('Invalid or stale approval response');
      activity.status='answered';activity.result=result;
    } catch(error) {
      result=cancellation;activity.status='cancelled';activity.error=error.message;
      if(!signal.aborted)state.approvalError=error;
    } finally {signal.removeEventListener('abort',abort);}
    this.#update(state);
    try {this.#send({id:message.id,result});} catch(error) {if(!this.closed)this.#disconnect(error);}
  }

  #deny(message) {
    const approval = /Approval|approval/.test(message.method);
    const state = this.turns.get(message.params?.threadId ?? message.params?.conversationId);
    const error = new Error(`Denied App Server request: ${message.method}`);
    if (approval && state) {
      state.approvalError = error;
      state.activity.push({ type: 'approval', method: message.method, status: 'denied' });
      this.#update(state);
    }
    let result;
    if (message.method === 'item/commandExecution/requestApproval' || message.method === 'item/fileChange/requestApproval') result = { decision: 'cancel' };
    else if (message.method === 'applyPatchApproval' || message.method === 'execCommandApproval') result = { decision: 'abort' };
    else if (message.method === 'item/permissions/requestApproval') result = {permissions:{},scope:'turn'};
    else if (message.method === 'mcpServer/elicitation/request') result = { action: 'cancel' };
    else if (message.method === 'item/tool/requestUserInput') result = { answers: {} };
    else if (message.method === 'item/tool/call') result = { contentItems: [{ type: 'inputText', text: 'Denied by native workflows' }], success: false };
    else return this.#send({ id: message.id, error: { code: -32000, message: 'Request denied by native workflows' } });
    this.#send({ id: message.id, result });
  }

  #notification(method, params) {
    const state = this.turns.get(params.threadId);
    if (!state) return;
    // Resume may publish usage for a retired turn before the new start reply.
    // Only that reply establishes this run's identity; replay matching early events.
    if(!state.turnId){(state.pendingNotifications??=[]).push({method,params});return;}
    const notificationTurnId = params.turnId ?? params.turn?.id;
    if (notificationTurnId && state.turnId && notificationTurnId !== state.turnId) return;
    if(method==='item/started'&&params.item?.id)state.items.set(params.item.id,params.item);
    const responseStarted=['item/agentMessage/delta','item/reasoning/summaryTextDelta','item/reasoning/textDelta'].includes(method)||(method==='item/started'&&['commandExecution','fileChange','mcpToolCall','dynamicToolCall','webSearch'].includes(params.item?.type));
    if(responseStarted)state.firstResponseStarted=true;
    if (method === 'turn/started') {
      state.turnId ??= params.turn?.id;
      state.status = params.turn?.status ?? 'running';
    } else if (method === 'item/agentMessage/delta') {
      state.turnId ??= params.turnId;
      state.text += params.delta ?? '';
    } else if (['item/reasoning/summaryTextDelta','item/reasoning/textDelta'].includes(method)||(method==='item/started'&&responseStarted)) {
      state.turnId ??= params.turnId;
    } else if (method === 'item/completed') {
      state.turnId ??= params.turnId;
      if (params.item?.type === 'agentMessage') state.text = params.item.text ?? state.text;
      else state.activity.push(params.item);
    } else if (method === 'thread/tokenUsage/updated') {
      state.turnId ??= params.turnId;
      state.usage = params.tokenUsage;
    } else if (method === 'turn/completed') {
      state.turnId ??= params.turn?.id;
      state.status = params.turn?.status;
      this.#update(state);
      state.resolve(params.turn);
      return;
    } else if (method === 'error') {
      state.activity.push({ type: 'error', error: params.error, willRetry: params.willRetry });
    } else return;
    this.#update(state);
  }

  #update(state) {
    const update = {
      threadId: state.threadId, turnId: state.turnId, model: state.model,
      modelProvider: state.modelProvider, effort: state.effort, usage: state.usage,
      activity: [...state.activity], status: state.status, firstResponseStarted: state.firstResponseStarted,
    };
    try { state.onUpdate?.(update); } catch (error) { this.emit('callbackError', error); }
    this.emit('update', update);
  }

  #disconnect(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    for (const state of this.turns.values()) {state.approvalController.abort(error);state.reject(error);}
    this.turns.clear();
  }
}
