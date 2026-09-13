const abortError=signal=>signal.reason??new Error('Prefix-stagger wait aborted');

export function createPrefixStagger({delayMs=5000,setTimer=setTimeout,clearTimer=clearTimeout}={}) {
  if(!Number.isInteger(delayMs)||delayMs<0)throw new TypeError('Prefix stagger delay must be a non-negative integer');
  const groups=new Map();
  const release=group=>{
    if(group.released)return;group.released=true;clearTimer(group.timer);
    for(const token of group.members)if(token.resolve){token.signal?.removeEventListener('abort',token.abort);token.resolve(token);delete token.resolve;}
  };
  return {
    enter(key,{signal}={}) {
      if(signal?.aborted)return Promise.reject(abortError(signal));
      if(delayMs===0)return Promise.resolve({leader:true});
      let group=groups.get(key);
      if(!group){group={key,released:false,members:new Set()};groups.set(key,group);group.timer=setTimer(()=>release(group),delayMs);const token={group,leader:true};group.members.add(token);return Promise.resolve(token);}
      const token={group,leader:false,signal};group.members.add(token);
      if(group.released)return Promise.resolve(token);
      return new Promise((resolve,reject)=>{token.resolve=resolve;token.abort=()=>{if(!token.resolve)return;delete token.resolve;group.members.delete(token);reject(abortError(signal));if(!group.members.size){clearTimer(group.timer);groups.delete(key);}};signal?.addEventListener('abort',token.abort,{once:true});});
    },
    started(token){if(token?.leader&&token.group)release(token.group);},
    finish(token){
      const group=token?.group;if(!group)return;
      if(token.leader&&!group.released)release(group);
      token.signal?.removeEventListener('abort',token.abort);group.members.delete(token);
      if(!group.members.size){clearTimer(group.timer);groups.delete(group.key);}
    },
  };
}

export function prefixStaggerDelay(value=process.env.ULTRACODE_WORKFLOW_PREFIX_STAGGER_MS) {
  if(value===undefined)return 5000;
  const delay=Number(value);if(!Number.isInteger(delay)||delay<0)throw new Error('ULTRACODE_WORKFLOW_PREFIX_STAGGER_MS must be a non-negative integer');return delay;
}
