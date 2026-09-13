import fs from 'node:fs';
import {fileURLToPath} from 'node:url';
import {digest,workflowSourceCandidates} from './store.mjs';
import {parseWorkflowMetadata} from './script.mjs';

export function workflowCatalog(cwd,options={}) {
  const found=new Map();
  const candidates=workflowSourceCandidates(cwd,options);
  if(options.webSearchAvailable===true)candidates.push({file:fileURLToPath(new URL('../../bundled/deep-research.js',import.meta.url)),scope:'bundled'});
  for(const candidate of candidates) {
    try {
      const source=fs.readFileSync(candidate.file,'utf8'),meta=parseWorkflowMetadata(source);
      const name=candidate.namespace?`${candidate.namespace}:${meta.name}`:meta.name;
      if(found.has(name))continue;
      const sourceDigest=digest(source),workflowId=digest(`${candidate.file}\0${name}\0${sourceDigest}`);
      found.set(name,{workflowId,name,description:meta.description,scope:candidate.scope,digest:sourceDigest,file:candidate.file});
    }catch{}
  }
  return [...found.values()];
}
