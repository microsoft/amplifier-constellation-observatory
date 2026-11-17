// Finite UI controller checks; no installation, agent, model or network calls.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {randomUUID,createHash} from 'node:crypto';
const source = await fs.readFile(new URL('../ui/approval-policy.js', import.meta.url), 'utf8');
const {createApprovalPolicyController, renderApprovalPolicy} = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
const clone = value => structuredClone(value);
const baseTargets = [
    {endpoint_id:'one', label:'First agent', endpoint_revision:3, instance_id:'instance-one', policy_revision:7, mode:'ask', status:'ready'},
    {endpoint_id:'two', label:'Other computer', endpoint_revision:4, instance_id:'instance-two', policy_revision:null, mode:null, status:'offline'},
    {endpoint_id:'three', label:'Third agent', endpoint_revision:5, instance_id:'instance-three', policy_revision:11, mode:'ask', status:'ready'},
];
function snapshot(targets = baseTargets, last_operation = null) {
    return {mode:targets.every(t=>t.mode==='ask')?'ask':'unknown',complete:false,observed_at:123,targets:clone(targets),preview:{targets:targets.map(({endpoint_id,endpoint_revision,instance_id,policy_revision})=>({endpoint_id,endpoint_revision,instance_id,policy_revision}))},last_operation};
}
function setup(handler) {
    const calls=[], saved=new Map();
    const storage={getItem:key=>saved.get(key)||null,setItem:(key,value)=>saved.set(key,value),removeItem:key=>saved.delete(key)};
    const api=async(path,method='GET',body)=>{calls.push({path,method,body:clone(body)});return handler(path,method,body)};
    return {calls,saved,storage,controller:createApprovalPolicyController({api,storage,storageKey:'fixture',uuid:randomUUID}),api};
}
const check = async(name,fn)=>{await fn();console.log('PASS '+name)};
await check('Opening and choosing a mode do not change policies',async()=>{
    const f=setup(()=>snapshot());await f.controller.refresh();f.controller.select('automatic');
    assert(f.controller.canApply());assert(f.calls.every(c=>c.method==='GET'));
});
await check('Explicit Apply binds the entire preview, including offscreen and unavailable agents, once',async()=>{
    let resolve;const wait=new Promise(r=>resolve=r);let id;
    const f=setup((path,method,body)=>{if(method==='POST'){id=body.request_id;return wait}return snapshot()});
    await f.controller.refresh();f.controller.select('automatic');const first=f.controller.apply();await f.controller.apply();
    assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
    assert.deepEqual(f.calls.find(c=>c.method==='POST').body.targets,snapshot().preview.targets);
    assert.deepEqual(Object.keys(f.calls.find(c=>c.method==='POST').body).sort(),['mode','request_id','targets']);
    resolve({id,mode:'automatic',status:'running',targets:[]});await first;
    assert(!f.controller.canApply());
});
await check('Already matching modes and access refusal cannot send writes',async()=>{
    const f=setup(()=>snapshot([baseTargets[0]]));await f.controller.refresh();assert(!f.controller.canApply());await f.controller.apply();
    assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
    const denied=setup(()=>{throw Object.assign(Error('private diagnostic'),{status:403})});await denied.controller.refresh();denied.controller.select('automatic');assert(!denied.controller.canApply());
    assert(!renderApprovalPolicy(denied.controller).includes('private diagnostic'));
});
await check('Definite conflict needs a fresh preview, not an automatic retry',async()=>{
    const f=setup((path,method)=>{if(method==='POST')throw Object.assign(Error('revision mismatch'),{status:409});return snapshot()});
    await f.controller.refresh();f.controller.select('automatic');await f.controller.apply();assert(!f.controller.state.snapshot);assert(!f.controller.state.requestId);assert(!f.controller.canApply());
    await f.controller.refresh();assert(f.controller.canApply());assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});
await check('Lost submission reply uses only the same request GET, including after reload',async()=>{
    let operation=null;
    const f=setup((path,method,body)=>{
        if(method==='POST'){operation={id:body.request_id,mode:body.mode,status:'complete',targets:[{...baseTargets[0],status:'succeeded',observed:{mode:'automatic'}}]};throw Error('connection lost')}
        if(path.startsWith('approval-policy/operations/'))return clone(operation);
        return snapshot(baseTargets,operation);
    });
    await f.controller.refresh();f.controller.select('automatic');await f.controller.apply();assert(f.controller.state.operation);
    const reloaded=createApprovalPolicyController({api:f.api,storage:f.storage,storageKey:'fixture'});await reloaded.refresh();
    assert.equal(reloaded.state.operation.id,operation.id);assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});
await check('Unconfirmed request with no readable receipt stays blocked and is never resent',async()=>{
    const f=setup((path,method)=>{if(method==='POST')throw Error('connection lost');if(path.includes('/operations/'))throw Object.assign(Error('not found'),{status:404});return snapshot()});
    await f.controller.refresh();f.controller.select('automatic');await f.controller.apply();assert(f.controller.state.uncertain);assert(!f.controller.canApply());
    await f.controller.refresh();await f.controller.poll();await f.controller.apply();assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});
await check('Automatic works with no delegated Always clients and Ask does not alter rules',async()=>{
    const f=setup(()=>snapshot([{...baseTargets[0],mode:'automatic',delegated_clients:[],rules:[{id:'remembered'}]}]));
    await f.controller.refresh();f.controller.select('ask');assert(f.controller.canApply());
    assert(!Object.hasOwn(f.controller.state.snapshot.preview.targets[0],'delegated_clients'));
});
await check('Partial and unknown outcomes stay distinct and untrusted details are not rendered',async()=>{
    const operation={id:randomUUID(),mode:'automatic',status:'partial',targets:[{endpoint_id:'one',status:'succeeded',observed:{mode:'automatic'}},{endpoint_id:'two',status:'unknown',observed:{mode:'automatic',matches_requested_mode:true},detail:'secret diagnostic'}]};
    const f=setup(path=>path.includes('/operations/')?operation:snapshot([{...baseTargets[0],label:'<img onerror=bad()>',mode:'ask'},baseTargets[1]],operation));
    await f.controller.refresh();const html=renderApprovalPolicy(f.controller);
    assert(html.includes('1 of 2 agents confirmed Automatic'));assert(html.includes('Change unconfirmed'));assert(!html.includes('secret diagnostic'));assert(!html.includes('<img onerror'));
    assert(html.includes('ow-approval-state">Ask</span>')); // Current read is distinct from the prior change result.
});
await check('Server last operation restores tracking when local browser storage has no receipt',async()=>{
    const op={id:randomUUID(),mode:'automatic',status:'running',targets:[]};const f=setup(path=>path.includes('/operations/')?op:snapshot(baseTargets,op));await f.controller.refresh();assert.equal(f.controller.state.requestId,op.id);assert(!f.controller.canApply());assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});
await check('A newer operation from another browser replaces an older completed view and blocks Apply',async()=>{
    const old={id:randomUUID(),mode:'automatic',status:'complete',targets:[]};
    const latest={id:randomUUID(),mode:'ask',status:'running',targets:[]};
    const f=setup(path=>path==='approval-policy'?snapshot(baseTargets,latest):path.endsWith(old.id)?old:latest);
    f.saved.set('fixture',old.id);await f.controller.refresh();
    assert.equal(f.controller.state.operation.id,latest.id);assert(!f.controller.canApply());
    assert.equal(f.saved.get('fixture:previous:'+old.id),old.id);assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});

await check('New-agent default can be saved when every existing agent already matches',async()=>{
 let defaultValue={revision:0,mode:null};
 const f=setup((path,method,body)=>{if(method==='PUT'){assert.equal(path,'approval-policy/default');assert.deepEqual(body,{expected_revision:0,mode:'automatic'});defaultValue={revision:1,mode:'automatic'};return defaultValue;}return {...snapshot([{...baseTargets[0],mode:'automatic'}]),new_agent_default:defaultValue};});
 await f.controller.refresh();assert(!f.controller.canApply());f.controller.setInherit(true);assert(f.controller.canApply());await f.controller.apply();
 assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);assert.equal(f.calls.filter(c=>c.method==='POST').length,0);assert.equal(f.controller.state.snapshot.new_agent_default.mode,'automatic');
 assert(renderApprovalPolicy(f.controller).includes('Later per-agent changes are preserved'));
});
await check('Lost default response requires readback and never resends automatically',async()=>{
 let defaultValue={revision:0,mode:null};const f=setup((path,method)=>{if(method==='PUT'){defaultValue={revision:1,mode:'automatic'};throw Error('lost reply');}return {...snapshot([{...baseTargets[0],mode:'automatic'}]),new_agent_default:defaultValue};});
 await f.controller.refresh();f.controller.setInherit(true);await f.controller.apply();assert.equal(f.controller.state.snapshot,null);assert(!f.controller.canApply());
 await f.controller.refresh();assert(!f.controller.canApply());assert.equal(f.calls.filter(c=>c.method==='PUT').length,1);
});
console.log('12 approval UI controls passed');
console.log(JSON.stringify({sourceSha256:createHash('sha256').update(source).digest('hex'),testSha256:createHash('sha256').update(await fs.readFile(fileURLToPath(import.meta.url))).digest('hex'),passed:12}));
