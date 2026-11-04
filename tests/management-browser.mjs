import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import https from 'node:https';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory');
const home=await fs.mkdtemp(path.join(os.tmpdir(),'observatory-management-'));
let child,browser,fixture,advertised=false,executionPayload,updatePayload;
const errors=[],checks=[];
const pass=s=>{checks.push(s);console.log(`PASS ${s}`);};
try {
  execFileSync(binary,['--home',home,'init','--owner','fixture-owner','--bind','127.0.0.1'],{stdio:'pipe'});
  const config=JSON.parse(await fs.readFile(path.join(home,'config.json'),'utf8'));
  const token=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim(),cookie='management-cookie';
  execFileSync('python3',['-c',"import sqlite3,hashlib,sys,time; db=sqlite3.connect(sys.argv[1]); db.execute('INSERT INTO browser_sessions VALUES(?,?,?,?)',(hashlib.sha256(sys.argv[2].encode()).hexdigest(),'fixture-owner','fixture-csrf',int(time.time()*1000)+3600000)); db.commit()",path.join(home,'state.sqlite3'),cookie]);
  const memoryId='d2f00bd9-ab40-4eb6-b333-a8dd8f833f48',workId='13408493-34ab-4ee4-b723-3edd8b3e3cf6',runId='a3c04fb0-c176-4399-97ac-7f9a2d9e8586';
  const memory={id:memoryId,key:'Release date',revision:2,state:'conflicted',audience:{kind:'conversation',provider_id:'fixture-chat',conversation_id:'work'},selected_candidate_id:null,candidates:[{id:'a',statement:'Release on **Monday**.',sources:[{note:'Initial planning agreement',recorded_by:'fixture-human',recorded_at:1793500000000}]},{id:'b',statement:'Release on **Tuesday**.',sources:[{note:'Later correction',recorded_by:'fixture-human',recorded_at:1793500001000}]}]};
  const scheduler={settings:{enabled:true,paused:false,concurrency:1},limits:{turns_per_run:12},runs:[{id:runId,work_id:workId,state:'paused_limit',work_snapshot:{title:'Prepare the product'},turns_completed:3,budget:{max_turns:3,max_seconds:120},tool_calls:7,progress:'Plan and checks saved; implementation continues next.',report:'## Product report\n\nA **checked** planning artifact awaits review.'}]};
  const operations=[{path:'updates',methods:['GET','POST']},{path:'updates/policy',methods:['PUT']},{path:'work',methods:['GET']},{path:'memory',methods:['GET','POST']},{path:'memory/{id}',methods:['GET']},{path:'memory/{id}/commands',methods:['POST']},{path:'scheduler',methods:['GET','PUT']},{path:'work/{id}/execution',methods:['GET','POST']},{path:'config',methods:['GET','PUT']},{path:'native-config',methods:['GET','PUT']},{path:'native-config/schema',methods:['GET']},{path:'native-config/validate',methods:['POST']}];
  fixture=https.createServer({key:await fs.readFile(config.tls_key),cert:await fs.readFile(config.tls_cert)},async(req,res)=>{
    if(req.url==='/') {res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Independent agent</title><p>Independent agent dashboard</p><script>addEventListener("message",e=>{if(e.data?.protocol==="agent-instance.ui/1"&&e.data.type==="command")document.body.textContent="Selected agent screen: "+e.data.command.route+(e.data.command.work_id?" / "+e.data.command.work_id:"");});</script>');return;}
    if(req.headers.authorization!=='Bearer fixture-value'){res.writeHead(401);res.end('{}');return;}
    let text='';for await(const chunk of req)text+=chunk;
    let value;
    if(req.url==='/api/v1/descriptor')value={instance_id:'management-fixture',operations:advertised?operations:[],capabilities:advertised?['agent-instance.memory/1','agent-instance.scheduler/1','agent-instance.native-config/1']:[]};
    else if(req.url==='/api/v1/updates'){if(req.method==='POST'){updatePayload=JSON.parse(text);value={pending:true};}else value={schema:'constellation.installation-updates/1',helper_connected:true,policy:{check_minutes:60,channel:'stable',agent_initiated:true},helper:{state:'up_to_date',components:[{id:'runtime',version:'1.2.3',available:false}]}};}
    else if(req.url==='/api/v1/status')value={instance_id:'management-fixture',version:'fixture'};
    else if(req.url==='/api/v1/memory')value={items:[memory]};
    else if(req.url==='/api/v1/work')value={items:[{id:workId,title:'Deliver fixture',goal:'Review the requested file',state:'review',completion_contract:{brief:{acceptance_criteria:['Correct file'],reviewer_actors:['fixture-reviewer']},artifact_set:{artifacts:[{artifact:'file'}]}}}]};
    else if(req.url==='/api/v1/scheduler')value=scheduler;
    else if(req.url===`/api/v1/work/${workId}/execution`){executionPayload=JSON.parse(text);value={accepted:true};}
    else {res.writeHead(404);res.end('{}');return;}
    res.setHeader('Content-Type','application/json');res.end(JSON.stringify(value));
  });
  await new Promise(resolve=>fixture.listen(0,'127.0.0.1',resolve));
  const origin=`https://127.0.0.1:${fixture.address().port}`;
  child=spawn(binary,['--home',home,'serve'],{stdio:'ignore'});
  browser=await chromium.launch({headless:true,args:['--no-sandbox']});
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1450,height:1000}}),base=`https://127.0.0.1:${config.port}`;
  for(let i=0;i<100;i++){try{if((await context.request.get(base+'/healthz')).ok())break;}catch{}await new Promise(r=>setTimeout(r,100));}
  async function api(resource,body,method='POST',credential=token){const r=await context.request.fetch(base+'/api/v1/'+resource,{method,headers:{Authorization:'Bearer '+credential},...(body===undefined?{}:{data:body})});const value=await r.json();assert(r.ok(),JSON.stringify(value));return value;}
  await api('credentials',{name:'fixture',value:'fixture-value'});
  const endpoint=await api('endpoints',{label:'Managed fixture',kind:'agent',ui_url:origin,management:{provider:'agent-instance',access:'manage',base_url:origin,credential_ref:'fixture',ca_file:config.tls_ca,expected_id:'management-fixture'}});
  await api(`endpoints/${endpoint.id}/inspect`,{});
  await context.addCookies([{name:'obs_'+config.id.replaceAll('-',''),value:cookie,url:base,secure:true,httpOnly:true,sameSite:'Strict'}]);
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));await page.goto(base);await page.getByRole('button',{name:'Open',exact:true}).click();
  assert.equal(await page.getByRole('button',{name:'Memory',exact:true}).count(),0);assert.equal(await page.getByRole('button',{name:'Background work',exact:true}).count(),0);
  pass('Optional controls stay hidden until this installation advertises them');
  advertised=true;await page.getByRole('button',{name:'Check connection',exact:true}).click();
  await page.getByRole('button',{name:'Updates',exact:true}).click();await page.getByText('1.2.3',{exact:true}).waitFor();
  await Promise.all([page.waitForResponse(r=>r.url().endsWith('/operations/request-update')&&r.status()===200),page.getByRole('button',{name:'Update now',exact:true}).click()]);assert.equal(updatePayload.action,'update');assert.match(updatePayload.request_id,/^[0-9a-f-]{36}$/);
  await page.getByRole('button',{name:'Machines',exact:true}).click();await page.getByRole('button',{name:'Check status',exact:true}).click();await page.getByRole('button',{name:'Open updates',exact:true}).click();await page.getByText('1.2.3',{exact:true}).waitFor();
  pass('Per-agent updates and fleet inventory use the installation API without a shared updater');
  await page.getByRole('button',{name:'Memory',exact:true}).click();
  await page.getByRole('heading',{name:'Release date',exact:true}).waitFor();assert.equal(await page.locator('.memory-candidate strong').count(),2);await page.getByRole('button',{name:'Show sources',exact:true}).click();await page.getByText('Initial planning agreement',{exact:false}).waitFor();
  const vid=await page.evaluate(()=>sessionStorage.getItem('observatory-view'));let view=await api(`views/${vid}`,undefined,'GET');assert.equal(view.draft.memory_details,memoryId);
  await page.reload();await page.getByText('Initial planning agreement',{exact:false}).waitFor();
  await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));await fs.mkdir(path.join(root,'private'),{recursive:true});await page.screenshot({path:path.join(root,'private/fleet-memory-mobile.png'),fullPage:true});
  pass('Attributed alternatives render as Markdown with controllable source details and narrow layout');
  await page.setViewportSize({width:1450,height:1000});view=await api(`views/${vid}`,undefined,'GET');await api(`views/${vid}/commands`,{expected_revision:view.revision,controller:'fixture-agent',commands:[{op:'set',field:'tab',value:'background'},{op:'set',field:'draft',value:{run_report:runId}}]});await page.getByText('3 / 3 turns · 7 tool calls · 120 seconds authorized',{exact:true}).waitFor();await page.getByRole('heading',{name:'Product report',exact:true}).waitFor();
  view=await api(`views/${vid}`,undefined,'GET');assert.equal(view.draft.run_report,runId);pass('Programmatic navigation restores run state, cumulative limits, progress and the selected report');
  const execute=await api('tokens',{name:'execution only',scopes:['work:execute']});const payload={type:'resume',budget:{max_turns:4},expected_revision:2};await api(`endpoints/${endpoint.id}/operations/execute-work`,{resource_id:workId,payload},'POST',execute.token);assert.deepEqual(executionPayload,payload);
  pass('Execution proxy preserves the explicit continuation payload through a separate grant');
  await page.locator('[data-tab="work"]').click();await page.getByRole('button',{name:'Load work',exact:true}).click();await page.getByText('1 acceptance criteria · 1 delivered versions · 1 named reviewers',{exact:true}).waitFor();await page.getByRole('button',{name:'Inspect work and review',exact:true}).click();await page.frameLocator('iframe.embedded').getByText('Selected agent screen: work / '+workId,{exact:true}).waitFor();view=await api(`views/${vid}`,undefined,'GET');assert.equal(view.draft.embedded_work,workId);pass('Fleet work opens the exact responsibility in its independent agent dashboard');
  await page.getByRole('button',{name:'Configuration',exact:true}).click();await page.getByRole('button',{name:'Model & capabilities in agent settings',exact:true}).click();await page.frameLocator('iframe.embedded').getByText('Selected agent screen: configuration',{exact:true}).waitFor();view=await api(`views/${vid}`,undefined,'GET');assert.equal(view.draft.embedded_route,'configuration');
  pass('Native configuration opens the independent service UI through the negotiated view bridge');
  assert.deepEqual(errors,[]);pass('No browser script errors');console.log(JSON.stringify({passed:true,checks}));
} finally {
  await browser?.close();child?.kill();fixture?.closeAllConnections();await new Promise(resolve=>fixture?fixture.close(resolve):resolve());await fs.rm(home,{recursive:true,force:true});
}
