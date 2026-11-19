// Combined consumer journey through the real Observatory desktop/world UI.
// Real: disposable native app/account, endpoint registry, browser navigation,
// model/Start/Slack UI modules, saved browser intents, and Observatory view state.
// Synthetic: completed OS install job, Instance operations and Slack authorization/
// package/deploy boundary. No provider/model, Slack network, OS manager or live home.
// Run with an exact built/extracted OBSERVATORY_BINARY and private Playwright/Chromium.
// The application child gets an OS-only PATH; this runner needs no Python/Git/npm CLI.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {createHash, randomUUID} from 'node:crypto';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=path.resolve(process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory'));
const artifacts=process.env.OBSERVATORY_QA_DIR||path.join(root,'private','consumer-onboarding-browser');
const temporary=await fs.mkdtemp(path.join(os.tmpdir(),'observatory consumer journey '));
const home=path.join(temporary,'fresh-observatory');
const clone=value=>structuredClone(value),sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const osPath=process.platform==='win32'?`${process.env.SystemRoot||'C:\\Windows'}\\System32;${process.env.SystemRoot||'C:\\Windows'}`:process.platform==='darwin'?'/usr/bin:/bin:/usr/sbin:/sbin':'/usr/bin:/bin';
const childEnv={...process.env};for(const key of Object.keys(childEnv))if(key.toLowerCase()==='path')delete childEnv[key];childEnv.PATH=osPath;
const checks=[],errors=[],network=[],operations=[],slackPosts=[],installPosts=[],instances=new Map(),slackRecords=new Map(),jobs=new Map();
let browser,context,page,child,base,token,host,newAgent,existingAgent,installed=false,plan=null;
let dropModel=false,refuseModel=false,dropFinish=false,dropStart=false,dropPrepare=false,dropConnect=false,slackUnavailable=false,mismatch=false,refuseSlack=false;
let stdout='',stderr='',passed=false;
const pass=label=>{checks.push(label);console.log('PASS '+label)};
const ids=['config','apply-config','status','lifecycle','backend','native-config','native-schema','validate-native-config','apply-native-config','native-account','native-login','native-credential','service-connections'];
const runtimeOriginal={provider:{kind:'openai',model:'owner-selected',endpoint:'https://api.openai.com/v1/responses',api_key_env:'MODEL_KEY',auth:'auto',native:{tools:[{type:'preserved'}]}},mode:'ask',instructions:'Keep existing instructions',limits:{turns:7}};
function instance(id,ready=false){return{identity:id,document:{revision:1,config:{instance_id:id,owner:'fixture-owner',setup_complete:ready,enabled:ready,name:ready?'Existing agent':'Notebook',backend:{command:'/private/selected-runtime',args:['--fixed'],env:{OWNER_CHOICE:'preserved'}},sessions:['existing-session'],remembered:{approval:'owner-selected'},workspace:{root:'/private/selected-work'}}},runtime:clone(runtimeOriginal),runtimeRevision:'a'.repeat(64),backendHash:'b'.repeat(64),authReady:ready};}
function freshSlack(endpoint){return{schema:1,endpoint_id:endpoint.id,endpoint_revision:endpoint.revision,revision:0,operation:null,connector:{},setup:{authorization:'missing',next_action:'authorize'},capabilities:{can_prepare:true}};}
const send=(route,value,status=200)=>route.fulfill({status,contentType:'application/json',body:JSON.stringify(value)});
const lost=route=>route.fulfill({status:200,contentType:'application/json',body:'{"reply_lost":'});
async function api(resource,body,method='POST'){
 const response=await context.request.fetch(base+'/api/v1/'+resource,{method,headers:{Authorization:'Bearer '+token},...(body===undefined?{}:{data:body})});
 const value=await response.json();assert(response.ok(),`${method} ${resource}: ${JSON.stringify(value)}`);return value;
}
async function stop(){if(!child||child.exitCode!==null||child.signalCode!==null)return;const exited=once(child,'exit');child.kill();await exited;}
async function start(){
 child=spawn(binary,['--home',home,'--no-browser'],{env:childEnv,stdio:['ignore','pipe','pipe']});
 child.stdout.on('data',bytes=>stdout+=bytes);child.stderr.on('data',bytes=>stderr+=bytes);
 const lines=createInterface({input:child.stdout});let timer;
 try{return await Promise.race([once(lines,'line').then(([line])=>JSON.parse(line)),once(child,'exit').then(()=>{throw Error('Native app exited before setup: '+stderr)}),new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Native setup readiness was not returned')),30000);timer.unref();})]);}finally{clearTimeout(timer);lines.close();}
}
async function operation(route,endpoint,op){
 const request=route.request(),body=request.postDataJSON()||{},f=instances.get(endpoint.id);
 if(op==='service-connections'){assert.deepEqual(body,{});return send(route,{schema:1,services:[],connections:[],helper_connected:true,request:null,last_operation:null,observed_at:Date.now()});}
 assert.equal(body._observatory_endpoint_revision,endpoint.revision);delete body._observatory_endpoint_revision;
 // Never record write-only fixture keys in retained evidence.
 operations.push({endpoint_id:endpoint.id,op,body:op==='native-credential'?{expected_backend_sha256:body.expected_backend_sha256,value:'[write-only fixture]'}:clone(body)});
 switch(op){
  case 'config':return send(route,f.document);
  case 'status':return send(route,{instance_id:f.identity,config_revision:f.document.revision,setup_complete:f.document.config.setup_complete,enabled:f.document.config.enabled});
  case 'backend':return send(route,{kind:'native',name:'Constellation',installed:true,configured:true,auth_ready:f.authReady,auth_validated:false});
  case 'native-config':return send(route,{config:f.runtime,revision:f.runtimeRevision,backend_sha256:f.backendHash,credential_mapping:{}});
  case 'native-schema':return send(route,{'x-agent-runtime':{provider_presets:[runtimeOriginal.provider]}});
  case 'native-account':return send(route,{account:{connected:false},login:{state:'idle'}});
  case 'native-login':return send(route,{state:'idle'});
  case 'validate-native-config':return send(route,{valid:true});
  case 'apply-native-config':
   assert.equal(body.expected_revision,f.runtimeRevision);assert.equal(body.expected_backend_sha256,f.backendHash);
   if(refuseModel){refuseModel=false;return send(route,{error:'Settings changed; refresh before saving.'},409);}
   f.runtime=clone(body.config);f.runtimeRevision='c'.repeat(64);f.backendHash='d'.repeat(64);
   if(dropModel){dropModel=false;return lost(route);}return send(route,{revision:f.runtimeRevision});
  case 'native-credential':assert.equal(body.value,'fixture-write-only-key');assert.equal(body.expected_backend_sha256,f.backendHash);f.authReady=true;return send(route,{configured:true,value_returned:false});
  case 'apply-config':
   assert.equal(body.expected_revision,f.document.revision);assert.deepEqual(body.config,{...f.document.config,setup_complete:true});
   f.document={revision:f.document.revision+1,config:clone(body.config)};
   if(dropFinish){dropFinish=false;return lost(route);}return send(route,f.document);
  case 'lifecycle':
   assert.deepEqual(body,{action:'start'});assert(f.document.config.setup_complete);f.document={revision:f.document.revision+1,config:{...f.document.config,enabled:true}};
   if(dropStart){dropStart=false;return lost(route);}return send(route,{enabled:true});
  default:throw Error('Unexpected Instance operation '+op);
 }
}
async function slack(route,endpoint){
 const request=route.request(),current=slackRecords.get(endpoint.id);
 if(request.method()==='GET')return slackUnavailable?send(route,{error:'Fixture observation unavailable'},503):send(route,current);
 const body=request.postDataJSON();slackPosts.push({...clone(body),...(body.challenge?{challenge:'[ephemeral fixture]'}:{})});
 assert.equal(body.expected_revision,current.revision);assert.equal(body.endpoint_revision,endpoint.revision);
 if(body.action==='prepare'){
  assert.equal(current.operation,null);assert.match(body.request_id,/^[0-9a-f-]{36}$/);
  current.operation={id:body.request_id,request_id:body.request_id,phase:'ready',state:'complete',uncertain:false};
  current.connector={id:'fixture-new-slack',label:'Notebook in Slack',endpoint_id:endpoint.id,binding_verified:true,account_ready:true};current.capabilities={can_authorize:true};current.revision++;
  if(dropPrepare){dropPrepare=false;return lost(route);}return send(route,current);
 }
 assert.equal(body.operation_id,current.operation.id);
 if(body.action==='authorize'){
  current.revision++;current.setup={...current.setup,next_action:'confirm',authorization_pending:true};current.capabilities={can_confirm:true};
  current.authorization_command={command:'/slackauthticket ephemeral-fixture-ticket',expires_at:Date.now()+600000};return send(route,current);
 }
 if(body.action==='confirm'){
  // A concurrent Observatory observation rejects this old revision before any
  // external confirmation. This is not a simulated known Slack code failure.
  if(refuseSlack){refuseSlack=false;current.revision++;return send(route,{schema:1,kind:'slack_setup_refusal',refused_before_intent:true,endpoint_id:endpoint.id,endpoint_revision:endpoint.revision,action:body.action,request_id:null,operation_id:body.operation_id,expected_revision:body.expected_revision,code:'setup_changed',message:'Saved setup changed before this step.'});}
  assert.equal(body.challenge,'fixture-confirmation');current.revision++;delete current.authorization_command;
  current.setup={...current.setup,authorization:'managed',authorization_pending:false,authorized_workspace:{team_id:mismatch?'TOTHER':'TWORKSHOP',domain:mismatch?'other-workspace':'workshop'},intended_workspace:{team_id:'TWORKSHOP',name:'Workshop'},workspace_mismatch:mismatch,next_action:mismatch?'authorize':'connect'};
  current.capabilities=mismatch?{can_authorize:true}:{can_connect:true,can_discard:true};return send(route,current);
 }
 if(body.action==='connect'){
  assert.equal(body.team_id,'TWORKSHOP');assert(!current.setup.workspace_mismatch);assert.equal(current.connector.setup_complete,undefined);
  current.revision++;current.connector={...current.connector,connection_ready:true,connection_running:true,setup_complete:true};current.setup.next_action='done';current.capabilities={};
  if(dropConnect){dropConnect=false;return lost(route);}return send(route,current);
 }
 throw Error('Unexpected Slack action '+body.action);
}
async function routeBoundary(route){
 const request=route.request(),url=new URL(request.url()),pathname=url.pathname,method=request.method();
 if(url.origin!==base){errors.push('Unexpected external request '+url.origin+pathname);await route.abort();return;}
 try{
  if(pathname==='/api/v1/hosts'&&method==='GET'){
   const response=await route.fetch(),value=await response.json();value.hosts=value.hosts.map(h=>h.id===host?.id?{...h,recipes:[{id:'bundled-instance',version:'fixture-pinned',executable:binary,args:[],timeout_seconds:300}]}:h);return send(route,value);
  }
  if(pathname==='/api/v1/endpoints'&&method==='GET'){
   const response=await route.fetch(),value=await response.json();value.endpoints=value.endpoints.filter(e=>installed||e.id!==newAgent?.id);return send(route,value);
  }
  if(pathname==='/api/v1/install/plan'){
   const body=request.postDataJSON();assert.equal(body.recipe_id,'bundled-instance');assert.equal(body.host_id,host.id);
   plan={...body,version:'fixture-pinned',home:path.join(home,'synthetic-agent-not-created'),plan_digest:'e'.repeat(64)};return send(route,plan);
  }
  if(pathname==='/api/v1/install'){
   const body=request.postDataJSON();assert.equal(body.plan_digest,plan.plan_digest);assert.equal(body.request_id,plan.request_id);assert.equal(installPosts.length,0);installPosts.push(body);installed=true;
   jobs.set(body.request_id,{id:body.request_id,status:'completed',endpoint_id:newAgent.id,plan,note:'Deterministic OS boundary: completed private install result.'});
   return lost(route); // Committed synthetic install result; recovery may only read it.
  }
  if(pathname==='/api/v1/jobs')return send(route,{jobs:[...jobs.values()]});
  if(pathname.startsWith('/api/v1/jobs/')){assert.equal(method,'GET');const job=jobs.get(pathname.split('/').at(-1));assert(job);return send(route,job);}
  if(pathname==='/api/v1/slack-setups')return send(route,{slack_setups:[...slackRecords.values()].filter(s=>s.operation)});
  const match=pathname.match(/^\/api\/v1\/endpoints\/([^/]+)\/(inspect|slack-setup|operations\/([^/]+))$/);
  if(match){
   const endpoint=[newAgent,existingAgent].find(e=>e?.id===match[1]);assert(endpoint,'Unknown fixture endpoint');
   if(match[2]==='inspect')return send(route,{instance_id:endpoint.management.expected_id,operations:ids.map(id=>({id})),status:{enabled:instances.get(endpoint.id).document.config.enabled}});
   if(match[2]==='slack-setup')return slack(route,endpoint);
   return operation(route,endpoint,match[3]);
  }
  return route.continue();
 }catch(error){errors.push(error.stack||String(error));return send(route,{error:'Fixture assertion failed'},500);}
}
async function closePopup(){if(await page.locator('#ow-dialog').isVisible())await page.getByRole('button',{name:'Close popup',exact:true}).click();}
async function openAgent(endpoint=newAgent){await closePopup();await page.locator(`[data-action="agent"][data-agent="${endpoint.id}"]`).first().click();}
async function openModel(reload=false,endpoint=newAgent){if(reload){await page.reload();await page.locator('.ow-top').waitFor();}await openAgent(endpoint);await page.getByRole('button',{name:'Model account',exact:true}).click();await page.locator('[data-setup=save]').waitFor();await page.waitForFunction(()=>!document.querySelector('[data-setup=refresh]')?.disabled);}
async function openSlack(reload=false){if(reload){await page.reload();await page.locator('.ow-top').waitFor();}await openAgent();await page.getByRole('button',{name:'Add a connection',exact:true}).click();await page.locator('[data-action=slack-setup]').click();await page.locator('.ow-slack-setup').waitFor();}
const mutations=()=>operations.filter(c=>['apply-config','lifecycle','apply-native-config','native-credential'].includes(c.op));
try{
 await fs.mkdir(artifacts,{recursive:true});const first=await start();assert.equal(first.state,'setup');base=new URL(first.url).origin;
 browser=await chromium.launch({headless:true,args:['--no-sandbox'],...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH}:{})});
 context=await browser.newContext({viewport:{width:1200,height:1000},reducedMotion:'reduce'});page=await context.newPage();page.setDefaultTimeout(10000);page.on('pageerror',error=>errors.push(error.message));page.on('request',request=>network.push({method:request.method(),path:new URL(request.url()).pathname}));
 await page.goto(first.url);await page.getByLabel('Your name',{exact:true}).fill('Consumer Fixture');await page.getByLabel('Choose an Observatory password',{exact:true}).fill('Disposable consumer browser password');await page.getByLabel('Open Observatory when I sign in').uncheck();
 const setupStarted=performance.now();await page.getByRole('button',{name:'Open my constellation'}).click();
 // Match check-native-launch.py's existing setup HTTP allowance. The generic
 // 10-second locator wait is for screen actions, not initial binary retention.
 await page.locator('.ow-top').waitFor({timeout:20000});
 await fs.writeFile(path.join(artifacts,'setup-timing.json'),JSON.stringify({elapsed_ms:performance.now()-setupStarted,limit_ms:20000,application_binary_sha256:sha(await fs.readFile(binary)),kind:'source-built UI composition; native package proof is separate'},null,2));
 // Fail if a stale binary serves a different UI; no source overlay masks packaging drift.
 const sourceHashes={};for(const name of ['world.js','agent-setup.js','slack-setup.js']){const response=await context.request.get(base+'/'+name);assert(response.ok());const bytes=await response.body(),expected=await fs.readFile(path.join(root,'ui',name));assert.equal(sha(bytes),sha(expected),'Rebuild the exact Observatory UI before this fixture: '+name);sourceHashes[name]=sha(bytes);}

 token=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim();
 host=(await api('hosts',undefined,'GET')).hosts.find(h=>h.native_local);assert(host,'Native desktop must retain its own local host');
 await api('credentials',{name:'fixture-management',value:'synthetic-no-platform-credential',create_only:true});
 async function register(label,ready){const instanceId=randomUUID();const endpoint=await api('endpoints',{label,kind:'agent',host_id:host.id,notes:'Preserve fixture registry metadata',management:{provider:'agent-instance',access:'manage',base_url:'https://127.0.0.1:1',credential_ref:'fixture-management',expected_id:instanceId}});instances.set(endpoint.id,instance(instanceId,ready));return endpoint;}
 existingAgent=await register('Existing colleague',true);newAgent=await register('Notebook',false);
 slackRecords.set(newAgent.id,freshSlack(newAgent));slackRecords.set(existingAgent.id,{...freshSlack(existingAgent),revision:9,operation:{id:randomUUID(),request_id:randomUUID(),state:'complete'},connector:{id:'existing-private-slack',endpoint_id:existingAgent.id,binding_verified:true,account_ready:true,connection_ready:true,connection_running:false,setup_complete:true},capabilities:{}});
 const registryBefore=await api('endpoints',undefined,'GET'),existingBefore=clone(instances.get(existingAgent.id)),oldSlackBefore=clone(slackRecords.get(existingAgent.id));
 const accountBefore=sha(await fs.readFile(path.join(home,'secrets/local-password.json'))),configBefore=sha(await fs.readFile(path.join(home,'config.json')));
 const newInstanceBefore=clone(instances.get(newAgent.id).document.config);
 await context.route('**/*',routeBoundary);await page.reload();await page.locator('.ow-top').waitFor();
 await page.getByRole('button',{name:'+ Add',exact:true}).click();await page.locator('[data-action=create-agent]').filter({hasText:'Create an agent'}).click();await page.getByLabel('Agent name',{exact:true}).fill('Notebook');await page.getByRole('button',{name:'Review installation',exact:true}).click();await page.getByRole('button',{name:'Install agent',exact:true}).click();await page.getByRole('button',{name:'Continue setup',exact:true}).waitFor();assert.equal(installPosts.length,1);await page.getByRole('button',{name:'Continue setup',exact:true}).click();await page.locator('[data-setup=save]').waitFor();
 pass('Real desktop account and main UI continue one synthetic completed install after its lost reply, without reinstalling');
 assert.equal(mutations().length,0);assert.equal(await page.locator('[name=model]').inputValue(),'owner-selected');assert.equal(await page.locator('[data-setup=done]').isEnabled(),false);
 refuseModel=true;await page.locator('[name=model]').fill('explicit-model');await page.locator('[data-setup=save]').click();await page.getByRole('alert').filter({hasText:'Settings changed'}).waitFor();assert.deepEqual(instances.get(newAgent.id).runtime,runtimeOriginal);assert.equal(mutations().length,1);
 await page.locator('[data-setup=refresh]').click();await page.waitForFunction(()=>!document.querySelector('[data-setup=save]')?.disabled);dropModel=true;await page.locator('[name=model]').fill('explicit-model');await page.locator('[data-setup=save]').click();await page.getByText('The last action was not confirmed.',{exact:false}).waitFor();await openModel(true);await page.getByText('Saved settings now match',{exact:false}).waitFor();assert.equal(operations.filter(c=>c.op==='apply-native-config').length,2);assert.deepEqual(instances.get(newAgent.id).runtime,{...runtimeOriginal,provider:{...runtimeOriginal.provider,model:'explicit-model'}});
 pass('Known model CAS refusal preserves settings; explicit later save with lost reply reconciles after main-UI reload without replay');
 await page.locator('[data-setup-key]').fill('fixture-write-only-key');await page.locator('[data-setup=key]').click();await page.getByText('API key stored privately.',{exact:false}).waitFor();assert.equal(await page.locator('[data-setup=done]').isEnabled(),false);
 dropFinish=true;await page.locator('[data-setup=start]').click();await page.getByText('The last action was not confirmed.',{exact:false}).waitFor();assert.equal(operations.filter(c=>c.op==='lifecycle').length,0);await openModel(true);await page.getByText('Saved agent setup matches the requested settings.',{exact:false}).waitFor();assert.equal(operations.filter(c=>c.op==='lifecycle').length,0);
 dropStart=true;await page.locator('[data-setup=start]').click();await page.getByText('The last action was not confirmed.',{exact:false}).waitFor();await openModel(true);await page.getByText('The agent is now observed configured and enabled.',{exact:false}).waitFor();assert.equal(operations.filter(c=>c.op==='apply-config').length,1);assert.equal(operations.filter(c=>c.op==='lifecycle').length,1);assert.deepEqual(instances.get(newAgent.id).document.config,{...newInstanceBefore,setup_complete:true,enabled:true});
 await page.locator('[data-setup=done]').click();await page.locator('[data-action=slack-setup]').click();await page.getByRole('button',{name:'Set up Slack',exact:true}).waitFor();
 pass('Fresh Instance setup needs explicit Start; lost completion and Start replies reopen by observation, with one config CAS and one Start');
 dropPrepare=true;await page.getByRole('button',{name:'Set up Slack',exact:true}).click();await page.getByText('The last step was not confirmed.',{exact:false}).waitFor();assert.equal(slackPosts.length,1);await openSlack(true);await page.getByRole('button',{name:'Connect to Slack',exact:true}).waitFor();assert.equal(slackPosts.length,1);assert.equal(slackRecords.get(newAgent.id).operation.request_id,slackPosts[0].request_id);
 pass('Cross-module continuation prepares one Slack installation; main-UI reopen recovers the same server intent after lost reply');
 await page.getByRole('button',{name:'Connect to Slack',exact:true}).click();await page.locator('[data-slack-command]').waitFor();assert.equal(await page.getByRole('link',{name:'Open Slack',exact:true}).getAttribute('href'),'https://app.slack.com/');
 refuseSlack=true;await page.getByLabel('Confirmation code from Slack').fill('fixture-confirmation');await page.getByRole('button',{name:'Confirm Slack approval',exact:true}).click();await page.getByRole('alert').filter({hasText:'changed'}).waitFor();assert.equal(slackPosts.filter(p=>p.action==='connect').length,0);
 // The previous confirmation form stays visible during this read. Wait for the
 // exact response and its final render before entering a new ephemeral code.
 const refreshedApproval=page.waitForResponse(response=>response.request().method()==='GET'&&response.url()===`${base}/api/v1/endpoints/${newAgent.id}/slack-setup`);
 await page.locator('[data-slack-setup=refresh]').click();assert.equal((await refreshedApproval).status(),200);
 await page.waitForFunction(()=>{const refresh=document.querySelector('[data-slack-setup=refresh]');return refresh&&!refresh.disabled;});
 mismatch=true;const confirmation=page.getByLabel('Confirmation code from Slack');await confirmation.fill('fixture-confirmation');assert.equal(await confirmation.inputValue(),'fixture-confirmation');
 await page.getByRole('button',{name:'Confirm Slack approval',exact:true}).click();await page.getByText('The saved approval belongs to other-workspace (TOTHER).',{exact:false}).waitFor();assert.equal(await page.getByRole('button',{name:'Create connection in this workspace',exact:true}).count(),0);assert.equal(slackPosts.filter(p=>p.action==='connect').length,0);
 pass('Proved Observatory predispatch refusal and explicit workspace mismatch cannot create a connection or overwrite an existing connector');
 mismatch=false;await page.getByRole('button',{name:'Connect to Slack',exact:true}).click();await page.getByLabel('Confirmation code from Slack').fill('fixture-confirmation');await page.getByRole('button',{name:'Confirm Slack approval',exact:true}).click();await page.getByText('workshop (TWORKSHOP)',{exact:true}).waitFor();assert.equal(slackPosts.filter(p=>p.action==='connect').length,0);
 await page.screenshot({path:path.join(artifacts,'combined-workspace-confirmation.png'),fullPage:true});
 dropConnect=true;await page.getByRole('button',{name:'Create connection in this workspace',exact:true}).click();await page.getByText('The last step was not confirmed.',{exact:false}).waitFor();await openSlack(true);await page.getByRole('button',{name:'Done',exact:true}).waitFor();assert.equal(slackPosts.filter(p=>p.action==='connect').length,1);await page.getByRole('button',{name:'Done',exact:true}).click();await page.getByRole('button',{name:'Model account',exact:true}).waitFor();
 pass('User confirms the named workspace; one lost connection reply reopens as saved only with verified exact endpoint binding');
 slackRecords.get(newAgent.id).connector.connection_running=false;const writesBefore=slackPosts.length;await openSlack(true);await page.getByText('The connection is saved and paused.',{exact:false}).waitFor();assert.equal(slackPosts.length,writesBefore);
 slackUnavailable=true;await page.locator('[data-slack-setup=refresh]').click();await page.getByText('Setup has not been confirmed.',{exact:false}).waitFor();assert.equal(await page.getByRole('button',{name:'Set up Slack',exact:true}).count(),0);assert.equal(await page.getByRole('button',{name:'Create connection in this workspace',exact:true}).count(),0);assert.equal(slackPosts.length,writesBefore);slackUnavailable=false;await page.locator('[data-slack-setup=refresh]').click();await page.getByRole('button',{name:'Done',exact:true}).waitFor();
 pass('Saved paused Slack stays paused; unavailable observations disable actions and never recreate/restart/re-authorize it');
 await openModel(false,existingAgent);assert.equal(await page.locator('[data-setup=start]').count(),0);assert.equal(await page.locator('[data-setup=done]').isEnabled(),true);assert.equal(mutations().filter(c=>c.endpoint_id===existingAgent.id).length,0);
 assert.deepEqual(await api('endpoints',undefined,'GET'),registryBefore);assert.deepEqual(instances.get(existingAgent.id),existingBefore);assert.deepEqual(slackRecords.get(existingAgent.id),oldSlackBefore);assert.equal(sha(await fs.readFile(path.join(home,'secrets/local-password.json'))),accountBefore);assert.equal(sha(await fs.readFile(path.join(home,'config.json'))),configBefore);
 const stored=await page.evaluate(()=>JSON.stringify({local:{...localStorage},session:{...sessionStorage},html:document.body.innerHTML}));for(const secret of ['fixture-write-only-key','ephemeral-fixture-ticket','fixture-confirmation','synthetic-no-platform-credential'])assert(!stored.includes(secret));
 assert.deepEqual(errors,[]);assert.equal(installPosts.length,1);assert.equal(slackPosts.filter(p=>p.action==='prepare').length,1);
 pass('Existing endpoint/config/account/session choices and paused connector remain unchanged; no credential or Slack ticket retained in browser state');
 passed=true;await fs.writeFile(path.join(artifacts,'result.json'),JSON.stringify({passed,checks,observatory_binary_sha256:sha(await fs.readFile(binary)),served_ui_sha256:sourceHashes,application_path:osPath,counts:{synthetic_install:installPosts.length,instance_completion:operations.filter(c=>c.op==='apply-config').length,instance_start:operations.filter(c=>c.op==='lifecycle').length,slack_prepare:slackPosts.filter(c=>c.action==='prepare').length,slack_connect:slackPosts.filter(c=>c.action==='connect').length,provider_calls:0,model_prompts:0,external_slack_calls:0,os_installations:0},scope:{real:['native Observatory first-account setup','embedded world UI and route integration','durable endpoint registry and views','browser intent storage and reload'],synthetic:['OS install completion','Instance config/account/lifecycle responses','Slack package/local management and external authorization/deploy'],not_qualified:['actual bundled Instance installation','real provider account or model access','real Slack app creation/workspace approval','Windows/macOS OS-manager lifecycle']}},null,2)+'\n');
 console.log(JSON.stringify({passed,checks},null,2));
}catch(error){await fs.mkdir(artifacts,{recursive:true});if(page)await page.screenshot({path:path.join(artifacts,'failure.png'),fullPage:true}).catch(()=>{});await fs.writeFile(path.join(artifacts,'failure.json'),JSON.stringify({passed:false,error:error.stack,errors,checks,network,operations,slackPosts,installPosts},null,2)+'\n');throw error;}
finally{await browser?.close();await stop();await fs.mkdir(artifacts,{recursive:true});await fs.writeFile(path.join(artifacts,'native.stdout.log'),stdout);await fs.writeFile(path.join(artifacts,'native.stderr.log'),stderr);await fs.rm(home,{recursive:true,force:true});}
