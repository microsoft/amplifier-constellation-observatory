import test from 'node:test';
import assert from 'node:assert/strict';
import {webcrypto} from 'node:crypto';
import {startAgentInstance,readInstanceSetup,instanceFinishCandidate,instanceIntentSatisfied,instanceSetupReady} from '../ui/agent-setup.js';
globalThis.crypto ||= webcrypto;
const clone=value=>structuredClone(value);
const id='selected-instance',endpointRevision=8;
const original={instance_id:id,owner:'owner',setup_complete:false,enabled:false,name:'Original agent',backend:{command:'/private/selected-runtime',args:['--fixed'],env:{MODEL:'selected'},auth_policy:'keep'},server:{bind:'127.0.0.1',port:1234},workspace:{root:'/private/work'},limits:{turns:12},remembered:{rules:['original'],sessions:['old-session']}};
function fixture(changes={}) {
  let document={revision:10,config:{...clone(original),...changes}},intent=null,closed=false;
  const calls=[],intents=[],hooks={};
  const api=async(op,body={})=>{
    calls.push({op,body:clone(body)});await hooks.before?.(op,body);
    if(op==='config')return clone(document);
    if(op==='status')return{instance_id:id,config_revision:document.revision,setup_complete:document.config.setup_complete,enabled:document.config.enabled};
    if(op==='apply-config'){assert.equal(body.expected_revision,document.revision);document={revision:document.revision+1,config:clone(body.config)};await hooks.after?.(op);return clone(document);}
    if(op==='lifecycle'){assert.deepEqual(body,{action:'start'});document={revision:document.revision+1,config:{...document.config,enabled:true}};await hooks.after?.(op);return{enabled:true};}
    throw Error('Unexpected operation '+op);
  };
  const call=async(op,body)=>{if(closed)throw Error('closed');const result=await api(op,body);if(closed)throw Error('closed');return result;};
  const options={call,expectedId:id,endpointRevision,remember:value=>{intent=clone(value);intents.push(clone(value));},confirmed:()=>{intent=null;},checkOpen:()=>{if(closed)throw Error('closed');}};
  return {calls,intents,hooks,options,call,start:()=>startAgentInstance(options),get document(){return clone(document);},get intent(){return clone(intent);},mutate(fn){document=fn(clone(document));},close(){closed=true;},open(){closed=false;}};
}
const writes=f=>f.calls.filter(c=>['apply-config','lifecycle'].includes(c.op));

test('fresh completion uses full configuration CAS then one explicit start',async()=>{
  const f=fixture(),result=await f.start();
  assert.deepEqual(writes(f),[{op:'apply-config',body:{expected_revision:10,config:{...original,setup_complete:true}}},{op:'lifecycle',body:{action:'start'}}]);
  assert.deepEqual(f.document,{revision:12,config:{...original,setup_complete:true,enabled:true}});assert(instanceSetupReady(result,id));assert.equal(f.intent,null);
  assert.deepEqual(f.intents.map(i=>i.action),['finish-instance-config','start-instance']);assert(!JSON.stringify(f.intents).includes('/private/selected-runtime'));assert(!JSON.stringify(f.intents).includes('old-session'));
});

test('finish candidate changes only setup_complete and leaves original untouched',()=>{
  const doc={revision:10,config:clone(original)},before=clone(doc);assert.deepEqual(instanceFinishCandidate(doc,id),{expected_revision:10,config:{...original,setup_complete:true}});assert.deepEqual(doc,before);
});

test('already configured and enabled agent is observed without write or restart',async()=>{
  const f=fixture({setup_complete:true,enabled:true});const result=await f.start();assert(instanceSetupReady(result,id));assert.deepEqual(writes(f),[]);assert.deepEqual(f.intents,[]);
});

test('manual paused agent remains paused on read and explicit Start does not rewrite other config',async()=>{
  const f=fixture({setup_complete:true});const read=await readInstanceSetup(f.call,id);assert.equal(instanceSetupReady(read,id),false);assert.deepEqual(writes(f),[]);
  await f.start();assert.deepEqual(writes(f),[{op:'lifecycle',body:{action:'start'}}]);assert.deepEqual(f.document.config,{...original,setup_complete:true,enabled:true});
});

test('lost completion request before effect stays unknown and never starts',async()=>{
  const f=fixture();f.hooks.before=op=>{if(op==='apply-config')throw Error('lost before effect');};await assert.rejects(f.start());
  assert.equal(f.document.config.setup_complete,false);assert.equal(f.intent.action,'finish-instance-config');assert.equal(writes(f).length,1);
  assert.equal(await instanceIntentSatisfied(f.intent,await readInstanceSetup(f.call,id),endpointRevision,id),false);
});

test('lost committed completion reply reconciles by full hash without continuing Start',async()=>{
  const f=fixture();f.hooks.after=op=>{if(op==='apply-config')throw Error('lost after effect');};await assert.rejects(f.start());
  assert.equal(f.document.config.setup_complete,true);assert.equal(f.document.config.enabled,false);assert.equal(writes(f).length,1);
  assert(await instanceIntentSatisfied(f.intent,await readInstanceSetup(f.call,id),endpointRevision,id));assert.equal(writes(f).length,1);
  f.hooks.after=null;f.options.confirmed();await f.start();assert.equal(writes(f).filter(c=>c.op==='apply-config').length,1);assert.equal(writes(f).filter(c=>c.op==='lifecycle').length,1);
});

test('lost start before effect retains its saved intent and cannot be settled by configured alone',async()=>{
  const f=fixture({setup_complete:true});f.hooks.before=op=>{if(op==='lifecycle')throw Error('lost before start');};await assert.rejects(f.start());
  assert.equal(f.document.config.enabled,false);assert.equal(f.intent.action,'start-instance');assert.equal(writes(f).length,1);
  assert.equal(await instanceIntentSatisfied(f.intent,await readInstanceSetup(f.call,id),endpointRevision,id),false);assert.equal(writes(f).length,1);
});

test('lost committed start reply settles only exact configured/enabled readback without resending',async()=>{
  const f=fixture();f.hooks.after=op=>{if(op==='lifecycle')throw Error('lost after start');};await assert.rejects(f.start());
  const observed=await readInstanceSetup(f.call,id);assert(await instanceIntentSatisfied(f.intent,observed,endpointRevision,id));assert(instanceSetupReady(observed,id));assert.equal(writes(f).length,2);
  assert.equal(await instanceIntentSatisfied(f.intent,observed,endpointRevision+1,id),false);
  f.mutate(doc=>({...doc,config:{...doc.config,name:'another edit'}}));assert.equal(await instanceIntentSatisfied(f.intent,await readInstanceSetup(f.call,id),endpointRevision,id),false);
});

test('closing during initial observation prevents all writes',async()=>{
  const f=fixture();f.hooks.before=op=>{if(op==='status')f.close();};await assert.rejects(f.start());assert.deepEqual(writes(f),[]);
});

test('closing after completion write prevents later Start and keeps intent',async()=>{
  const f=fixture();f.hooks.after=op=>{if(op==='apply-config')f.close();};await assert.rejects(f.start());assert.equal(writes(f).length,1);assert.equal(f.intent.action,'finish-instance-config');assert.equal(f.document.config.enabled,false);
});

test('concurrent stop after completion prevents pending Start',async()=>{
  const f=fixture();f.hooks.after=op=>{if(op==='apply-config')f.mutate(doc=>({revision:doc.revision+1,config:{...doc.config,enabled:false}}));};
  await assert.rejects(f.start(),/completion was not confirmed/);assert.equal(writes(f).length,1);assert.equal(f.document.config.enabled,false);
});

test('changed pre-start state is preserved rather than crossed by pending continuation',async()=>{
  const f=fixture({setup_complete:true});let reads=0;f.hooks.before=op=>{if(op==='config'&&++reads===2)f.mutate(doc=>({revision:doc.revision+1,config:{...doc.config,name:'new owner choice'}}));};
  await assert.rejects(f.start(),/changed before Start/);assert.equal(writes(f).length,0);assert.equal(f.document.config.name,'new owner choice');
});

test('wrong installation and inconsistent status refuse completion',async()=>{
  const f=fixture();f.options.expectedId='different';await assert.rejects(f.start(),/identity changed/);assert.deepEqual(writes(f),[]);
  const doc={revision:10,config:original};await assert.rejects(readInstanceSetup(async op=>op==='config'?doc:{instance_id:id,config_revision:9,setup_complete:false,enabled:false},id),/changed while/);
});
