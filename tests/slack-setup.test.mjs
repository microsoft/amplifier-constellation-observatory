import test from 'node:test';
import assert from 'node:assert/strict';
import {createSlackSetupController,deriveSlackSetupState,publicSlackSetup,slackAuthorizationUrl,mountSlackSetup} from '../ui/slack-setup.js';

const endpoint={id:'e05391f6-97aa-4a4e-b1b9-68947e1299b4',revision:7,label:'Notebook'};
const operationId='d994ef20-7f67-4fd3-b68f-183f45d5e598';
const requestId='ae02ed89-b1a2-4dfa-8cc9-f43095b4a933';
const clone=value=>JSON.parse(JSON.stringify(value));
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return{promise,resolve,reject};};
function storage(){const entries=new Map();return{entries,getItem:key=>entries.get(key)||null,setItem:(key,value)=>entries.set(key,value),removeItem:key=>entries.delete(key)};}
function observation(overrides={}) {
  return {schema:1,endpoint_id:endpoint.id,endpoint_revision:endpoint.revision,revision:1,operation:null,
    connector:{id:'',endpoint_id:endpoint.id,binding_verified:false,account_ready:false,setup_complete:false,connection_ready:false,connection_running:false},
    setup:{authorization:'missing',authorization_pending:false,next_action:'authorize',workspace_mismatch:false,intended_workspace:null,authorized_workspace:null},
    capabilities:{can_prepare:true},...overrides};
}
function prepared(overrides={}) {
  return observation({revision:2,operation:{id:operationId,request_id:requestId,phase:'account',state:'ready',uncertain:false},connector:{id:'connector',endpoint_id:endpoint.id,binding_verified:true,account_ready:true,setup_complete:false},capabilities:{can_authorize:true},...overrides});
}
function fixture(initial=observation()) {
  let current=clone(initial),onPost=async()=>current;const calls=[],store=storage();
  const api=async(path,method,body)=>{calls.push({path,method,body:body&&clone(body)});return method==='GET'?clone(current):onPost(body);};
  const controller=createSlackSetupController({endpoint,api,storage:store,randomUUID:()=>requestId});
  return {controller,calls,store,api,set(value){current=clone(value);},post(fn){onPost=fn;}};
}

test('opening and refreshing only read this exact endpoint',async()=>{
  const f=fixture();await f.controller.refresh();await f.controller.refresh();
  assert.deepEqual(f.calls.map(c=>[c.path,c.method]),Array(2).fill([`endpoints/${endpoint.id}/slack-setup`,'GET']));
  assert.equal(f.controller.snapshot().phase,'prepare');assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});

test('reconciled native start requires a separate explicit bind and never repeats preparation',async()=>{
  const f=fixture(prepared({operation:{id:operationId,request_id:requestId,phase:'bind',state:'complete',uncertain:false},connector:{},capabilities:{can_bind:true}}));
  await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'bind');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
  await assert.rejects(f.controller.act('prepare'));await assert.rejects(f.controller.act('authorize'));
  f.post(async body=>{assert.deepEqual(body,{action:'bind',operation_id:operationId,endpoint_revision:endpoint.revision,expected_revision:2});return prepared({revision:3});});
  await f.controller.act('bind');assert.equal(f.controller.snapshot().phase,'authorize');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('unresolved startup cannot expose explicit bind even with a malformed offered capability',()=>{
  for(const state of ['unknown','running']){
    const value=publicSlackSetup(prepared({operation:{id:operationId,state},capabilities:{can_bind:true}}),endpoint);
    assert.equal(deriveSlackSetupState(value).can.bind,undefined);
  }
});

test('prepare retains only its request identity before dispatch',async()=>{
  const f=fixture();await f.controller.refresh();
  f.post(async body=>{
    assert.equal(f.store.entries.size,1);assert.deepEqual(JSON.parse([...f.store.entries.values()][0]),{request_id:requestId,endpoint_revision:7});
    assert.deepEqual(body,{action:'prepare',request_id:requestId,endpoint_revision:endpoint.revision,expected_revision:1});return prepared();
  });
  await f.controller.act('prepare');assert.equal(f.controller.snapshot().uncertain,false);assert.equal(f.store.entries.size,0);
});

test('lost prepare acknowledgement and reload find one original server intent',async()=>{
  const f=fixture();await f.controller.refresh();f.post(async()=>{f.set(prepared());throw Error('lost');});
  await f.controller.act('prepare');assert.equal(f.controller.snapshot().phase,'unknown');
  await assert.rejects(f.controller.act('prepare'));assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
  f.controller.dispose();const reopened=createSlackSetupController({endpoint,api:f.api,storage:f.store,randomUUID:()=>{throw Error('must not allocate');}});
  await reopened.refresh();assert.equal(reopened.snapshot().observation.operation.id,operationId);assert.equal(reopened.snapshot().phase,'authorize');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('missing or other server operation never settles a lost prepare',async()=>{
  const f=fixture();await f.controller.refresh();f.post(async()=>{throw Error('lost');});await f.controller.act('prepare');await f.controller.refresh();
  assert.equal(f.controller.snapshot().phase,'unknown');assert.equal(f.store.entries.size,1);
  f.set(prepared({operation:{id:operationId,request_id:'c8e3db5c-6040-44ee-aef0-915e464490f8',state:'ready'}}));await f.controller.refresh();
  assert.equal(f.controller.snapshot().phase,'unknown');await assert.rejects(f.controller.act('authorize'));
});

test('unavailable persistence refuses prepare before a native request',async()=>{
  let posts=0;const controller=createSlackSetupController({endpoint,api:async(_p,m)=>{if(m==='POST')posts++;return observation();},storage:{getItem:()=>null,setItem(){throw Error('blocked');}}});
  await controller.refresh();await assert.rejects(controller.act('prepare'));assert.equal(posts,0);
});

test('unreadable saved request remains held even when GET reports no operation',async()=>{
  const c=createSlackSetupController({endpoint,api:async()=>observation(),storage:{getItem:()=>'{damaged'}});
  await c.refresh();assert.equal(c.snapshot().phase,'unknown');await assert.rejects(c.act('prepare'));
});

test('busy double click sends only one mutation',async()=>{
  const f=fixture(),wait=deferred();await f.controller.refresh();f.post(()=>wait.promise);
  const first=f.controller.act('prepare');await f.controller.act('prepare');assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
  wait.resolve(prepared());await first;
});

test('confirmation entry is disabled during a pending read and reopens empty without dispatch',async()=>{
  const current=prepared({setup:{authorization_pending:true,next_action:'confirm'},capabilities:{can_confirm:true}});
  const waiting=deferred(),calls=[];
  const container={innerHTML:'',addEventListener(){},removeEventListener(){},querySelectorAll(){return[];},querySelector(){return null;}};
  const mounted=mountSlackSetup(container,{endpoint,api:async(_path,method)=>{
    calls.push(method);assert.equal(method,'GET');return calls.length===1?clone(current):waiting.promise;
  }});
  try {
    await new Promise(resolve=>setImmediate(resolve));
    const input=()=>container.innerHTML.match(/<input name="challenge"[^>]*>/)?.[0];
    assert(input());assert(!/\sdisabled(?:\s|>)/.test(input()));
    const refresh=mounted.refresh();
    assert.match(input(),/\sdisabled(?:\s|>)/);
    assert.deepEqual(calls,['GET','GET']);
    waiting.resolve(clone(current));await refresh;
    assert(input());assert(!/\sdisabled(?:\s|>)/.test(input()));
    assert(!/\svalue=/.test(input()));assert.deepEqual(calls,['GET','GET']);
  } finally {mounted.dispose();}
});

test('undeclared workspace approval waits for explicit exact confirmation',async()=>{
  const f=fixture(prepared({setup:{authorization:'managed',next_action:'connect',authorized_workspace:{team_id:'TWORKSHOP',domain:'workshop'}} ,capabilities:{can_connect:true,can_discard:true}}));
  await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'workspace');assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
  await assert.rejects(f.controller.act('connect',{team_id:'TOTHER'}));assert.equal(f.calls.length,1);
  f.post(async body=>{assert.deepEqual(body,{action:'connect',operation_id:operationId,endpoint_revision:endpoint.revision,expected_revision:2,team_id:'TWORKSHOP'});return prepared({revision:3});});
  await f.controller.act('connect',{team_id:'TWORKSHOP'});assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('workspace mismatch can only authorize when the owner offers it',()=>{
  const value=publicSlackSetup(prepared({setup:{authorization:'managed',next_action:'connect',workspace_mismatch:true,intended_workspace:{team_id:'TINTENDED',name:'Intended'},authorized_workspace:{team_id:'TOTHER',domain:'other'}},capabilities:{can_connect:true,can_authorize:true,can_discard:true}}),endpoint);
  assert.deepEqual(deriveSlackSetupState(value).can,{authorize:true});
});

test('a new pending authorization can be confirmed while the previous workspace mismatch stays visible',async()=>{
  const setup={authorization:'managed',next_action:'confirm',authorization_pending:true,workspace_mismatch:true,intended_workspace:{team_id:'TINTENDED',name:'Intended'},authorized_workspace:{team_id:'TOTHER',domain:'other'}};
  const f=fixture(prepared({setup,capabilities:{can_confirm:true,can_connect:true}}));
  await f.controller.refresh();
  assert.equal(f.controller.snapshot().phase,'confirm');
  assert.deepEqual(f.controller.snapshot().can,{confirm:true});
  await assert.rejects(f.controller.act('connect',{team_id:'TOTHER'}));
  assert.equal(slackAuthorizationUrl(setup),'https://app.slack.com/client/TINTENDED');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});

test('existing paused configured connector completes without reprovision or start',async()=>{
  const f=fixture(prepared({connector:{id:'saved',endpoint_id:endpoint.id,binding_verified:true,account_ready:true,connection_ready:true,connection_running:false,setup_complete:true}}));
  await f.controller.refresh();assert.equal(f.controller.snapshot().done,true);assert.equal(f.controller.snapshot().phase,'done');
  await assert.rejects(f.controller.act('authorize'));await assert.rejects(f.controller.act('prepare'));assert.equal(f.calls.length,1);
});

test('running alone and an unverified or different binding never enable Done',()=>{
  for(const connector of [{connection_running:true},{id:'a',setup_complete:true,binding_verified:false,endpoint_id:endpoint.id},{id:'a',setup_complete:true,binding_verified:true,endpoint_id:'other'}]) {
    assert.equal(deriveSlackSetupState(publicSlackSetup(prepared({connector}),endpoint)).done,false);
  }
});

test('password and confirmation secrets never enter snapshots or browser persistence',async()=>{
  const f=fixture(prepared({connector:{account_ready:false},capabilities:{can_claim:true}}));await f.controller.refresh();
  f.post(async body=>{assert.equal(body.password,'private chosen password');return prepared({revision:3,token:'private-management-secret',setup_token:'private-capability',connector:{...prepared().connector,password:'not-public'}});});
  await f.controller.act('claim',{password:'private chosen password'});
  const snapshot=JSON.stringify(f.controller.snapshot());for(const secret of ['private chosen password','private-management-secret','private-capability','not-public'])assert(!snapshot.includes(secret));
  assert.equal(f.store.entries.size,0);
  f.set(prepared({revision:4,setup:{authorization_pending:true,next_action:'confirm'},capabilities:{can_confirm:true}}));await f.controller.refresh();
  f.post(async body=>{assert.equal(body.challenge,'private-code');throw Error('server reflected private-code');});await f.controller.act('confirm',{challenge:'private-code'});
  assert(!JSON.stringify(f.controller.snapshot()).includes('private-code'));assert.equal(f.store.entries.size,0);
});

test('ticket is transient, bounded, expiring and omitted from public observations',async()=>{
  let clock=100;const initial=prepared();let post=false;
  const controller=createSlackSetupController({endpoint,storage:storage(),now:()=>clock,api:async(_p,m)=>{
    if(m==='GET')return initial;post=true;return prepared({revision:3,authorization_command:{command:'/slackauthticket private-ticket',expires_at:200},setup:{next_action:'confirm',authorization_pending:true},capabilities:{can_confirm:true}});
  }});
  await controller.refresh();await controller.act('authorize');assert(post);assert.equal(controller.authorizationCommand().command,'/slackauthticket private-ticket');
  assert(!JSON.stringify(controller.snapshot()).includes('private-ticket'));clock=200;assert.equal(controller.authorizationCommand(),null);
  controller.dispose();assert.equal(controller.authorizationCommand(),null);
});

test('read-only same-operation ticket retrieval resumes after reload without authorize',async()=>{
  const calls=[];const c=createSlackSetupController({endpoint,storage:storage(),now:()=>100,api:async(_p,m)=>{calls.push(m);return prepared({setup:{next_action:'confirm',authorization_pending:true},capabilities:{can_confirm:true},authorization_command:{command:'/slackauthticket existing-ticket',expires_at:200}});}});
  await c.refresh();assert.equal(c.authorizationCommand().command,'/slackauthticket existing-ticket');assert.deepEqual(calls,['GET']);assert(!JSON.stringify(c.snapshot()).includes('existing-ticket'));
});

test('HTTP success with an unchanged operation revision is not mutation settlement',async()=>{
  const f=fixture(prepared());await f.controller.refresh();f.post(async()=>prepared());await f.controller.act('authorize');
  assert.equal(f.controller.snapshot().phase,'unknown');await assert.rejects(f.controller.act('authorize'));assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('lost deployment reply cannot be settled by old running/ready observation',async()=>{
  const value=prepared({setup:{authorization:'managed',next_action:'connect',authorized_workspace:{team_id:'TWORKSHOP'}},capabilities:{can_connect:true}});
  const f=fixture(value);await f.controller.refresh();f.post(async()=>{throw Error('timeout');});await f.controller.act('connect',{team_id:'TWORKSHOP'});
  f.set({...value,connector:{...value.connector,connection_running:true}});await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'unknown');
  await assert.rejects(f.controller.act('connect',{team_id:'TWORKSHOP'}));assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
  f.set({...value,revision:3,operation:{...value.operation,state:'unknown',uncertain:true}});await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'unknown');
});

test('same-operation newer committed observation reconciles without repeating action',async()=>{
  const f=fixture(prepared());await f.controller.refresh();f.post(async()=>{throw Error('lost');});await f.controller.act('authorize');
  f.set(prepared({revision:3,setup:{next_action:'confirm',authorization_pending:true},capabilities:{can_confirm:true}}));await f.controller.refresh();
  assert.equal(f.controller.snapshot().phase,'confirm');assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('foreign endpoint, changed operation and stale revision each disable mutations',async()=>{
  for(const invalid of [prepared({endpoint_id:'other'}),prepared({endpoint_revision:8}),prepared({revision:0}),prepared({operation:{id:requestId,state:'ready'}})]) {
    const f=fixture(prepared());await f.controller.refresh();f.set(invalid);await f.controller.refresh();
    assert.equal(f.controller.snapshot().phase,'unknown');await assert.rejects(f.controller.act('authorize'));assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
  }
});

test('failed read and missing capabilities never enable setup',async()=>{
  let fail=false;const controller=createSlackSetupController({endpoint,storage:storage(),api:async()=>{if(fail)throw Error('offline');return observation({capabilities:{}});}});
  await controller.refresh();assert.equal(controller.snapshot().can.prepare,false);fail=true;await controller.refresh();assert.equal(controller.snapshot().phase,'unknown');await assert.rejects(controller.act('prepare'));
});

test('dispose during a read ignores late response and schedules no follow-up',async()=>{
  const wait=deferred();let changes=0,calls=0;const c=createSlackSetupController({endpoint,api:()=>{calls++;return wait.promise;},onChange:()=>changes++});
  const work=c.refresh();c.dispose();const count=changes;wait.resolve(prepared());await work;assert.equal(changes,count);assert.equal(c.snapshot().observation,null);await c.refresh();assert.equal(calls,1);
});

test('dispose during mutation retains owner work, never sends cancel or another action',async()=>{
  const f=fixture(prepared()),wait=deferred();await f.controller.refresh();f.post(()=>wait.promise);const work=f.controller.act('authorize');
  f.controller.dispose();wait.resolve(prepared({revision:3}));await work;assert.equal(f.controller.snapshot().observation,null);assert.equal(f.calls.length,2);assert.equal(f.calls[1].body.action,'authorize');
});

test('Slack links never use untrusted URL metadata or include ticket/codes',()=>{
  assert.equal(slackAuthorizationUrl({authorized_workspace:{team_id:'TWORKSHOP',domain:'workshop'}}),'https://workshop.slack.com/messages/@slackbot');
  for(const domain of ['evil.invalid/path','x.slack.com.evil.invalid','https://evil.invalid','user@evil','workshop?secret=code'])assert.equal(slackAuthorizationUrl({authorized_workspace:{team_id:'TWORKSHOP',domain}}),'https://app.slack.com/client/TWORKSHOP');
  assert.equal(slackAuthorizationUrl({authorized_workspace:{team_id:'https://evil.invalid'}}),'https://app.slack.com/');
});


function refusal(body,code='package_unavailable') {
  return {schema:1,kind:'slack_setup_refusal',refused_before_intent:true,endpoint_id:endpoint.id,
    endpoint_revision:body.endpoint_revision,action:body.action,request_id:body.request_id??null,
    operation_id:body.operation_id??null,expected_revision:body.expected_revision,code,
    message:'The server refused this request before creating an intention.'};
}

test('correlated unavailable-package refusal clears only rejected prepare and permits a new explicit request',async()=>{
  const store=storage(),calls=[],nextId='c8e3db5c-6040-44ee-aef0-915e464490f8';let available=false,current=observation(),allocated=0;
  const c=createSlackSetupController({endpoint,storage:store,randomUUID:()=>allocated++?nextId:requestId,api:async(_p,m,b)=>{
    calls.push({method:m,body:b&&clone(b)});
    if(m==='GET')return clone(current);
    if(!available)return refusal(b);
    current=prepared({operation:{id:operationId,request_id:b.request_id,state:'ready'}});return clone(current);
  }});
  await c.refresh();await c.act('prepare');
  assert.equal(c.snapshot().phase,'prepare');assert.equal(c.snapshot().uncertain,false);assert.equal(store.entries.size,0);
  assert.deepEqual(calls.map(x=>x.method),['GET','POST','GET']);assert.equal(allocated,1);
  available=true;await c.act('prepare');assert.equal(c.snapshot().phase,'authorize');
  assert.deepEqual(calls.filter(x=>x.method==='POST').map(x=>x.body.request_id),[requestId,nextId]);
});

test('correlated pre-intent CAS refusal reads current state and waits for another explicit action',async()=>{
  const f=fixture(prepared());await f.controller.refresh();let attempt=0;
  f.post(async body=>{if(!attempt++){f.set(prepared({revision:3}));return refusal(body,'setup_changed');}return prepared({revision:4,setup:{next_action:'confirm',authorization_pending:true},capabilities:{can_confirm:true}});});
  await f.controller.act('authorize');assert.equal(f.controller.snapshot().phase,'authorize');assert.equal(f.controller.snapshot().uncertain,false);
  assert.equal(f.calls.filter(c=>c.method==='POST').length,1);assert.equal(f.controller.snapshot().observation.revision,3);
  await f.controller.act('authorize');assert.equal(f.controller.snapshot().phase,'confirm');assert.equal(f.calls.filter(c=>c.method==='POST').length,2);
});

test('generic status errors and unbound refusal markers cannot clear a prepare intention',async()=>{
  const mutations=[r=>({...r,schema:2}),r=>({...r,kind:'other'}),r=>({...r,refused_before_intent:false}),r=>({...r,endpoint_id:operationId}),r=>({...r,endpoint_revision:8}),r=>({...r,action:'authorize'}),r=>({...r,request_id:operationId}),r=>({...r,operation_id:operationId}),r=>({...r,expected_revision:2}),r=>({...r,code:'untrusted'}),r=>({...r,request_id:null})];
  for(const transform of mutations){const f=fixture();await f.controller.refresh();f.post(async body=>transform(refusal(body)));await f.controller.act('prepare');await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'unknown');assert.equal(f.store.entries.size,1);await assert.rejects(f.controller.act('prepare'));assert.equal(f.calls.filter(c=>c.method==='POST').length,1);}
  for(const status of [undefined,400,409,501]){const f=fixture();await f.controller.refresh();f.post(async()=>{throw Object.assign(Error('unconfirmed'),{status});});await f.controller.act('prepare');await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'unknown');assert.equal(f.store.entries.size,1);assert.equal(f.calls.filter(c=>c.method==='POST').length,1);}
});

test('pre-intent refusal cannot turn a separately saved unknown operation into ready',async()=>{
  const f=fixture(prepared());await f.controller.refresh();
  f.post(async body=>{f.set(prepared({revision:3,operation:{id:operationId,request_id:requestId,state:'unknown',uncertain:true}}));return refusal(body,'setup_changed');});
  await f.controller.act('authorize');assert.equal(f.controller.snapshot().phase,'unknown');await assert.rejects(f.controller.act('authorize'));assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});


test('saved inactive connector offers one explicit resume, never an automatic restart',async()=>{
  const stopped=prepared({operation:{id:operationId,request_id:requestId,state:'complete'},connector:{...prepared().connector,binding_verified:false},capabilities:{can_resume:true}});
  const f=fixture(stopped);await f.controller.refresh();assert.equal(f.controller.snapshot().phase,'resume');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
  f.post(async body=>{assert.equal(body.action,'resume');assert.equal(body.operation_id,operationId);return prepared({revision:3,operation:{id:operationId,request_id:requestId,state:'complete'}});});
  await f.controller.act('resume');assert.equal(f.controller.snapshot().phase,'authorize');
  assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});

test('unknown saved start cannot use even an incorrectly offered resume capability',async()=>{
  const f=fixture(prepared({operation:{id:operationId,state:'unknown',uncertain:true},capabilities:{can_resume:true}}));
  await f.controller.refresh();await assert.rejects(f.controller.act('resume'));
  assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});

test('read-only ticket restoration is transient and a later missing ticket clears the display',async()=>{
  const pending=prepared({setup:{authorization_pending:true,next_action:'confirm'},capabilities:{can_confirm:true},authorization_command:{command:'/slackauthticket restored-ticket',expires_at:Date.now()+600000}});
  const f=fixture(pending);await f.controller.refresh();assert.equal(f.controller.authorizationCommand().command,pending.authorization_command.command);
  assert(!JSON.stringify(f.controller.snapshot()).includes('restored-ticket'));assert.equal(f.store.entries.size,0);
  delete pending.authorization_command;pending.revision++;f.set(pending);await f.controller.refresh();
  assert.equal(f.controller.authorizationCommand(),null);assert.equal(f.calls.filter(c=>c.method==='POST').length,0);
});

test('known declined confirmation presents the completed refusal without automatic approval',async()=>{
  const f=fixture(prepared({setup:{authorization_pending:true,next_action:'confirm'},capabilities:{can_confirm:true}}));await f.controller.refresh();
  f.post(async()=>prepared({revision:3,operation:{id:operationId,request_id:requestId,state:'complete'},message:'Slack did not accept this confirmation code. Start a new approval explicitly.',confirmation_refusal:'invalid_challenge'}));
  await f.controller.act('confirm',{challenge:'declined-code'});
  assert.equal(f.controller.snapshot().uncertain,false);assert.equal(f.controller.snapshot().phase,'authorize');
  assert.match(f.controller.snapshot().observation.message,/not accept/);assert.equal(f.calls.filter(c=>c.method==='POST').length,1);
});
