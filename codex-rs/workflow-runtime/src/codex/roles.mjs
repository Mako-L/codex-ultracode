import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {parse} from 'smol-toml';

const disabledFeatures=new Set(['shell_tool','apps','personality','plugins','memory_tool','request_permissions','request_permissions_tool']);
const metadataKeys=['description','config_file','nickname_candidates'];
const object=value=>value!==null&&typeof value==='object'&&!Array.isArray(value);
const nonempty=(value,label)=>{if(typeof value!=='string'||!value.trim())throw new Error(`${label} must be a nonempty string`);return value.trim();};

function folder(layer){
  const name=layer.name;
  if(name?.type==='project'&&path.isAbsolute(name.dotCodexFolder??''))return name.dotCodexFolder;
  if(['user','system'].includes(name?.type)&&path.isAbsolute(name.file??''))return path.dirname(name.file);
  if(['packagedDefaults','mdm','enterpriseManaged','sessionFlags','legacyManagedConfigTomlFromFile','legacyManagedConfigTomlFromMdm'].includes(name?.type))return null;
  throw new Error(`Unsupported Codex config layer: ${name?.type??'missing type'}`);
}

function files(directory){
  let entries;try{entries=fs.readdirSync(directory,{withFileTypes:true});}catch(error){if(error.code==='ENOENT')return [];throw error;}
  return entries.flatMap(entry=>{const file=path.join(directory,entry.name);return entry.isDirectory()?files(file):entry.isFile()&&entry.name.endsWith('.toml')?[file]:[];}).sort();
}

function roleFile(file,hint){
  const source=fs.readFileSync(file,'utf8'),config=parse(source);
  const name=config.name?.trim()||hint;
  nonempty(name,'Role name');
  if(config.description!==undefined)nonempty(config.description,'Role description');
  if(config.developer_instructions!==undefined||!hint)nonempty(config.developer_instructions,'Role developer_instructions');
  return {name,description:config.description?.trim(),config_file:file,nickname_candidates:config.nickname_candidates,source,config};
}

// config/read returns layers in descending precedence on official Codex 0.153.4.
// Disabled project layers remain in the response and must never be discovered.
export function resolveConfiguredRole(response,name){
  if(!Array.isArray(response?.layers))throw new Error('Codex config/read did not return configuration layers; custom-role compatibility cannot be verified');
  const roles=new Map(),warnings=[];
  for(const layer of [...response.layers].reverse()){
    if(layer.disabledReason)continue;
    const base=folder(layer),local=new Map(),declaredFiles=new Set();
    for(const [declared,value] of Object.entries(layer.config?.agents??{})){
      if(!object(value))continue;
      try{
        let role={name:declared,...Object.fromEntries(metadataKeys.filter(key=>value[key]!=null).map(key=>[key,value[key]]))};
        if(role.config_file){
          if(!path.isAbsolute(role.config_file)){if(!base)throw new Error('Relative role path has no layer base');role.config_file=path.resolve(base,role.config_file);}
          const parsed=roleFile(role.config_file,declared);
          role={...role,...Object.fromEntries(Object.entries(parsed).filter(([,v])=>v!==undefined))};
          declaredFiles.add(role.config_file);
        }
        if(local.has(role.name))throw new Error(`Duplicate role declaration: ${role.name}`);
        local.set(role.name,role);
      }catch(error){warnings.push(error.message);}
    }
    if(base)for(const file of files(path.join(base,'agents'))){
      if(declaredFiles.has(file))continue;
      try{const role=roleFile(file);if(local.has(role.name))throw new Error(`Duplicate role: ${role.name}`);local.set(role.name,role);}
      catch(error){warnings.push(error.message);}
    }
    for(const [key,role] of local){
      const earlier=roles.get(key);
      const merged={...role};
      for(const field of metadataKeys)if(merged[field]==null&&earlier?.[field]!=null)merged[field]=earlier[field];
      if(!role.config_file&&merged.config_file){merged.source=earlier.source;merged.config=earlier.config;}
      try{nonempty(merged.description,'Role description');roles.set(key,merged);}catch(error){warnings.push(error.message);}
    }
  }
  const role=roles.get(name);if(!role)return null;
  const value=role.config??{},config={};
  for(const key of ['model_reasoning_summary','model_verbosity','personality','service_tier'])if(value[key]!==undefined)config[key]=nonempty(value[key],key);
  for(const key of ['model','model_reasoning_effort'])if(value[key]!==undefined)nonempty(value[key],key);
  if(value.features!==undefined){
    if(!object(value.features))throw new Error('Role features must be a table');
    config.features={};
    for(const [key,enabled] of Object.entries(value.features)){
      if(typeof enabled!=='boolean')throw new Error(`Role feature ${key} must be boolean`);
      if(!enabled&&disabledFeatures.has(key))config.features[key]=false;
    }
    if(!Object.keys(config.features).length)delete config.features;
  }
  if(value.skills!==undefined){
    if(!object(value.skills))throw new Error('Role skills must be a table');
    const skills={};
    if(value.skills.config!==undefined){
      if(!Array.isArray(value.skills.config))throw new Error('Role skills.config must be an array');
      skills.config=value.skills.config.flatMap(skill=>{
        if(!object(skill)||typeof skill.enabled!=='boolean'||typeof skill.path!=='string')throw new Error('Invalid role skill configuration');
        return skill.enabled?[]:[{path:path.resolve(path.dirname(role.config_file),skill.path),enabled:false}];
      });
    }
    if(value.skills.bundled?.enabled===false)skills.bundled={enabled:false};
    if(value.skills.include_instructions===false)skills.include_instructions=false;
    if(Object.keys(skills).length)config.skills=skills;
  }
  const resolved={name,description:role.description,model:value.model,effort:value.model_reasoning_effort,developerInstructions:value.developer_instructions,config};
  const digest=createHash('sha256').update(JSON.stringify({resolved,file:role.config_file??null,source:role.source??null})).digest('hex');
  return {...resolved,digest,warnings};
}
