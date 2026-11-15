import test from 'node:test';
import assert from 'node:assert/strict';
import {UNASSIGNED,bindingKey,connectionStatus,engineLabel,operationMatch,project,safeUrl} from '../ui/world-model.js';

const A='11111111-1111-4111-8111-111111111111';
const B='22222222-2222-4222-8222-222222222222';
const HOST='33333333-3333-4333-8333-333333333333';
const endpoint=(id,label='Maker',host_id=null)=>({id,label,host_id,kind:'agent',management:{access:'manage',expected_id:'installation-'+id}});
const connection=(id='slack-studio',service='slack',state='connected')=>({id,service,state,label:'Studio',revision:'owner-revision',enrolled:true,settled:false});
const catalog=(connections,extra={})=>({schema:1,services:[],connections,helper_connected:true,request:null,last_operation:null,...extra});
const read=(data,error=null)=>({data,error,checkedAt:123});
const place=(id,bindings=[])=>({id,label:'Studio',service:'slack',kind:'workspace',rooms:[{id:'design',label:'#design'}],bindings});
const observed=({status,hosted}={})=>({...endpoint(A),...(status?{observation:{status}}:{}),...(hosted?{acp_observation:{health:'reachable',agent_info:{name:'agent-instance',title:'Maker'},backend_agent_info:hosted}}:{})});

test('same labels and connection IDs remain separate across installations without an explicit venue binding',()=>{
  const model=project({endpoints:[endpoint(A),endpoint(B)],catalogs:{[A]:read(catalog([connection()])),[B]:read(catalog([connection()]))}});
  assert.equal(model.agents.length,2);
  assert.equal(model.places.length,2);
  assert.equal(new Set(model.bindings.map(b=>b.key)).size,2);
  assert.notEqual(model.bindings[0].placeId,model.bindings[1].placeId);
  assert.equal(model.bindings[0].key,bindingKey(A,'slack-studio'));
  assert.equal(model.bindings[1].key,bindingKey(B,'slack-studio'));
});

test('only explicit endpoint-scoped bindings combine a real venue',()=>{
  const shared=place('shared',[{endpoint_id:A,connection_id:'slack-studio',room_id:'design'},{endpoint_id:B,connection_id:'slack-studio',room_id:'design'}]);
  const input={endpoints:[endpoint(A),endpoint(B)],places:[shared],catalogs:{[A]:read(catalog([connection()])),[B]:read(catalog([connection()]))}};
  const before=structuredClone(input),model=project(input);
  assert.equal(model.places.length,1);assert.equal(model.places[0].bindings.length,2);
  assert.deepEqual(new Set(model.places[0].bindings.map(b=>b.agentId)),new Set([A,B]));
  assert.deepEqual(input,before,'projection cannot rewrite canonical inventory or remote observations');
});

test('same-label configured places never merge by name',()=>{
  const model=project({endpoints:[endpoint(A),endpoint(B)],places:[place('one',[{endpoint_id:A,connection_id:'slack-studio'}]),place('two',[{endpoint_id:B,connection_id:'slack-studio'}])],catalogs:{[A]:read(catalog([connection()])),[B]:read(catalog([connection()]))}});
  assert.equal(model.places.length,2);
  assert.equal(model.bindings.find(b=>b.agentId===A).placeId,'one');
  assert.equal(model.bindings.find(b=>b.agentId===B).placeId,'two');
});

test('Slack periodic settled=false without a request is connected, not a pending operation',()=>{
  const c=connection(),status=connectionStatus(catalog([c]),c);
  assert.equal(status.connected,true);assert.equal(status.label,'Connected');assert.ok(!status.pending);
});

test('offline helper never proves present connectivity from an old connected report',()=>{
  const c=connection(),cat=catalog([c],{helper_connected:false});
  const status=connectionStatus(cat,c);
  assert.equal(status.connected,false);assert.equal(status.uncertain,true);assert.match(status.label,/offline/i);
  const model=project({endpoints:[endpoint(A)],catalogs:{[A]:read(cat)}});
  assert.equal(model.bindings[0].canManage,false);
});

test('queued intent with an offline helper stays visibly uncertain instead of claiming progress',()=>{
  const c=connection(),cat=catalog([c],{helper_connected:false,request:{command:{connection_id:c.id,action:'disconnect',request_id:'pending'}}});
  const status=connectionStatus(cat,c);
  assert.equal(status.connected,false);assert.equal(status.uncertain,true);assert.match(status.label,/offline/i);
});

test('saved binding whose connection disappeared remains visible but unavailable',()=>{
  const model=project({endpoints:[endpoint(A)],places:[place('saved',[{endpoint_id:A,connection_id:'removed',room_id:'design'}])],catalogs:{[A]:read(catalog([]))}});
  assert.equal(model.places.length,1);assert.equal(model.bindings.length,1);
  const b=model.bindings[0];assert.equal(b.connection,null);assert.equal(b.placeId,'saved');
  assert.equal(b.status.connected,false);assert.equal(b.status.uncertain,true);assert.equal(b.canManage,false);
});

test('reused connection ID with a different service cannot silently move venue or vanish',()=>{
  const model=project({endpoints:[endpoint(A)],places:[place('old-slack',[{endpoint_id:A,connection_id:'reused'}])],catalogs:{[A]:read(catalog([connection('reused','minecraft')]))}});
  assert.ok(model.bindings.length>0,'retain an unavailable binding or expose the unassigned current connection');
  for(const b of model.bindings){
    if(b.placeId==='old-slack'){assert.equal(b.status.connected,false);assert.equal(b.canManage,false);assert.equal(b.status.uncertain,true)}
    if(b.connection?.service==='minecraft')assert.notEqual(b.placeId,'old-slack');
  }
});

test('computer residence requires a saved ID; matching names and addresses establish nothing',()=>{
  const host={id:HOST,label:'Spark',transport:'ssh',address:'spark.local'};
  const endpoints=[endpoint(A,'Spark'),{...endpoint(B,'Other','44444444-4444-4444-8444-444444444444'),management:{base_url:'https://spark.local'}}];
  const model=project({endpoints,hosts:[host]});
  assert.ok(model.agents.every(a=>a.machineId===UNASSIGNED));
  assert.ok(model.machines.find(m=>m.id===UNASSIGNED).unassigned);
  const assigned=project({endpoints:[endpoint(A,'Maker',HOST)],hosts:[host]});
  assert.equal(assigned.agents[0].machineId,HOST);
  assert.ok(!assigned.machines.some(m=>m.id===UNASSIGNED));
});

test('catalog failures and removed enrollment cannot retain a connected claim or controls',()=>{
  for(const reading of [read(catalog([connection()]),'unreachable'),read(catalog([{...connection(),enrolled:false}]))]){
    const b=project({endpoints:[endpoint(A)],catalogs:{[A]:reading}}).bindings[0];
    assert.equal(b.status.connected,false);assert.equal(b.status.uncertain,true);assert.equal(b.canManage,false);
  }
});

test('residence is unchanged by joining two places and endpoint observation-only access cannot act',()=>{
  const e=endpoint(A,'Maker',HOST),input={endpoints:[e],hosts:[{id:HOST,label:'Computer',transport:'local'}],catalogs:{[A]:read(catalog([connection('slack-studio'),connection('minecraft-workshop','minecraft')]))}};
  const model=project(input);assert.equal(model.agents[0].machineId,HOST);assert.equal(model.bindings.length,2);
  assert.ok(model.bindings.every(b=>b.agentId===A));
  e.management.access='observe';assert.ok(project(input).bindings.every(b=>!b.canManage));
});

test('operation reconciliation matches the original request identity only',()=>{
  const pending={command:{request_id:'pending'}},finished={command:{request_id:'finished'},finished_at:12,result:{state:'disconnected'}};
  const data={request:pending,last_operation:finished};
  assert.equal(operationMatch(data,'pending'),pending);assert.equal(operationMatch(data,'finished'),finished);
  assert.equal(operationMatch(data,'different'),null);assert.equal(operationMatch(null,'pending'),null);
});

test('external links never accept credentials or executable URL schemes',()=>{
  assert.equal(safeUrl('https://service.example/settings'),'https://service.example/settings');
  for(const value of ['javascript:alert(1)','http://service.example','https://user:secret@service.example','not a URL'])assert.equal(safeUrl(value),null);
});


test('paired computers retain their reported platform and independent agent residence',()=>{
  const host={id:HOST,label:'My computer',transport:'paired',machine:{os:'macos',state:'offline',last_seen_at:123},recipes:[]};
  const input={hosts:[host],endpoints:[endpoint(A,'Maker',HOST)]},before=structuredClone(input),model=project(input);
  assert.equal(model.machines[0].platform,'Mac');assert.equal(model.machines[0].host.machine.state,'offline');
  assert.equal(model.agents[0].machineId,HOST);assert.deepEqual(input,before);
});

test('an engine is named by what its agent reports before any name kept here',()=>{
  const hosted={name:'example-engine',title:'Example Engine',version:'1.2.3'},command='/opt/engines/example-engine-acp';
  assert.equal(engineLabel(observed({status:{engine:{name:'Declared Engine'},backend:{kind:'Declared kind',command:'/usr/bin/codex-acp'}},hosted})),'Declared Engine');
  assert.equal(engineLabel(observed({status:{backend:{kind:'Declared kind',command}},hosted})),'Declared kind');
  assert.equal(engineLabel(observed({status:{backend:{command:'/usr/bin/codex-acp'}},hosted})),'Example Engine 1.2.3');
  assert.equal(engineLabel(observed({status:{backend:{command}},hosted:{name:'example-engine',version:'1.2.3'}})),'example-engine 1.2.3');
  assert.equal(engineLabel(observed({hosted:{title:'Example Engine'}})),'Example Engine');
  assert.equal(engineLabel(observed({status:{backend:{command:'/usr/bin/codex-acp'}}})),'Codex ACP');
  assert.equal(engineLabel(observed({status:{backend:{command}}})),'Configured runtime: example-engine-acp');
  // An ACP check that failed, or an agent that discloses nothing, claims no engine.
  assert.equal(engineLabel({...endpoint(A),acp_observation:{health:'unavailable',error:'ACP connection failed'}}),null);
  assert.equal(engineLabel(observed({status:{backend:{configured:false,command:null}}})),null);
  assert.equal(engineLabel(endpoint(A)),null);
  const input={endpoints:[observed({status:{backend:{command}},hosted})]},before=structuredClone(input);
  assert.equal(project(input).agents[0].engine,'Example Engine 1.2.3');assert.deepEqual(input,before);
});

test('known engines keep their names wherever the command lives and whatever title they report',()=>{
  const names={'agent-runtime':'Constellation Native','amplifier-agent-acp':'Amplifier Agent','claude-agent-acp':'Claude ACP','codex-acp':'Codex ACP','copilot':'GitHub Copilot'};
  for(const [binary,name] of Object.entries(names)){
    for(const command of [binary,'/opt/engines/'+binary,'C:\\Engines\\'+binary+'.exe'])assert.equal(engineLabel(observed({status:{backend:{command}}})),name,command);
  }
  assert.equal(engineLabel(observed({status:{backend:{command:'C:\\Engines\\example-engine.EXE'}}})),'Configured runtime: example-engine');
  // The agent's own name in an engine title cannot displace a known engine's name.
  assert.equal(engineLabel(observed({hosted:{name:'agent-runtime',title:'Maker',version:'0.1.0'}})),'Constellation Native 0.1.0');
});

test('reported engine names are bounded plain display text and choose nothing else',()=>{
  assert.equal(engineLabel(observed({hosted:{name:'example-engine',title:'Example\u0000 \u202eEngine\n',version:'1\t.0'}})),'Example Engine 1.0');
  const long=engineLabel(observed({hosted:{name:'e'.repeat(5000),version:'9'.repeat(5000)}}));
  assert.ok([...long].length<=108,long.length);assert.ok(long.startsWith('e'.repeat(80)+'… '));
  assert.equal(engineLabel(observed({status:{engine:{name:'x'.repeat(5000)}}})),'x'.repeat(80)+'…');
  // Markup stays text for the view to escape; it is never interpreted here.
  const markup='<img src=x onerror=alert(1)>';
  for(const reading of [{status:{engine:{name:markup}}},{status:{backend:{kind:markup}}},{hosted:{name:markup}},{hosted:{name:'example-engine',title:markup}}])assert.equal(engineLabel(observed(reading)),markup);
  // Only text is a name: other shapes fall through to the next source instead of being rendered.
  const status={backend:{command:'/opt/engines/example-engine-acp'}};
  for(const name of [{text:'Example'},['copilot'],42,true,null,'',' \n'])for(const reading of [{status:{...status,engine:{name}}},{status:{backend:{...status.backend,kind:name}}},{status,hosted:{name,title:name,version:'1'}}])assert.equal(engineLabel(observed(reading)),'Configured runtime: example-engine-acp',JSON.stringify(reading));
  assert.equal(engineLabel(observed({status:{backend:{command:['/usr/bin/copilot']}}})),null);
  // Inherited object keys are ordinary names, not entries in the known-engine list.
  for(const name of ['constructor','toString','__proto__','hasOwnProperty']){
    assert.equal(engineLabel(observed({hosted:{name}})),name);
    assert.equal(engineLabel(observed({status:{backend:{command:'/opt/engines/'+name}}})),'Configured runtime: '+name);
  }
  // Portraits follow the saved agent, never the engine it reports.
  const plain=project({endpoints:[endpoint(A,'Scribe')]}).agents[0];assert.match(plain.portrait,/^robot-\d$/);
  for(const name of ['maker','coach','rook','copilot','agent-runtime'])assert.equal(project({endpoints:[{...observed({status:{engine:{name}},hosted:{name,title:name}}),label:'Scribe'}]}).agents[0].portrait,plain.portrait);
});
