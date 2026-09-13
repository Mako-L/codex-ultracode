import { parse, tokenizer } from 'acorn';
import { newQuickJSWASMModule } from 'quickjs-emscripten';

const MAX_BYTES = 1024 * 1024;

// Discovery reads the leading declaration only. Body syntax errors belong to
// invocation, and neither discovery nor validation evaluates user code.
export function parseWorkflowMetadata(source) {
  if (typeof source !== 'string' || Buffer.byteLength(source) > MAX_BYTES) throw new Error('Script exceeds 1 MiB');
  let depth = 0;
  for (const token of tokenizer(source, {ecmaVersion: 'latest', sourceType: 'module'})) {
    const kind = token.type.label;
    if (['{', '[', '(', '${'].includes(kind)) depth++;
    else if (['}', ']', ')'].includes(kind) && --depth === 0) return parseScript(source.slice(0, token.end)).meta;
    else if (kind === ';' && depth === 0) break;
  }
  throw new Error('Script requires export const meta = {name, description}');
}

function literal(node) {
  if (node.type === 'Literal' && (node.value === null || ['string', 'number', 'boolean'].includes(typeof node.value))) return node.value;
  if (node.type === 'ArrayExpression') return node.elements.map(literal);
  if (node.type === 'ObjectExpression') return Object.fromEntries(node.properties.map(p => {
    if (p.type !== 'Property' || p.computed || p.method || p.kind !== 'init') throw new Error('Metadata must contain literal properties');
    return [p.key.name ?? p.key.value, literal(p.value)];
  }));
  throw new Error('Metadata must be literal data');
}

export function parseScript(source) {
  if (typeof source !== 'string' || Buffer.byteLength(source) > MAX_BYTES) throw new Error('Script exceeds 1 MiB');
  const tree = parse(source, { ecmaVersion: 'latest', sourceType: 'module', allowReturnOutsideFunction: true });
  const exports = tree.body.filter(n => n.type.startsWith('Export'));
  const metadataExport = tree.body[0];
  const declaration = metadataExport?.type === 'ExportNamedDeclaration' ? metadataExport.declaration : undefined;
  if (exports.length !== 1 || declaration?.kind !== 'const' || declaration.declarations.length !== 1 || declaration.declarations[0].id.name !== 'meta') {
    throw new Error('Script requires export const meta = {name, description}');
  }
  const walk = (node) => {
    if (!node || typeof node !== 'object') return;
    if (node.type === 'ImportExpression' || node.type === 'ImportDeclaration') throw new Error('Module imports are prohibited');
    for (const value of Object.values(node)) if (Array.isArray(value)) value.forEach(walk); else if (value && typeof value === 'object') walk(value);
  };
  walk(tree);
  const meta = literal(declaration.declarations[0].init);
  if (!meta || typeof meta.name !== 'string' || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(meta.name) || typeof meta.description !== 'string' || !meta.description.trim() || meta.description.length > 1000) throw new Error('Invalid workflow metadata');
  if(meta.phases!==undefined&&(!Array.isArray(meta.phases)||meta.phases.length>100||new Set(meta.phases).size!==meta.phases.length||meta.phases.some(name=>typeof name!=='string'||!name.trim()||name.length>200)))throw new Error('Invalid workflow phases');
  const body = source.slice(0, metadataExport.start) + source.slice(metadataExport.end);
  return { meta, body };
}

export function renameWorkflowSource(source,name) {
  if(typeof name!=='string'||!/^[a-z0-9][a-z0-9-]{0,63}$/.test(name))throw new Error('Invalid workflow name');
  const {meta}=parseScript(source);if(meta.name===name)return source;
  const tree=parse(source,{ecmaVersion:'latest',sourceType:'module',allowReturnOutsideFunction:true});
  const property=tree.body[0].declaration.declarations[0].init.properties.findLast(item=>(item.key.name??item.key.value)==='name');
  return source.slice(0,property.value.start)+JSON.stringify(name)+source.slice(property.value.end);
}

export async function executeScript(source, {args, call = async () => null, cpuMs = 30_000, timeoutMs, signal} = {}) {
  const {body} = parseScript(source);
  const module = await newQuickJSWASMModule();
  const runtime = module.newRuntime();
  runtime.setMaxStackSize(512 * 1024);
  let segmentStart = performance.now();
  let cpuExceeded = false;
  const started = Date.now();
  runtime.setInterruptHandler(() => {
    if(signal?.aborted || (timeoutMs !== undefined && Date.now() - started > timeoutMs))return true;
    if(performance.now() - segmentStart > cpuMs){cpuExceeded=true;return true;}
    return false;
  });
  const vm = runtime.newContext();
  const pending = new Set();
  const deferred = new Set();
  let alive = true;
  const rejectedPromise = message => {
    const promise = vm.newPromise();
    deferred.add(promise);
    const error = vm.newError(message);
    promise.reject(error);
    error.dispose();
    return promise.handle;
  };
  const bridge = vm.newFunction('__call', (typeHandle, payloadHandle) => {
    const type = vm.getString(typeHandle);
    const json = vm.getString(payloadHandle);
    if (Buffer.byteLength(json) > MAX_BYTES) return rejectedPromise('Call payload exceeds 1 MiB');
    const promise = vm.newPromise();
    deferred.add(promise);
    const operation = Promise.resolve().then(() => call(type, JSON.parse(json))).then(
      value => {
        const data = JSON.stringify({value: value ?? null});
        if (Buffer.byteLength(data) > MAX_BYTES) throw new Error('Worker result exceeds 1 MiB');
        if (alive) { const handle = vm.newString(data); promise.resolve(handle); handle.dispose(); }
      },
      error => { if (alive) { const handle = vm.newError(String(error.message ?? error)); promise.reject(handle); handle.dispose(); } },
    ).catch(error => { if (alive) { const handle = vm.newError(error.message); promise.reject(handle); handle.dispose(); } }).finally(() => pending.delete(operation));
    pending.add(operation);
    return promise.handle;
  });
  vm.setProp(vm.global, '__call', bridge);
  bridge.dispose();
  const argsJSON = JSON.stringify(args);
  const wrapped = `
    const Date = (() => {
      const NativeDate = globalThis.Date;
      function DeterministicDate(...values) {
        if (!new.target || values.length === 0) throw new Error('Nondeterministic Date access is prohibited');
        return Reflect.construct(NativeDate, values, new.target === DeterministicDate ? NativeDate : new.target);
      }
      DeterministicDate.prototype = NativeDate.prototype;
      Object.defineProperty(DeterministicDate.prototype, 'constructor', {value: DeterministicDate, writable: true, configurable: true});
      Object.defineProperties(DeterministicDate, {
        now: {value: () => { throw new Error('Nondeterministic Date access is prohibited'); }},
        parse: {value: NativeDate.parse},
        UTC: {value: NativeDate.UTC},
      });
      globalThis.Date = DeterministicDate;
      return DeterministicDate;
    })();
    Math.random = () => { throw new Error('Nondeterministic random access is prohibited'); };
    Object.freeze(Math);
    const args = ${argsJSON === undefined ? 'undefined' : `JSON.parse(${JSON.stringify(argsJSON)})`};
    const invoke = async (type, value) => JSON.parse(await __call(type, JSON.stringify(value))).value;
    const agent = (prompt, options = {}) => invoke('agent', {prompt, options});
    const phase = name => invoke('phase', {name});
    const log = value => invoke('log', {value});
    const pipeline = (items, ...stages) => {
      if (!Array.isArray(items) || items.length > 4096) throw new Error('Fan-out requires an array of at most 4096 items');
      if (stages.length === 0 || stages.some(stage => typeof stage !== 'function')) throw new Error('Pipeline requires one or more function stages');
      return Promise.all(items.map(item => stages.reduce((value, stage) => value.then(stage), Promise.resolve(item))));
    };
    const parallel = tasks => {
      if (!Array.isArray(tasks) || tasks.some(task => typeof task !== 'function')) throw new Error('Parallel requires an array of functions');
      return pipeline(tasks, task => task());
    };
    (async () => { ${body}\n })()
  `;
  let resultHandle;
  let fulfilledValue;
  try {
    segmentStart = performance.now();
    const evaluated = vm.evalCode(wrapped, 'workflow.js');
    resultHandle = vm.unwrapResult(evaluated);
    while (true) {
      if (signal?.aborted) throw signal.reason ?? new Error('Workflow interrupted');
      if (timeoutMs !== undefined && Date.now() - started > timeoutMs) throw new Error('Workflow time budget exceeded');
      segmentStart = performance.now();
      cpuExceeded = false;
      const jobs = runtime.executePendingJobs();
      if (jobs.error) { const error = vm.dump(jobs.error); jobs.error.dispose(); throw new Error(error.message ?? String(error)); }
      const result = vm.getPromiseState(resultHandle);
      if (result.type === 'fulfilled') {
        const value = vm.dump(result.value);
        result.value.dispose();
        fulfilledValue ??= {value};
        if(!pending.size)return fulfilledValue.value;
      }
      if(fulfilledValue&&!pending.size)return fulfilledValue.value;
      if (result.type === 'rejected') {
        const error = vm.dump(result.error);
        result.error.dispose();
        throw new Error(error.message ?? String(error));
      }
      await new Promise(resolve => setTimeout(resolve, 5));
    }
  } catch(error) {
    if(signal?.aborted)throw signal.reason??error;
    if(timeoutMs !== undefined && Date.now()-started>timeoutMs)throw new Error('Workflow time budget exceeded');
    if(cpuExceeded)throw new Error('Workflow CPU budget exceeded');
    throw error;
  } finally {
    alive = false;
    resultHandle?.dispose();
    for (const promise of deferred) promise.dispose();
    vm.dispose();
    runtime.dispose();
  }
}
