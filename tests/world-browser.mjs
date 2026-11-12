// Real disposable Observatory + TLS installation fixture. Never uses live homes or services.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import https from 'node:https';
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory');
const artifacts=process.env.OBSERVATORY_QA_DIR||path.join(root,'private','world-browser');
const home=await fs.mkdtemp(path.join(os.tmpdir(),'observatory-world-'));
const checks=[],errors=[],consoleErrors=[],network=[],httpErrors=[],captures=[],instances=new Map();
let child,browser,fixture,context,base,token,page,config,peer;
const pass=name=>{checks.push(name);console.log('PASS '+name);};
const clone=value=>JSON.parse(JSON.stringify(value));
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const sameCamera=(a,b)=>['x','y','scale'].every(key=>Math.abs(a[key]-b[key])<1e-8);
const registeredEndpoints=endpoints=>endpoints.map(({observation,acp_observation,...registered})=>registered);
const endpointConfig=e=>Object.fromEntries(['label','kind','host_id','acp','management','ui_url','notes'].filter(k=>e[k]!==undefined).map(k=>[k,e[k]]));
async function until(fn,label,timeout=8000){const end=Date.now()+timeout;while(Date.now()<end){if(await fn())return;await delay(80)}throw new Error('Timed out: '+label)}
function catalogFor(instance){return{schema:1,services:[{id:'minecraft',label:'Minecraft',enrolled:true,setup_url:null,add:null},{id:'slack',label:'Slack',enrolled:true,setup_url:null,add:null}],connections:clone(instance.connections),observed_at:Date.now(),helper_connected:instance.helperConnected,request:clone(instance.pending),last_operation:clone(instance.last)};}
function finish(instance){assert(instance.pending,'Expected a queued fixture operation');const op=instance.pending;const connection=instance.connections.find(c=>c.id===op.command.connection_id);connection.state=op.command.action==='connect'?'connected':'disconnected';connection.desired_state=connection.state;connection.context_active=connection.state==='connected';connection.settled=true;connection.request_id=null;instance.last={...op,finished_at:Date.now(),result:{state:connection.state,settled:true,failed:false}};instance.pending=null;}
async function api(resource,body,method='POST'){
 const response=await context.request.fetch(base+'/api/v1/'+resource,{method,headers:{Authorization:'Bearer '+token},...(body===undefined?{}:{data:body})});
 const value=await response.json();assert(response.ok(),`${method} ${resource}: ${JSON.stringify(value)}`);return value;
}
async function screenshot(name){await fs.mkdir(artifacts,{recursive:true});const target=path.join(artifacts,name);await page.screenshot({path:target,fullPage:true});captures.push(target);}
async function currentView(){const id=await page.evaluate(()=>sessionStorage.getItem('observatory-world-view'));assert(id,'Expected durable view ID');return api('views/'+id,undefined,'GET');}
async function closeDialog(){await page.keyboard.press('Escape');await page.locator('#ow-dialog').waitFor({state:'hidden'});}
async function waitFocus(key){await page.waitForFunction(key=>document.activeElement?.dataset.key===key,key);}
async function pendingJournal(p=page){return p.evaluate(()=>{const prefix='observatory-operations:'+location.origin+':fixture-owner:';return Object.keys(localStorage).filter(k=>k.startsWith(prefix)).map(k=>JSON.parse(localStorage.getItem(k))).filter(j=>j&&!j.settled&&j.body?.request_id&&!localStorage.getItem(prefix+'done:'+j.body.request_id)).map(j=>j.body.request_id).sort()})}
try{
 execFileSync(binary,['--home',home,'init','--owner','fixture-owner','--bind','127.0.0.1'],{stdio:'pipe'});
 config=JSON.parse(await fs.readFile(path.join(home,'config.json'),'utf8'));
 token=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim();
 const cookie='world-browser-fixture-cookie';
 execFileSync('python3',['-c',"import sqlite3,hashlib,sys,time; db=sqlite3.connect(sys.argv[1]); db.execute('INSERT INTO browser_sessions VALUES(?,?,?,?)',(hashlib.sha256(sys.argv[2].encode()).hexdigest(),'fixture-owner','fixture-csrf',int(time.time()*1000)+3600000)); db.commit()",path.join(home,'state.sqlite3'),cookie]);
 fixture=https.createServer({key:await fs.readFile(config.tls_key),cert:await fs.readFile(config.tls_cert)},async(req,res)=>{
  const instance=instances.get(req.headers.authorization);
  if(!instance){res.writeHead(401);res.end('{}');return;}
  let body='';for await(const chunk of req)body+=chunk;
  let value;
  if(req.url==='/api/v1/descriptor')value={instance_id:instance.id,implementation:{name:'agent-instance',version:'fixture'},capabilities:['constellation.service-connections/1'],operations:[{path:'service-connections',methods:['GET','POST'],scope:'admin'}]};
  else if(req.url==='/api/v1/status')value={instance_id:instance.id,name:instance.name,version:'fixture',enabled:true,setup_complete:true,platform:'linux',backend:{kind:'Constellation'},counts:{sessions:0,connections:0}};
  else if(req.url==='/api/v1/service-connections'&&req.method==='GET'){instance.gets++;value=catalogFor(instance);}
  else if(req.url==='/api/v1/service-connections'&&req.method==='POST'){
   const command=JSON.parse(body);instance.posts.push(command);
   const old=[instance.pending,instance.last].find(op=>op?.command.request_id===command.request_id);
   if(old){if(JSON.stringify(old.command)!==JSON.stringify(command)){res.writeHead(409);res.end('{"error":"request_id_reused"}');return;}value=old;}
   else{
    const connection=instance.connections.find(c=>c.id===command.connection_id&&c.revision===command.revision);
    if(!connection||instance.pending||!['connect','disconnect'].includes(command.action)){res.writeHead(409);res.end('{"error":"connection_changed"}');return;}
    assert.deepEqual(Object.keys(command).sort(),['action','connection_id','request_id','revision']);assert.match(command.request_id,/^[0-9a-f-]{36}$/);
    instance.pending={command,created_at:Date.now(),finished_at:null,result:null};value=instance.pending;
    // Committed outcome with a lost transport acknowledgement. It must never cause a second action POST.
    if(instance.loseNextAck){instance.loseNextAck=false;res.socket.destroy();return;}
   }
  }else{res.writeHead(404);res.end('{}');return;}
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify(value));
 });
 await new Promise(resolve=>fixture.listen(0,'127.0.0.1',resolve));
 const origin=`https://127.0.0.1:${fixture.address().port}`;
 child=spawn(binary,['--home',home,'serve'],{stdio:'ignore'});
 browser=await chromium.launch({headless:true,args:['--no-sandbox'],...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH}:{})});
 context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1120,height:1050},reducedMotion:'reduce'});base=`https://127.0.0.1:${config.port}`;
 await until(async()=>{try{return(await context.request.get(base+'/healthz')).ok()}catch{return false}},'Observatory startup');
 const hostA=await api('hosts',{label:'Spark',transport:'local',install_roots:[home],recipes:[]});
 const hostB=await api('hosts',{label:'My laptop',transport:'ssh',address:'fixture-laptop',install_roots:[home],recipes:[]});
 const hostC=await api('hosts',{label:'Studio',transport:'ssh',address:'fixture-studio',install_roots:[home],recipes:[]});
 const names=['Maker','Coach','Rook','Scribe','Probe','Atlas','Nova','Pip','Echo','Scout','Bolt','Mica','Lumi'];
 const endpoints=[];
 for(const [index,name]of names.entries()){
  const id=randomUUID(),credential='fixture-installation-'+index;
  const connections=[{id:'minecraft-workshop',service:'minecraft',label:'Workshop',revision:'fixture-minecraft-v1',enrolled:true,settings_url:null,context_provider_ids:['minecraft:'+id],state:'connected',desired_state:'connected',context_active:true,settled:true,failed:false,context_ready:false,context_quiesced:false,request_id:null,message:''},{id:'slack-studio',service:'slack',label:'Studio workspace',revision:'fixture-slack-v1',enrolled:true,settings_url:null,context_provider_ids:['slack:'+id],state:index===12?'disconnected':'connected',desired_state:index===12?'disconnected':'connected',context_active:index!==12,settled:true,failed:false,context_ready:false,context_quiesced:false,request_id:null,message:''}];
  const instance={id,name,connections,pending:null,last:null,gets:0,posts:[],helperConnected:true,loseNextAck:false};instances.set('Bearer '+credential,instance);
  await api('credentials',{name:credential,value:credential});
  const endpoint=await api('endpoints',{label:name,kind:'agent',host_id:index===12?hostB.id:hostA.id,ui_url:origin,management:{provider:'agent-instance',access:'manage',base_url:origin,credential_ref:credential,ca_file:config.tls_ca,expected_id:id}});
  endpoints.push({...endpoint,fixture:instance});await api(`endpoints/${endpoint.id}/inspect`,{});
 }
 const maker=endpoints[0],lumi=endpoints.at(-1);
 const workspace=await api('places',{label:'Studio workspace',service:'slack',kind:'workspace',external_id:'T-FIXTURE-STUDIO',rooms:[],bindings:endpoints.map(e=>({endpoint_id:e.id,connection_id:'slack-studio'}))});
 const workshop=await api('places',{label:'Workshop',service:'minecraft',kind:'server',external_id:'minecraft-fixture-workshop',rooms:[{id:'main',label:'Main hangout'}],bindings:endpoints.map(e=>({endpoint_id:e.id,connection_id:'minecraft-workshop',room_id:'main'}))});
 await context.addCookies([{name:'obs_'+config.id.replaceAll('-',''),value:cookie,url:base,secure:true,httpOnly:true,sameSite:'Strict'}]);
 page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')consoleErrors.push(m.text())});page.on('request',r=>network.push({url:r.url(),method:r.method()}));page.on('response',r=>{if(r.status()>=400)httpErrors.push({url:r.url(),method:r.request().method(),status:r.status()})});
 await page.goto(base);await page.locator('#observatory-world .ow-machine').first().waitFor();
 // Interaction checks continue below once the final product controls are frozen.
 const fixtureContext={hostA,hostB,hostC,endpoints,maker,lumi,workspace,workshop};
 void fixtureContext;
 assert.equal(await page.locator('.ow-machine').count(),3);
 assert.equal(await page.locator(`[data-computer="${hostA.id}"] [data-action="agent"]`).count(),12);
 await screenshot('world-desktop.png');pass('Actual API inventory renders three computers and twelve agents on one computer');

 const ui=page.locator('#observatory-world'),dialog=page.locator('#ow-dialog');
 const visual=()=>page.evaluate(()=>window.observatoryWorldUI.getView());
 async function persisted(){await until(async()=>{const saved=await currentView(),shown=await visual();return JSON.stringify(saved.world.positions)===JSON.stringify(shown.positions)&&sameCamera(saved.world.camera,shown.camera)&&(saved.world.place_id||null)===(shown.currentPlace||null)&&(saved.world.room_id||null)===(shown.currentRoom||null)},'world view saved');}
 async function openAgent(endpoint){await ui.locator(`[data-control="jump"]`).selectOption('machine:'+endpoint.host_id);await ui.locator(`[data-key="agent-${endpoint.id}"]`).click();await dialog.waitFor({state:'visible'});}
 async function refreshAgent(){await dialog.locator('[data-action="refresh-agent"]').last().click();}
 async function enter(place){await ui.locator('[data-control="jump"]').selectOption('place:'+place.id);await ui.locator(`[data-key="enter-${place.id}"]`).click();await page.locator('#ow-inside').waitFor({state:'visible'});}
 async function back(){await ui.locator('[data-action="back"]').click();await page.locator('#ow-viewport').waitFor({state:'visible'});}
 const initial=await visual(),nameplate=ui.locator(`[data-drag-node="machine:${hostA.id}"]`),bb=await nameplate.boundingBox();
 await page.mouse.move(bb.x+bb.width/2,bb.y+bb.height/2);await page.mouse.down();await page.mouse.move(bb.x+bb.width/2+42,bb.y+bb.height/2+22,{steps:8});await page.mouse.up();
 await persisted();const moved=await visual();assert.notDeepEqual(moved.positions['machine:'+hostA.id],initial.positions['machine:'+hostA.id]);
 for(const [key,value]of Object.entries(initial.positions))if(key!=='machine:'+hostA.id)assert.deepEqual(moved.positions[key],value);
 assert.equal(await dialog.isVisible(),false);pass('Dragging a computer changes only its durable position and does not open its popup');
 const handle=ui.locator(`[data-drag-node="machine:${hostB.id}"]`);await handle.focus();const beforeKey=await visual();await handle.press('ArrowRight');await persisted();assert.equal((await visual()).positions['machine:'+hostB.id].x,beforeKey.positions['machine:'+hostB.id].x+15);pass('Keyboard nameplate arrangement uses the same saved view API');
 const blank=await page.evaluate(()=>{const v=document.getElementById('ow-viewport'),r=v.getBoundingClientRect();for(let y=r.top+45;y<r.bottom-70;y+=35)for(let x=r.left+35;x<r.right-35;x+=35){const e=document.elementFromPoint(x,y);if(e&&v.contains(e)&&!e.closest('[data-node],button,select,input'))return{x,y}}return null});assert(blank);
 const beforePan=await visual();await page.mouse.move(blank.x,blank.y);await page.mouse.down();await page.mouse.move(blank.x+24,blank.y+22,{steps:6});await page.mouse.up();await persisted();assert.notDeepEqual((await visual()).camera,beforePan.camera);assert.deepEqual((await visual()).positions,beforePan.positions);
 const vp=await ui.locator('#ow-viewport').boundingBox(),cx=vp.width*.56,cy=vp.height*.44;const beforeZoom=await visual(),point={x:(cx-beforeZoom.camera.x)/beforeZoom.camera.scale,y:(cy-beforeZoom.camera.y)/beforeZoom.camera.scale};await page.mouse.move(vp.x+cx,vp.y+cy);await page.mouse.wheel(0,-110);await until(async()=>Math.abs((await visual()).camera.scale-beforeZoom.camera.scale)>.001,'wheel zoom');await persisted();const zoomed=await visual();assert(Math.abs(point.x*zoomed.camera.scale+zoomed.camera.x-cx)<2);assert(Math.abs(point.y*zoomed.camera.scale+zoomed.camera.y-cy)<2);assert.deepEqual(zoomed.positions,beforeZoom.positions);pass('Canvas pan and pointer-anchored zoom preserve manually placed objects');
 const arranged=clone((await visual()).positions);
 await enter(workspace);assert.match(await ui.locator('#ow-inside').innerText(),/Workspace connections.*Individual channel membership stays in slack/s);assert.equal(await ui.locator('.ow-room-tab').count(),0);
 await ui.locator('[data-action="invite"]').click();
 const lumiBinding=`${lumi.id}:slack-studio`,lumiConnect=`[data-action="connection"][data-binding="${lumiBinding}"]`;
 for(let n=0;n<4&&await dialog.locator(lumiConnect).count()===0;n++)await dialog.getByRole('button',{name:'Next',exact:true}).click();
 const otherLink=clone(lumi.fixture.connections.find(c=>c.service==='minecraft'));
 await dialog.locator(lumiConnect).click();await until(()=>lumi.fixture.posts.length===1,'remote queued Connect');
 assert.equal(lumi.fixture.pending.command.connection_id,'slack-studio');assert.equal(lumi.fixture.connections.find(c=>c.service==='slack').state,'disconnected');assert.deepEqual(lumi.fixture.connections.find(c=>c.service==='minecraft'),otherLink);
 assert.equal(await dialog.locator(lumiConnect).count(),0);assert.match(await dialog.innerText(),/Change requested|Connecting|handling|not confirmed/);
 finish(lumi.fixture);await closeDialog();await ui.locator('[data-action="refresh"]').click();await until(async()=>/13 connected · 13 saved/.test(await ui.locator('#ow-inside').innerText()),'owner reported connected');
 const actualLumi=await api('endpoints/'+lumi.id,undefined,'GET');assert.equal(actualLumi.host_id,hostB.id);assert.equal(actualLumi.management.expected_id,lumi.fixture.id);assert.deepEqual(lumi.fixture.connections.find(c=>c.service==='minecraft'),otherLink);assert.equal(lumi.fixture.posts.length,1);
 await screenshot('inside-workspace.png');pass('Inside workspace Connect queues the exact saved route and waits for owner settlement without moving or copying the agent');
 await back();assert.deepEqual((await visual()).positions,arranged);
 await openAgent(maker);const makerBinding=`${maker.id}:minecraft-workshop`,makerControl=verb=>`[data-action="connection"][data-binding="${makerBinding}"][data-verb="${verb}"]`;
 const makerOther=clone(maker.fixture.connections.find(c=>c.service==='slack'));
 await dialog.locator(makerControl('disconnect')).click();await until(()=>maker.fixture.posts.length===1,'queued Disconnect');assert.equal(maker.fixture.connections.find(c=>c.service==='minecraft').state,'connected');finish(maker.fixture);await refreshAgent();await dialog.locator(makerControl('connect')).waitFor();
 peer=await context.newPage();await peer.goto(base);await peer.locator('.ow-machine').first().waitFor();
 maker.fixture.loseNextAck=true;await dialog.locator(makerControl('connect')).click();await until(()=>maker.fixture.posts.length===2,'one committed Connect before lost ack');
 const lostRequest=maker.fixture.pending.command.request_id;assert.equal(maker.fixture.connections.find(c=>c.service==='minecraft').state,'disconnected');await until(()=>maker.fixture.gets>2,'read-only catalog reconciliation');
 const staleJournalRecord=await page.evaluate(id=>localStorage.getItem('observatory-operations:'+location.origin+':fixture-owner:'+id),lostRequest);assert(staleJournalRecord);
 await peer.locator('[data-action=refresh]').click();await until(async()=>(await pendingJournal(peer)).includes(lostRequest),'other tab retains original pending request');
 await peer.locator('[data-control=jump]').selectOption('machine:'+lumi.host_id);await peer.locator(`[data-key="agent-${lumi.id}"]`).click();const peerDialog=peer.locator('#ow-dialog');await peerDialog.locator(`[data-action=connection][data-binding="${lumi.id}:minecraft-workshop"][data-verb=disconnect]`).click();await until(()=>lumi.fixture.posts.length===2,'second tab independent request');const peerRequest=lumi.fixture.pending.command.request_id;assert.deepEqual(await pendingJournal(peer),[lostRequest,peerRequest].sort());
 await page.reload();await ui.locator('.ow-machine').first().waitFor();await openAgent(maker);assert.equal(maker.fixture.posts.length,2);assert.equal(maker.fixture.pending.command.request_id,lostRequest);assert.equal(await dialog.locator(makerControl('connect')).count(),0);
 finish(maker.fixture);await refreshAgent();await dialog.locator(makerControl('disconnect')).waitFor();assert.equal(maker.fixture.posts.length,2);assert.deepEqual(maker.fixture.connections.find(c=>c.service==='slack'),makerOther);
 assert.deepEqual(await pendingJournal(),[peerRequest]);
 // Reproduce a stale tab writing the old request after another tab commits settlement.
 await peer.evaluate(({id,record})=>localStorage.setItem('observatory-operations:'+location.origin+':fixture-owner:'+id,record),{id:lostRequest,record:staleJournalRecord});
 await until(async()=>!(await pendingJournal()).includes(lostRequest),'late stale write cannot resurrect a settled request');await dialog.locator(makerControl('disconnect')).waitFor();assert.equal(maker.fixture.posts.length,2);
 await peerDialog.locator('[data-action=refresh-agent]').last().click();await until(async()=>!(await pendingJournal(peer)).includes(lostRequest),'settled tombstone cannot be resurrected');assert.deepEqual(await pendingJournal(peer),[peerRequest]);finish(lumi.fixture);await peerDialog.locator('[data-action=refresh-agent]').last().click();await until(async()=>(await pendingJournal(peer)).length===0,'independent request settled');assert.equal(lumi.fixture.posts.length,2);await peer.close();peer=null;await page.bringToFront();pass('Independent tabs preserve both pending requests and a late stale journal write cannot resurrect settlement');
 await screenshot('agent-popup.png');pass('Lost committed acknowledgement survives reload and reconciles by status with exactly one Connect POST and unchanged other memberships');
 maker.fixture.helperConnected=false;await refreshAgent();await until(async()=>/helper offline/.test(await dialog.innerText()),'helper offline status rendered');assert.match(await dialog.innerText(),/Last seen connected · helper offline/);assert.equal(await dialog.locator('[data-action="connection"]').count(),0);maker.fixture.helperConnected=true;await refreshAgent();await dialog.locator(makerControl('disconnect')).waitFor();pass('Offline helper shows stale observation without claiming the connection stopped or offering a blind retry');
 await closeDialog();await waitFocus('agent-'+maker.id);
 await openAgent(maker);await dialog.getByRole('button',{name:'Assign computer',exact:true}).click();await dialog.getByLabel(/^Computer/).selectOption(hostB.id);
 const beforePlacement=await api('endpoints/'+maker.id,undefined,'GET'),lumiBeforePlacement=await api('endpoints/'+lumi.id,undefined,'GET');
 const concurrent=await api('endpoints/'+maker.id,{...endpointConfig(beforePlacement),label:'Maker edited elsewhere',expected_revision:beforePlacement.revision},'PUT');
 await dialog.getByRole('button',{name:'Save placement',exact:true}).click();await until(async()=>/This record changed; refresh before saving/.test(await dialog.locator('#ow-form-error').innerText()),'stale placement refused');
 assert.deepEqual(registeredEndpoints([await api('endpoints/'+maker.id,undefined,'GET')]),registeredEndpoints([concurrent]));assert.deepEqual(registeredEndpoints([await api('endpoints/'+lumi.id,undefined,'GET')]),registeredEndpoints([lumiBeforePlacement]));assert.equal(maker.fixture.posts.length,2);
 const placementRefusalUrl=base+'/api/v1/endpoints/'+maker.id;assert.equal(httpErrors.filter(e=>e.status===409&&e.method==='PUT'&&e.url===placementRefusalUrl).length,1);
 await closeDialog();await api('endpoints/'+maker.id,{...endpointConfig(concurrent),label:'Maker',expected_revision:concurrent.revision},'PUT');await ui.locator('[data-action="refresh"]').click();pass('A stale placement form cannot overwrite concurrent endpoint edits or change another agent');
 const beforeAdd=clone((await visual()).positions);await ui.locator('[data-key="add"]').click();await dialog.locator('[data-action="create-machine"]').click();await dialog.getByLabel('Computer name',{exact:true}).fill('Office desktop');await dialog.getByLabel(/^Connection/).selectOption('local');await dialog.getByLabel('Computer name',{exact:true}).press('Enter');await dialog.waitFor({state:'hidden'});await until(async()=>(await api('hosts',undefined,'GET')).hosts.length===4,'saved actual computer record');
 for(const [key,value]of Object.entries(beforeAdd))assert.deepEqual((await visual()).positions[key],value);assert.equal((await api('jobs',undefined,'GET')).jobs.length,0);pass('Keyboard computer creation saves a real record without installation or shuffling existing positions');
 await ui.locator('[data-key="add"]').click();await dialog.locator('[data-action="create-agent"]').click();assert.match(await dialog.innerText(),/No installer is enabled/);assert.equal(await dialog.getByRole('button',{name:'Review installation',exact:true}).count(),0);await screenshot('no-installer-popup.png');await closeDialog();pass('Create agent truthfully exposes missing installer configuration without a simulated success');
 await ui.locator('[data-action="fit"]').click();await persisted();const retained=await visual();const registryBefore=await api('endpoints',undefined,'GET');await page.reload();await ui.locator('.ow-machine').first().waitFor();await until(async()=>JSON.stringify((await visual()).positions)===JSON.stringify(retained.positions),'positions after reload');assert(sameCamera((await visual()).camera,retained.camera));assert.deepEqual(registeredEndpoints((await api('endpoints',undefined,'GET')).endpoints),registeredEndpoints(registryBefore.endpoints));pass('Reload restores manual positions/camera while registry identities remain unchanged');
 await enter(workshop);assert.equal(await ui.locator('.ow-room-tab').count(),0);assert.match(await ui.locator('#ow-inside').innerText(),/Agents connected to this server/);await screenshot('inside-minecraft.png');await back();await persisted();
 // A remote controller must still be able to navigate the shared saved view after local gestures.
 const remote=await currentView();await api(`views/${remote.id}/commands`,{expected_revision:remote.revision,controller:'fixture-controller',commands:[{op:'set',field:'world',value:{...remote.world,place_id:workspace.id,room_id:'connections',navigation:'place'}}]});
 await page.waitForFunction(id=>window.observatoryWorldUI.getView().currentPlace===id,workspace.id,{timeout:40000});pass('A newer programmatic view revision applies after local gestures');
 await page.setViewportSize({width:352,height:1000});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await screenshot('world-mobile-inside.png');await ui.locator('[data-action="invite"]').click();assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await screenshot('world-mobile-invite.png');await closeDialog();await waitFocus('invite');await back();await ui.locator('[data-action="fit"]').click();await screenshot('world-mobile-canvas.png');assert(await page.evaluate(()=>{const v=document.querySelector('#ow-viewport').getBoundingClientRect();return [...document.querySelectorAll('[data-node]')].every(n=>{const r=n.getBoundingClientRect();return r.left>=v.left-1&&r.right<=v.right+1&&r.top>=v.top-1&&r.bottom<=v.bottom+1})}),'Fit must show every computer and place on the narrow canvas');pass('352px inside view, invitation and canvas controls remain usable without horizontal overflow');
 const imageFailures=await page.evaluate(async()=>{const images=[...document.images];await Promise.all(images.map(i=>i.decode().catch(()=>{})));return images.filter(i=>!i.complete||i.naturalWidth===0).map(i=>i.src)});assert.deepEqual(imageFailures,[]);
 const art=await context.request.get(base+'/world-assets/club.webp');assert(art.ok());assert.match(art.headers()['content-type'],/image\/webp/);
 assert(network.every(r=>new URL(r.url).origin===base),JSON.stringify(network.filter(r=>new URL(r.url).origin!==base)));assert.deepEqual(errors,[]);
 assert.deepEqual(httpErrors.filter(e=>!(e.status===502&&e.url.endsWith('/operations/service-connection-action'))&&!(e.status===409&&e.method==='PUT'&&e.url===placementRefusalUrl)),[]);
 assert.deepEqual(consoleErrors.filter(e=>!/Failed to load resource: the server responded with a status of (502|409)/.test(e)),[]);pass('Local art/font assets load under production CSP with no external requests or script errors');
 await page.goto(base+'/advanced');await page.getByRole('button',{name:'Agents',exact:true}).waitFor();assert.equal(await page.locator('#observatory-world').count(),0);pass('The original advanced control surface remains accessible');
 await fs.mkdir(artifacts,{recursive:true});await fs.writeFile(path.join(artifacts,'result.json'),JSON.stringify({passed:true,checks,screenshots:captures,remote_action_count:[...instances.values()].reduce((n,i)=>n+i.posts.length,0),scope:'Disposable TLS Observatory and simulated installation-owned settlement. No live agent, session, model, domain or installer operation.'},null,2)+'\n');

 console.log(JSON.stringify({passed:true,checks,screenshots:captures}));
}catch(error){
 await fs.mkdir(artifacts,{recursive:true});let observed={};try{observed={saved:await currentView(),shown:await page.evaluate(()=>window.observatoryWorldUI?.getView()),status:await page.locator('#ow-message').innerText()};await screenshot('failure.png')}catch{}
 await fs.writeFile(path.join(artifacts,'failure.json'),JSON.stringify({error:String(error),stack:error.stack,checks,observed,errors,consoleErrors,httpErrors},null,2)+'\n');throw error;
}finally{
 await browser?.close();
 if(child){child.kill();await Promise.race([new Promise(resolve=>child.once('exit',resolve)),delay(1500)]);}
 fixture?.closeAllConnections();await new Promise(resolve=>fixture?fixture.close(resolve):resolve());
 await fs.rm(home,{recursive:true,force:true});
}
