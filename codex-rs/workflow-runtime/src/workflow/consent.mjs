const PROMPT_LIMIT=100;
const SCAN_LIMIT=5000;
const PRESENTATION_LIMIT=200000;
const ELEMENT_LIMIT=10000;
const DEPTH_LIMIT=64;
const ARG_WITHHELD='(value cannot be shown in full — approval withheld; one-time options only)';
const unsafeControl=/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u;
const invisible=/[\p{Default_Ignorable_Code_Point}\p{Cf}\u2028\u2029\u2800]/u;

const identifier=character=>character!==undefined&&/[A-Za-z0-9_]/.test(character);
const truncate=(value,limit)=>value.length>limit?`${value.slice(0,limit-1)}…`:value;
const scrub=value=>Array.from(value,character=>invisible.test(character)?'�':character).join('');
const scrubSource=value=>Array.from(value,character=>{
  const code=character.codePointAt(0);
  const retained=code>=8204&&code<=8207||code===1564||code>=65024&&code<=65039||code>=6155&&code<=6159||code===10240;
  return invisible.test(character)&&!retained?'�':character;
}).join('');
const normalize=value=>scrub(String(value)).replace(/\s+/g,' ').trim();

function firstArgument(source,start) {
  while(start<source.length&&/\s/.test(source[start]))start++;
  const quote=source[start];
  if(!['"',"'",'`'].includes(quote)){
    const end=Math.min(source.length,start+SCAN_LIMIT);let depth=0,index=start;
    while(index<end){
      const character=source[index];
      if(['(','[','{'].includes(character))depth++;
      else if([')',']','}'].includes(character)){if(depth===0)break;depth--;}
      else if(character===','&&depth===0)break;
      else if(['"',"'",'`'].includes(character)){
        index++;
        while(index<end&&source[index]!==character){if(source[index]==='\\')index++;index++;}
      }
      index++;
    }
    return truncate(source.slice(start,index).trim(),PROMPT_LIMIT);
  }
  start++;const value=[];
  while(start<source.length){
    const character=source[start];
    if(character==='\\'){value.push(source[start+1]??'');start+=2;continue;}
    if(character===quote)break;
    if(quote==='`'&&character==='$'&&source[start+1]==='{'){
      value.push('${…}');let depth=1;start+=2;
      while(start<source.length&&depth>0){if(source[start]==='{')depth++;if(source[start]==='}')depth--;start++;}
      continue;
    }
    value.push(character);start++;
  }
  return truncate(value.join('').replace(/\s+/g,' ').trim(),PROMPT_LIMIT);
}

function loopExpression(source,open,forLoop) {
  if(source[open]!=='(')return undefined;
  const end=Math.min(source.length,open+SCAN_LIMIT);let depth=0;
  for(let index=open;index<end;index++){
    const character=source[index];
    if(character==='(')depth++;
    else if(character===')'){depth--;if(depth===0)return source.slice(open+1,index);}
    else if(forLoop&&character===';'&&depth===1)return source.slice(open+1,index);
  }
  return undefined;
}

function statementEnd(source,start,end) {
  let depth=0;
  for(let index=start;index<end;index++){
    const character=source[index];
    if(['(','[','{'].includes(character))depth++;
    else if([')',']','}'].includes(character)){if(depth===0)return index;depth--;}
    else if(depth===0&&character===';')return index+1;
    else if(depth===0&&character==='\n'){
      let next=index+1;while(next<end&&/\s/.test(source[next]??''))next++;
      if(source[next]!=='{')return index+1;
    }
  }
  return -1;
}

function scanPhases(source) {
  const calls=[];let braceDepth=0,parenDepth=0,pendingLoop=false,condition,bodyEnd=-1;
  const loopBlocks=[],parallelDepths=[];
  for(let index=0;index<source.length;index++){
    if(bodyEnd>=0&&index>=bodyEnd){pendingLoop=false;condition=undefined;bodyEnd=-1;}
    const character=source[index];
    if(character==='/'&&source[index+1]==='/')while(index<source.length&&source[index]!=='\n')index++;
    else if(character==='/'&&source[index+1]==='*'){
      index+=2;while(index<source.length&&!(source[index]==='*'&&source[index+1]==='/'))index++;index++;
    }else if(['"',"'",'`'].includes(character)){
      index++;while(index<source.length&&source[index]!==character){if(source[index]==='\\')index++;index++;}
    }else if(character==='{'){
      braceDepth++;
      if(pendingLoop&&parenDepth===0){loopBlocks.push({braceDepth,condition});pendingLoop=false;condition=undefined;bodyEnd=-1;}
    }else if(character==='}'){
      if(loopBlocks.at(-1)?.braceDepth===braceDepth)loopBlocks.pop();braceDepth--;
    }else if(character==='(')parenDepth++;
    else if(character===')'){
      if(parallelDepths.at(-1)===parenDepth)parallelDepths.pop();
      parenDepth--;
      if(pendingLoop&&parenDepth===0&&bodyEnd<0){
        let next=index+1,end=Math.min(source.length,index+1+SCAN_LIMIT);while(next<end&&/\s/.test(source[next]??''))next++;
        if(source[next]!=='{')bodyEnd=statementEnd(source,next,end);
      }
    }else if(character==='w'&&source.startsWith('while',index)&&parenDepth===0&&!identifier(source[index-1])&&!identifier(source[index+5])){
      const open=source.indexOf('(',index);
      if(open<0||open-index>SCAN_LIMIT)continue;
      pendingLoop=true;bodyEnd=-1;condition=loopExpression(source,open,false);
    }else if(character==='f'&&source.startsWith('for',index)&&parenDepth===0&&!identifier(source[index-1])&&!identifier(source[index+3])){
      const open=source.indexOf('(',index);
      if(open<0||open-index>SCAN_LIMIT)continue;
      pendingLoop=true;bodyEnd=-1;condition=loopExpression(source,open,true);
    }else if(character==='p'&&source.startsWith('parallel(',index)&&!identifier(source[index-1])){
      parallelDepths.push(parenDepth+1);index+=7;
    }else if(character==='a'&&source.startsWith('agent',index)&&!identifier(source[index-1])){
      let open=index+5;while(open<source.length&&/\s/.test(source[open]??''))open++;
      if(source[open]==='('){
        const kind=parallelDepths.length?'parallel':pendingLoop||loopBlocks.length?'loop':'sequential';
        const activeCondition=pendingLoop?condition:loopBlocks.at(-1)?.condition;
        calls.push({kind,annotation:kind==='loop'?activeCondition?.trim().slice(0,40):kind==='parallel'?'× N':undefined,prompt:firstArgument(source,open+1)});
      }
    }
  }
  if(!calls.length)return null;
  const groups=[];
  for(const call of calls){
    const previous=groups.at(-1);
    if(previous?.kind===call.kind&&previous.annotation===call.annotation)previous.prompts.push(call.prompt);
    else groups.push({kind:call.kind,annotation:call.annotation,prompts:[call.prompt]});
  }
  return groups;
}

function mergePhases(inferred,metadataPhases) {
  const prompts=phase=>{
    const seen=new Set;
    return (phase?.prompts??[]).flatMap(prompt=>{
      if(!prompt||seen.has(prompt))return[];seen.add(prompt);return[normalize(prompt)];
    });
  };
  const inferredPhase=phase=>({title:normalize(`${{sequential:'step',parallel:'parallel',loop:'loop'}[phase.kind]}${phase.annotation?` ${phase.annotation}`:''}`),prompts:prompts(phase)});
  if(Array.isArray(metadataPhases)&&metadataPhases.length){
    const phases=metadataPhases.map((phase,index)=>{
      const title=typeof phase==='string'?phase:phase.title;
      return {title:normalize(title),...(typeof phase==='object'&&phase?.detail!==undefined?{detail:normalize(phase.detail)}:{}),prompts:prompts(inferred?.[index])};
    });
    return [...phases,...(inferred??[]).slice(metadataPhases.length).map(inferredPhase)];
  }
  return inferred?.map(inferredPhase)??null;
}

function spend(budget,units) {
  budget.units-=units;if(budget.units<0)throw new Error('units');
}

function serialize(value,budget,depth=0) {
  if(typeof value==='string'){const encoded=JSON.stringify(value);spend(budget,encoded.length);return encoded;}
  if(typeof value==='number'){const encoded=Number.isFinite(value)?String(value):'null';spend(budget,encoded.length);return encoded;}
  if(typeof value==='boolean'||value===null){const encoded=String(value);spend(budget,encoded.length);return encoded;}
  if(typeof value==='bigint')throw new Error('unsupported');
  if(typeof value!=='object')return undefined;
  if(depth>=DEPTH_LIMIT)throw new Error('depth');
  if(Array.isArray(value)){
    budget.elements-=value.length;if(budget.elements<0)throw new Error('elements');
    spend(budget,2+Math.max(0,value.length-1));
    const output=[];
    for(const item of value){const encoded=serialize(item,budget,depth+1);if(encoded===undefined)spend(budget,4);output.push(encoded??'null');}
    return `[${output.join(',')}]`;
  }
  const entries=Object.entries(value);budget.elements-=entries.length;if(budget.elements<0)throw new Error('elements');spend(budget,2);
  const output=[];
  for(const [key,item] of entries){
    const encoded=serialize(item,budget,depth+1);if(encoded===undefined)continue;
    const encodedKey=JSON.stringify(key);spend(budget,encodedKey.length+1+(output.length?1:0));output.push(`${encodedKey}:${encoded}`);
  }
  return `{${output.join(',')}}`;
}

function presentArgs(value) {
  let text;
  try{text=typeof value==='string'?value:serialize(value,{elements:ELEMENT_LIMIT,units:PRESENTATION_LIMIT});}
  catch{return {text:ARG_WITHHELD,needsGutter:false,withheld:true};}
  if(text===undefined||text.length>PRESENTATION_LIMIT)return {text:ARG_WITHHELD,needsGutter:false,withheld:true};
  text=scrub(text).replace(/\t/g,' ');
  if(unsafeControl.test(text))return {text:ARG_WITHHELD,needsGutter:false,withheld:true};
  return {text,needsGutter:text.includes('\n')||text.length>80,withheld:false};
}

function presentSource(source) {
  const originalLength=source.length;
  if(originalLength>PRESENTATION_LIMIT||unsafeControl.test(source)){
    return {text:`(script of ${originalLength.toLocaleString('en-US')} characters cannot be shown in full — approval is unavailable; deny or send feedback)`,withheld:true,originalLength};
  }
  return {text:scrubSource(source),withheld:false,originalLength};
}

export function workflowConsentPresentation(source,options={}) {
  if(typeof source!=='string')throw new TypeError('Workflow source must be a string');
  const body=Object.hasOwn(options,'body')?options.body:source;
  if(typeof body!=='string')throw new TypeError('Workflow body must be a string');
  return {
    phases:mergePhases(scanPhases(body),options.meta?.phases),
    ...(Object.hasOwn(options,'args')?{args:presentArgs(options.args)}:{}),
    source:presentSource(source),
  };
}
