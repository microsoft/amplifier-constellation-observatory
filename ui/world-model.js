// Display projection only. Installations own every connection state and operation.
export const UNASSIGNED='00000000-0000-0000-0000-000000000000';
export const bindingKey=(endpoint,connection)=>`${endpoint}:${connection}`;
export function appearance(endpoint){
  const name=String(endpoint.label||'').toLowerCase();
  if(['maker','coach','rook'].includes(name))return name;
  let hash=0;for(const c of endpoint.id)hash=(hash*31+c.charCodeAt(0))>>>0;
  return 'robot-'+(hash%9);
}
export function safeUrl(raw){try{const u=new URL(raw);return u.protocol==='https:'&&!u.username&&!u.password?u.href:null}catch{return null}}
export function connectionStatus(catalog,connection,error){
  if(error)return {label:'Unavailable',connected:false,uncertain:true};
  if(!connection||connection.enrolled===false)return {label:'Not available',connected:false,uncertain:true};
  const request=catalog?.request;
  if(!catalog?.helper_connected)return {label:`Last seen ${connection.state||'unknown'} · helper offline`,connected:false,uncertain:true};
  if(request?.command?.connection_id===connection.id)return {label:request.command.action==='disconnect'?'Disconnecting…':'Connecting…',connected:false,pending:true,uncertain:connection.state==='unknown'};
  if(connection.failed)return {label:connection.message||'Change failed',connected:false,failed:true};
  if(connection.state==='connected')return {label:'Connected',connected:true};
  if(connection.state==='disconnected'||connection.state==='offline')return {label:'Disconnected',connected:false};
  return {label:connection.state==='unknown'?'Outcome unknown':connection.state||'Not checked',connected:false,uncertain:true};
}
export function project({endpoints=[],hosts=[],places=[],catalogs={}}){
  const hostIds=new Set(hosts.map(h=>h.id));
  const agents=endpoints.filter(e=>e.kind==='agent'||!e.kind).map(e=>({id:e.id,name:e.label,machineId:hostIds.has(e.host_id)?e.host_id:UNASSIGNED,portrait:appearance(e),endpoint:e,engine:e.observation?.status?.engine?.name||e.observation?.status?.backend?.kind||null}));
  for(const a of agents){if(!a.engine){const command=a.endpoint.observation?.status?.backend?.command;const binary=typeof command==='string'?command.split('/').pop():null;a.engine=({'agent-runtime':'Constellation Native','amplifier-agent-acp':'Amplifier Agent','claude-agent-acp':'Claude ACP','codex-acp':'Codex ACP','copilot':'GitHub Copilot'})[binary]||(binary?`Configured runtime: ${binary}`:null)}}
  const known=new Set(agents.map(a=>a.id));
  const machines=hosts.map(h=>({id:h.id,name:h.label,platform:h.transport==='local'?'Observatory computer':'Configured SSH access',host:h}));
  if(agents.some(a=>a.machineId===UNASSIGNED))machines.push({id:UNASSIGNED,name:'Other agents',platform:'Computer not assigned',unassigned:true});
  const result=places.map(p=>({...p,name:p.label,brand:p.service,rooms:p.rooms?.length?p.rooms:[{id:'connections',label:p.kind==='workspace'?'Workspace connections':'Connections'}],bindings:[],configured:true}));
  const assignments=new Map();for(const p of places)for(const b of p.bindings||[])if(known.has(b.endpoint_id))assignments.set(bindingKey(b.endpoint_id,b.connection_id),{placeId:p.id,roomId:b.room_id||null});
  const bindings=[];
  for(const a of agents){
    const read=catalogs[a.id]||{},cat=read.data;
    const connections=new Map((cat?.connections||[]).map(c=>[c.id,c]));
    for(const p of places)for(const b of p.bindings||[])if(b.endpoint_id===a.id&&!connections.has(b.connection_id))connections.set(b.connection_id,null);
    for(const [id,c] of connections){
      const key=bindingKey(a.id,id),assignment=assignments.get(key);let p=assignment&&result.find(p=>p.id===assignment.placeId);
      const mismatch=!!(p&&c&&p.service!==c.service);
      if(mismatch)p=null; // Surface the changed service separately; do not claim the old assignment.
      if(!p){const pid='connection:'+key;p={id:pid,name:c?.label||id,service:c?.service||'other',brand:c?.service||'other',kind:c?.service==='minecraft'?'server':'experience',rooms:[{id:'connections',label:'Connections'}],bindings:[],configured:false};result.push(p)}
      const error=mismatch?'The saved connection now reports a different service. Review its place assignment.':read.error;
      const status=connectionStatus(cat,mismatch?null:c,error),endpoint=a.endpoint;
      const b={key,agentId:a.id,connectionId:id,placeId:p.id,roomId:(!mismatch&&assignment?.roomId)||p.rooms[0]?.id||'connections',connection:c,catalog:cat,status,checkedAt:read.checkedAt||null,error:error||null,canManage:!!(!mismatch&&c&&c.enrolled!==false&&cat?.helper_connected&&!cat?.request&&!read.error&&endpoint.management?.access==='manage'&&endpoint.management?.expected_id)};
      bindings.push(b);p.bindings.push(b);
    }
  }
  return {agents,machines,places:result,bindings};
}
export function operationMatch(catalog,requestId){
  for(const op of [catalog?.request,catalog?.last_operation])if(op?.command?.request_id===requestId)return op;
  return null;
}
export function defaultLayout(model){
  const positions={};
  model.machines.forEach((m,i)=>{positions['machine:'+m.id]={x:40+(i%2)*630,y:40+Math.floor(i/2)*620}});
  const base=model.machines.length>1?1350:720;
  model.places.forEach((p,i)=>{positions['place:'+p.id]={x:base+(i%3)*300,y:70+Math.floor(i/3)*330}});
  return positions;
}
