import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory');
const home=await fs.mkdtemp(path.join(os.tmpdir(),'observatory-ui-'));
let child,browser,count=0;const errors=[];
const pass=s=>{count++;console.log(`PASS ${s}`);};
try{
 execFileSync(binary,['--home',home,'init','--owner','fixture-owner','--bind','127.0.0.1'],{stdio:'pipe'});
 const config=JSON.parse(await fs.readFile(path.join(home,'config.json'),'utf8'));
 const credential=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim();
 const cookie='browser-fixture-cookie';
 execFileSync('python3',['-c',"import sqlite3,hashlib,sys,time; db=sqlite3.connect(sys.argv[1]); db.execute('INSERT INTO browser_sessions VALUES(?,?,?,?)',(hashlib.sha256(sys.argv[2].encode()).hexdigest(),'fixture-owner','fixture-csrf',int(time.time()*1000)+3600000)); db.commit()",path.join(home,'state.sqlite3'),cookie]);
 child=spawn(binary,['--home',home,'serve'],{stdio:'ignore'});
 browser=await chromium.launch({headless:true,args:['--no-sandbox']});
 const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1450,height:1000}});
 const base=`https://127.0.0.1:${config.port}`;
 for(let i=0;i<100;i++){try{const r=await context.request.get(base+'/healthz');if(r.ok())break;}catch{}await new Promise(r=>setTimeout(r,100));}
 assert.equal(JSON.parse(execFileSync(binary,['--home',home,'status'],{encoding:'utf8'})).version,'0.3.1');pass('CLI verifies HTTPS with installation CA');
 const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
 // A rejected view compare-and-swap is an expected API conflict, not a script error.
 page.on('console',m=>{if(m.type()==='error'&&!/Failed to load resource:.*status of 409/.test(m.text()))errors.push(m.text());});
 await page.goto(base);await page.getByRole('heading',{name:'Your agents, together.'}).waitFor();pass('Signed-out page has ordinary machine-account sign-in');
 await context.addCookies([{name:'obs_'+config.id.replaceAll('-',''),value:cookie,url:base,secure:true,httpOnly:true,sameSite:'Strict'}]);
 await page.reload();await page.getByRole('heading',{name:'Your agents, at a glance.'}).waitFor();pass('Authenticated empty fleet explains connecting an independent agent');
 async function api(resource,body,method='POST'){const response=await context.request.fetch(base+'/api/v1/'+resource,{method,headers:{Authorization:'Bearer '+credential},...(body===undefined?{}:{data:body})});const v=await response.json();assert(response.ok(),JSON.stringify(v));return v;}
 await page.getByRole('button',{name:'Access',exact:true}).click();await page.getByRole('button',{name:'Store credential'}).click();
 await page.getByLabel('Credential name',{exact:true}).fill('test-agent');await page.getByLabel('Credential value',{exact:true}).fill('fixture-value');
 await page.locator('#modal-form').getByRole('button',{name:'Save',exact:true}).click();await page.getByRole('dialog').waitFor({state:'hidden'});
 assert.deepEqual((await api('credentials',undefined,'GET')).names,['test-agent']);pass('Credentials are stored by name and absent from returned values');
 const fixture=path.join(home,'echo.py');await fs.writeFile(fixture,`import sys,json\nfor line in sys.stdin:\n r=json.loads(line);m=r.get('method');result={}\n if m=='initialize':result={'protocolVersion':1,'agentInfo':{'name':'fixture'},'agentCapabilities':{'loadSession':True}}\n if m in ('session/new','session/load'):result={'sessionId':'one'}\n if m=='session/prompt':\n  print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'one','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'image','mimeType':'image/png','data':'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII='}}}}),flush=True);print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'one','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Independent response'}}}}),flush=True);result={'stopReason':'end_turn'}\n print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)\n`);
 await page.getByRole('button',{name:'Agents',exact:true}).click();await page.getByRole('button',{name:'Connect existing',exact:true}).click();
 await page.getByLabel('Name',{exact:true}).fill('Fixture ACP');await page.getByText('ACP and other options',{exact:true}).click();
 await page.getByLabel('ACP executable',{exact:true}).fill('python3');await page.getByLabel('Arguments (JSON array)',{exact:true}).fill(JSON.stringify([fixture]));await page.getByLabel('Working folder',{exact:true}).fill(home);
 await page.locator('#modal-form').getByRole('button',{name:'Connect',exact:true}).click();await page.getByRole('dialog').waitFor({state:'hidden'});
 await page.getByRole('button',{name:'Open',exact:true}).click();await page.getByRole('heading',{name:'Fixture ACP',exact:true}).first().waitFor();pass('Generic ACP-only endpoint connects without management contract');
 await page.getByRole('button',{name:'Check ACP',exact:true}).click();await page.getByRole('status').filter({hasText:'ACP connected'}).waitFor();
 await page.locator('.badge').filter({hasText:/^Connected$/}).waitFor();await page.reload();await page.locator('.badge').filter({hasText:/^Connected$/}).waitFor();assert.equal(await page.getByRole('button',{name:'Configuration',exact:true}).count(),0);assert.equal(await page.getByRole('button',{name:'Start agent',exact:true}).count(),0);pass('ACP-only connection health survives reload without implying management controls');
 await page.getByRole('button',{name:'Talk',exact:true}).click();await page.getByLabel('Message',{exact:true}).fill('hello');await page.getByRole('button',{name:'Send',exact:true}).click();await page.locator('.chat-message').filter({hasText:'Independent response'}).waitFor();pass('Real subprocess negotiates ACP and renders model fixture response');
 const sid=(await api('sessions',undefined,'GET')).sessions[0].id;assert.equal((await api(`sessions/${sid}`,undefined,'GET')).messages.length,2);
 await page.reload();await page.locator('.chat-message').filter({hasText:'Independent response'}).waitFor();assert.equal(await page.getByLabel('Saved conversation',{exact:true}).inputValue(),sid);pass('Reload keeps the selected conversation and saved transcript');await page.locator('.generated-media img').first().waitFor();assert(await page.locator('.generated-media img').first().evaluate(n=>n.complete&&n.naturalWidth===1));assert(await page.getByRole('link',{name:'Download image'}).count()>0);pass('ACP image and download survive reload');
 {
 const betaFixture=path.join(home,'beta.py');await fs.writeFile(betaFixture,(await fs.readFile(fixture,'utf8')).replace('Independent response','Beta response'));
 const beta=await api('endpoints',{label:'Beta fixture',kind:'agent',acp:{transport:'stdio',command:'python3',args:[betaFixture],cwd:home}});
 const vid=await page.evaluate(()=>sessionStorage.getItem('observatory-view'));let v=await api(`views/${vid}`,undefined,'GET');
 const rejected=await context.request.post(base+`/api/v1/views/${vid}/commands`,{headers:{Authorization:'Bearer '+credential},data:{expected_revision:v.revision,commands:[{op:'set',field:'selected_id',value:beta.id},{op:'set',field:'session_id',value:sid}]}});
 assert.equal(rejected.status(),400);assert.equal((await api(`views/${vid}`,undefined,'GET')).revision,v.revision);
 await api(`views/${vid}/commands`,{expected_revision:v.revision,commands:[{op:'set',field:'selected_id',value:beta.id},{op:'set',field:'tab',value:'chat'}]});
 await page.getByRole('heading',{name:'Beta fixture',exact:true}).first().waitFor();assert.equal(await page.locator('.chat-message').count(),0);
 await page.getByLabel('Message',{exact:true}).fill('hello beta');await page.getByRole('button',{name:'Send',exact:true}).click();await page.locator('.chat-message').filter({hasText:'Beta response'}).waitFor();
 const betaSession=(await api('sessions',undefined,'GET')).sessions.find(s=>s.endpoint_id===beta.id);assert(betaSession);assert.equal((await api(`sessions/${sid}`,undefined,'GET')).messages.length,2);
 await page.reload();await page.locator('.chat-message').filter({hasText:'Beta response'}).waitFor();assert.equal(await page.locator('.chat-message').filter({hasText:'Independent response'}).count(),0);
 pass('Programmatic selection rejects crossed identities and sends only to the displayed agent');
 await page.getByRole('button',{name:'Agents',exact:true}).click();await page.getByRole('button',{name:'Check connections',exact:true}).click();await page.getByRole('status').filter({hasText:'Connections checked'}).waitFor();assert.equal(await page.locator('.card .badge').filter({hasText:/^Connected$/}).count(),2);await page.reload();await page.locator('.card .badge').filter({hasText:/^Connected$/}).first().waitFor();assert.equal(await page.locator('.card .badge').filter({hasText:/^Connected$/}).count(),2);pass('Bulk connection check includes ordinary ACP agents and persists their health');
 }

 {
 const approval=await api('endpoints',{label:'Approval fixture',kind:'agent',acp:{command:'python3',args:[path.join(root,'tests/fixture_permissions.py')],cwd:home}});
 const vid=await page.evaluate(()=>sessionStorage.getItem('observatory-view'));let v=await api(`views/${vid}`,undefined,'GET');await api(`views/${vid}/commands`,{expected_revision:v.revision,commands:[{op:'set',field:'selected_id',value:approval.id},{op:'set',field:'tab',value:'chat'}]});
 await page.getByRole('heading',{name:'Approval fixture',exact:true}).first().waitFor();await page.getByLabel('Message',{exact:true}).fill('Perform action');await page.getByRole('button',{name:'Send',exact:true}).click();
 await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();await page.locator('.chat-message').filter({hasText:'Preparing.'}).waitFor();assert.match(await page.locator('.tool-progress').innerText(),/Write a note/);assert.match(await page.locator('.agent-plan').innerText(),/Prepare note/);
 assert.doesNotMatch(await page.locator('#turn-progress').innerText(),/PRIVATE THOUGHT|FOREIGN|Done/);
 await page.getByLabel('Message',{exact:true}).fill('A draft while the agent waits');await new Promise(r=>setTimeout(r,1500));assert.equal(await page.getByLabel('Message',{exact:true}).inputValue(),'A draft while the agent waits');
 const pendingId=await page.locator('[data-permission-details]').getAttribute('data-permission-details');v=await api(`views/${vid}`,undefined,'GET');await api(`views/${vid}/commands`,{expected_revision:v.revision,controller:'voice-driver',commands:[{op:'set',field:'permission_details',value:pendingId},{op:'set',field:'focus',value:`permission-${pendingId}-option-0`}]});await page.locator('.permission-card pre').waitFor();assert.equal(await page.evaluate(()=>document.activeElement.id),`permission-${pendingId}-option-0`);pass('Approval details and control focus can be puppeteered through the view API');assert.match(await page.locator('.permission-card pre').innerText(),/private-action.txt/);
 await page.reload();await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();await page.locator('.permission-card pre').waitFor();await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await fs.mkdir(path.join(root,'private'),{recursive:true});await page.screenshot({path:path.join(root,'private/fleet-approval-mobile.png'),fullPage:true});
 await page.getByRole('button',{name:'Allow once',exact:true}).click();await page.locator('.chat-message').filter({hasText:'Done.'}).waitFor();assert.equal(await page.locator('.permission-card').count(),0);await page.setViewportSize({width:1450,height:1000});pass('Live text, tools, plans and explicit approvals survive reload without overwriting the draft');
 await page.getByLabel('Message',{exact:true}).fill('Another action');await page.getByRole('button',{name:'Send',exact:true}).click();await page.getByRole('button',{name:'Allow once',exact:true}).waitFor();await page.getByRole('button',{name:'Stop this turn',exact:true}).click();await page.getByText('You stopped this turn. Work already performed is not undone.',{exact:true}).waitFor();assert.equal(await page.locator('.permission-card').count(),0);assert.match(await page.locator('.tool-progress').innerText(),/cancelled/);pass('Stopping a turn cancels its pending approval and final tool status');
 }
 const vid=await page.evaluate(()=>sessionStorage.getItem('observatory-view'));let v=await api(`views/${vid}`,undefined,'GET');
 await api(`views/${vid}/commands`,{expected_revision:v.revision,controller:'voice-driver',commands:[{op:'set',field:'screen',value:'hosts'},{op:'set',field:'selected_id',value:null}]});await page.getByRole('heading',{name:'Places your agents can live.'}).waitFor();pass('Programmatic view navigation appears in an already-open browser');
 let forcedConflict=false;
 await page.route(`**/api/v1/views/${vid}/commands`,async route=>{const b=route.request().postDataJSON();if(!forcedConflict&&b.commands?.every(c=>c.field==='scroll')){forcedConflict=true;const current=await api(`views/${vid}`,undefined,'GET');await api(`views/${vid}/commands`,{expected_revision:current.revision,controller:'voice-driver',commands:[{op:'set',field:'screen',value:'activity'}]});}await route.continue();});
 await page.evaluate(()=>window.dispatchEvent(new Event('scroll')));
 await page.getByRole('heading',{name:'Changes worth knowing about.'}).waitFor();assert(forcedConflict);await page.unroute(`**/api/v1/views/${vid}/commands`);
 pass('A background view conflict redraws the newer programmatic screen');
 await page.getByRole('button',{name:'Machines',exact:true}).click();await page.getByRole('heading',{name:'Places your agents can live.'}).waitFor();
 const host=await api('hosts',{label:'Fixture host',transport:'local',install_roots:[home],recipes:[{id:'fixture',version:'1',argv:['python3','-c',"print('installed');print('http://192.0.2.1:3001');print('https://192.0.2.1:3002')"]}]});
 await page.reload();await page.getByRole('heading',{name:'Fixture host'}).waitFor();await page.getByRole('button',{name:'Create an agent',exact:true}).click();await page.getByLabel('Agent name',{exact:true}).fill('New individual');await page.locator('#modal-form').getByRole('button',{name:'Install',exact:true}).click();await page.getByRole('dialog').waitFor({state:'hidden'});
 for(let i=0;i<100;i++){const j=(await api('jobs',undefined,'GET')).jobs[0];if(j?.status==='completed')break;await new Promise(r=>setTimeout(r,50));}
 assert.equal((await api('jobs',undefined,'GET')).jobs[0].status,'completed');pass('Host recipe plan and asynchronous installation complete with a durable job');
 await page.reload();await page.getByRole('button',{name:'View result',exact:true}).click();await page.getByRole('heading',{name:'Installation result',exact:true}).waitFor();await page.locator('pre.detail').filter({hasText:'installed'}).waitFor();assert.equal(await page.getByRole('link',{name:/Open setup/}).count(),1);assert.equal(await page.getByRole('link',{name:/Connection help/}).count(),1);await page.reload();await page.getByRole('heading',{name:'Installation result',exact:true}).waitFor();await page.locator('pre.detail').filter({hasText:'installed'}).waitFor();assert.equal(await page.getByRole('link',{name:/Open setup/}).count(),1);assert.equal(await page.getByRole('link',{name:/Connection help/}).count(),1);await page.getByRole('dialog').getByRole('button',{name:'Close',exact:true}).last().click();pass('Installation output is inspectable and selected result survives reload');
 await page.getByRole('button',{name:'Activity',exact:true}).click();await page.getByRole('heading',{name:'Changes worth knowing about.'}).waitFor();assert(!await page.getByText('view ack',{exact:true}).count());pass('Activity omits view-control noise');
 const experiment=await api('experiments',{label:'Workshop transcript fixture',status:'completed',summary:'Nine independently checked cases.',measurements:{checks:9}});
 await api(`experiments/${experiment.id}/transcript`,{expected_revision:experiment.revision,messages:[
  {id:'one',speaker:'Operator',kind:'operator_cue',room:'product',text:'Design a useful status card.'},
  {id:'two',speaker:'Alex',kind:'simulated_human',room:'product',text:'Keep the **result visible**.'},
  {id:'three',speaker:'Builder',kind:'support_agent',room:'product',text:'```python\nstatus = "ready"\n```'}
 ]},'PUT');
 await page.reload();await page.getByRole('button',{name:'Experiments',exact:true}).click();await page.getByRole('button',{name:'Read conversation',exact:true}).click();
 await page.locator('.experiment-message').first().waitFor();await page.getByRole('heading',{name:'Workshop transcript fixture',exact:true}).waitFor();assert.equal(await page.locator('.experiment-message').count(),3);assert.equal(await page.locator('.experiment-message strong').filter({hasText:'result visible'}).count(),1);await page.getByText('Simulated human',{exact:true}).waitFor();await page.getByText('Operator cue',{exact:true}).waitFor();assert.match(await page.locator('.experiment-message pre').innerText(),/status = "ready"/);
 await page.reload();await page.locator('.experiment-message').first().waitFor();await page.getByRole('heading',{name:'Workshop transcript fixture',exact:true}).waitFor();assert.equal(await page.locator('.experiment-message').count(),3);pass('Experiment public transcript preserves roles, renders Markdown, and survives reload');
 v=await api(`views/${vid}`,undefined,'GET');await api(`views/${vid}/commands`,{expected_revision:v.revision,controller:'voice-driver',commands:[{op:'set',field:'experiment_measurements',value:true}]});await page.locator('#experiment-measurements').waitFor();assert.match(await page.locator('#experiment-measurements').innerText(),/"checks": 9/);
 await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await page.getByRole('button',{name:'All experiments',exact:true}).click();await page.getByRole('button',{name:'Read conversation',exact:true}).waitFor();await page.setViewportSize({width:1450,height:1000});pass('Transcript navigation and measurement disclosure support view control and mobile');
 const output=path.join(root,'private');await fs.mkdir(output,{recursive:true});await page.getByRole('button',{name:'Agents',exact:true}).click();await page.screenshot({path:path.join(output,'fleet-desktop.png'),fullPage:true});
 await page.setViewportSize({width:390,height:844});await page.screenshot({path:path.join(output,'fleet-mobile.png'),fullPage:true});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));pass('Narrow layout avoids horizontal overflow');
 assert.deepEqual(errors,[]);pass('No browser script or content-security errors');
 console.log(`${count} browser checks passed`);
}catch(e){console.error('BROWSER ERRORS',errors);for(const c of browser?.contexts()||[])for(const p of c.pages()){console.error((await p.locator('body').innerText()).slice(0,2000));}throw e;}finally{await browser?.close();child?.kill();await fs.rm(home,{recursive:true,force:true});}
