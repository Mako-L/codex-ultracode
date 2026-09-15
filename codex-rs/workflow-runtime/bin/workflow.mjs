#!/usr/bin/env node
import path from 'node:path';
import {parseArgs} from 'node:util';
import {startBridge} from '../src/bridge/server.mjs';

// Internal Codex child process. Workflow authoring and controls belong to Codex.
try {
  const {values,positionals} = parseArgs({allowPositionals:true,options:{
    stdio:{type:'boolean'},cwd:{type:'string'},'state-dir':{type:'string'},
  }});
  if (positionals.length !== 1 || positionals[0] !== 'bridge' || !values.stdio
    || !path.isAbsolute(values.cwd ?? '') || !path.isAbsolute(values['state-dir'] ?? '')) {
    throw new Error('The native workflow bridge requires --stdio, an absolute --cwd, and an absolute --state-dir');
  }
  startBridge({cwd:values.cwd,stateDir:values['state-dir']});
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
