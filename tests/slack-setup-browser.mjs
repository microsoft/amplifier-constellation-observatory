// Actual browser/module; deterministic local setup service and Slack boundary.
// No connector install, account, Slack API, model, manager or network authorization.
import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const endpoint={id:'e05391f6-97aa-4a4e-b1b9-68947e1299b4',revision:7,label:'Notebook'};
const op='d994ef20-7f67-4fd3-b68f-183f45d5e598';
const clone=value=>JSON.parse(JSON.stringify(value));
const fresh=()=>({schema:1,endpoint_id:endpoint.id,endpoint_revision:7,revision:1,operation:null,connector:{id:'',endpoint_id:endpoint.id,binding_verified:false,account_ready:false},setup:{authorization:'missing',next_action:'authorize'},capabilities:{can_prepare:true}});
let current=fresh(),deploys=0,posts=[],mismatch=false,refuseNext='package_unavailable';
const html=`<!doctype html><html lang="en"><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/world.css"><link rel="stylesheet" href="/slack-setup.css"><style>body{margin:0}#observatory-world{min-height:100vh;background:#fff1dc;padding:20px}.fixture-shell{max-width:720px;margin:auto}button,input{font:inherit}*{box-sizing:border-box}</style></head><body><main id="observatory-world"><div class="fixture-shell" id="fixture"></div></main><script type="module">import {mountSlackSetup} from '/slack-setup.js';const api=async(p,m='GET',b)=>{const r=await fetch('/api/v1/'+p,{method:m,headers:{'Content-Type':'application/json'},...(b===undefined?{}:{body:JSON.stringify(b)})});const v=await r.json();if(!r.ok)throw Object.assign(Error(v.error),{status:r.status});return v;};window.fixture=mountSlackSetup(document.querySelector('#fixture'),{endpoint:${JSON.stringify(endpoint)},api,onDone:v=>window.done=v,onBack:()=>window.back=true});</script></body></html>`;
const server=http.createServer(async(req,res)=>{
  try {
    if(req.url==='/'){res.setHeader('Content-Type','text/html');res.end(html);return;}
    if(['/slack-setup.js','/slack-setup.css','/world.css'].includes(req.url)){res.setHeader('Content-Type',req.url.endsWith('.js')?'text/javascript':'text/css');res.end(await fs.readFile(path.join(root,'ui',req.url.slice(1))));return;}
    if(req.url!==`/api/v1/endpoints/${endpoint.id}/slack-setup`){res.writeHead(404);res.end('{}');return;}
    if(req.method==='GET'){res.setHeader('Content-Type','application/json');res.end(JSON.stringify(current));return;}
    let bytes='';for await(const chunk of req)bytes+=chunk;const body=JSON.parse(bytes);posts.push(clone(body));
    if(refuseNext){
      const code=refuseNext;refuseNext=null;
      if(code==='setup_changed')current.revision++;
      res.setHeader('Content-Type','application/json');res.end(JSON.stringify({schema:1,kind:'slack_setup_refusal',refused_before_intent:true,endpoint_id:endpoint.id,endpoint_revision:body.endpoint_revision,action:body.action,request_id:body.request_id??null,operation_id:body.operation_id??null,expected_revision:body.expected_revision,code,message:'Pre-dispatch refusal fixture'}));return;
    }
    assert.equal(body.expected_revision,current.revision);current.revision++;
    let transient={};
    if(body.action==='prepare'){
      current.operation={id:op,request_id:body.request_id,state:'ready',phase:'slack',uncertain:false};current.connector={id:'saved-connector',label:'Notebook in Slack',endpoint_id:endpoint.id,binding_verified:true,account_ready:true};current.capabilities={can_authorize:true};
    } else {
      assert.equal(body.operation_id,op);
      if(body.action==='authorize'){
        current.setup={...current.setup,next_action:'confirm',authorization_pending:true};current.capabilities={can_confirm:true};transient.authorization_command={command:'/slackauthticket fixture-private-ticket',expires_at:Date.now()+600000};current.authorization_command=transient.authorization_command;
      } else if(body.action==='confirm'){
        assert.equal(body.challenge,'fixture-private-code');delete current.authorization_command;current.setup={...current.setup,authorization:'managed',authorization_pending:false,authorized_workspace:{team_id:'TWORKSHOP',domain:'workshop'},next_action:mismatch?'authorize':'connect',workspace_mismatch:mismatch};current.capabilities=mismatch?{can_authorize:true}:{can_connect:true,can_discard:true};
      } else if(body.action==='connect'){
        assert.equal(body.team_id,'TWORKSHOP');assert(!mismatch);deploys++;current.connector={...current.connector,setup_complete:true,connection_ready:true,connection_running:true};current.setup.next_action='done';current.capabilities={};
      } else if(body.action==='resume'){current.operation.state='complete';current.connector.connection_running=true;current.connector.binding_verified=true;current.capabilities={};
      } else throw Error('Unexpected action');
    }
    res.setHeader('Content-Type','application/json');res.end(JSON.stringify({...current,...transient}));
  } catch(error){res.writeHead(500,{'Content-Type':'application/json'});res.end(JSON.stringify({error:error.message}));}
});
let browser;const checks=[];const pass=message=>checks.push(message);
try {
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const origin=`http://127.0.0.1:${server.address().port}`;
  browser=await chromium.launch({headless:true});const context=await browser.newContext({viewport:{width:720,height:940}});const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  let drop=null;await page.route('**/api/v1/endpoints/*/slack-setup',async route=>{const body=route.request().postDataJSON();if(route.request().method()==='POST'&&body.action===drop){drop=null;await route.fetch();await route.abort();}else await route.continue();});
  await page.goto(origin);await page.getByRole('button',{name:'Set up Slack',exact:true}).click();await page.getByRole('alert').filter({hasText:'not available'}).waitFor();assert.equal(posts.length,1);assert.equal(current.operation,null);assert.equal(await page.evaluate(()=>sessionStorage.length),0);
  await page.reload();await page.getByRole('button',{name:'Set up Slack',exact:true}).waitFor();assert.equal(posts.length,1);
  pass('Exact unavailable-package refusal clears only its rejected preparation; reload never resubmits it');
  drop='prepare';await page.getByRole('button',{name:'Set up Slack',exact:true}).click();await page.getByText('The last step was not confirmed.',{exact:false}).waitFor();assert.equal(posts.length,2);assert.notEqual(posts[0].request_id,posts[1].request_id);
  await page.reload();await page.getByRole('button',{name:'Connect to Slack',exact:true}).waitFor();assert.equal(posts.length,2);assert.equal(current.operation.request_id,posts[1].request_id);pass('A new explicit preparation allocates one new intent; lost reply and reload resume it without repeating setup');
  refuseNext='setup_changed';await page.getByRole('button',{name:'Connect to Slack',exact:true}).click();await page.getByRole('alert').filter({hasText:'Saved setup changed'}).waitFor();assert.equal(posts.length,3);assert.equal(await page.locator('[data-slack-command]').count(),0);
  await page.getByRole('button',{name:'Connect to Slack',exact:true}).waitFor();assert.equal(posts.length,3);pass('Exact pre-intent CAS refusal reads current state and waits for another explicit action');
  await page.getByRole('button',{name:'Connect to Slack',exact:true}).click();await page.locator('[data-slack-command]').waitFor();assert((await page.locator('[data-slack-command]').textContent()).includes('fixture-private-ticket'));assert.equal(await page.getByRole('link',{name:'Open Slack',exact:true}).getAttribute('href'),'https://app.slack.com/');
  await page.setViewportSize({width:352,height:900});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  const browserState=await page.evaluate(()=>JSON.stringify({local:{...localStorage},session:{...sessionStorage}}));assert(!browserState.includes('fixture-private-ticket'));
  const approvedPosts=posts.length;await page.reload();await page.locator('[data-slack-command]').waitFor();assert.equal(posts.length,approvedPosts);assert.equal(await page.evaluate(()=>sessionStorage.length),0);pass('Reopening retrieves the same private approval command without another authorization or browser persistence');
  await page.getByLabel('Confirmation code from Slack').fill('fixture-private-code');await page.getByRole('button',{name:'Confirm Slack approval',exact:true}).click();await page.getByText('workshop (TWORKSHOP)',{exact:true}).waitFor();assert.equal(deploys,0);assert.equal(await page.locator('[data-slack-command]').count(),0);pass('Approval shows exact workspace for explicit confirmation; no app created and ticket removed');
  if(process.env.OBSERVATORY_QA_DIR){await fs.mkdir(process.env.OBSERVATORY_QA_DIR,{recursive:true});await page.screenshot({path:path.join(process.env.OBSERVATORY_QA_DIR,'slack-workspace-confirmation.png'),fullPage:true});}
  drop='connect';await page.getByRole('button',{name:'Create connection in this workspace',exact:true}).click();await page.getByText('The last step was not confirmed.',{exact:false}).waitFor();assert.equal(deploys,1);
  await page.reload();await page.getByRole('button',{name:'Done',exact:true}).waitFor();assert.equal(deploys,1);await page.getByRole('button',{name:'Done',exact:true}).click();assert.deepEqual(await page.evaluate(()=>window.done),{connector_id:'saved-connector'});pass('Lost committed connection reply reconciles after reload with exactly one creation and verified binding');
  current.connector.connection_running=false;const before=posts.length;await page.reload();await page.getByText('The connection is saved and paused.',{exact:false}).waitFor();assert.equal(posts.length,before);assert.equal(await page.getByRole('button',{name:'Connect to Slack',exact:true}).count(),0);pass('Existing paused connector remains saved without authorization, restart or reprovision');
  current.operation.state='complete';current.connector.binding_verified=false;current.capabilities={can_resume:true};const beforeResume=posts.length;await page.reload();await page.getByRole('button',{name:'Start saved connection',exact:true}).waitFor();assert.equal(posts.length,beforeResume);await page.getByRole('button',{name:'Start saved connection',exact:true}).click();await page.getByRole('button',{name:'Done',exact:true}).waitFor();assert.equal(posts.length,beforeResume+1);assert.equal(posts.at(-1).action,'resume');pass('Stopped saved connector requires one explicit resume and keeps the same installation');
  current={...fresh(),revision:10,operation:{id:op,request_id:posts[1].request_id,state:'ready'},connector:{id:'saved-connector',endpoint_id:endpoint.id,binding_verified:true,account_ready:true},setup:{authorization:'missing',next_action:'authorize',intended_workspace:{team_id:'TINTENDED',name:'Intended team'}},capabilities:{can_authorize:true}};mismatch=true;
  await page.reload();await page.getByRole('button',{name:'Connect to Slack',exact:true}).click();await page.getByLabel('Confirmation code from Slack').fill('fixture-private-code');await page.getByRole('button',{name:'Confirm Slack approval',exact:true}).click();await page.getByText('The saved approval belongs to workshop (TWORKSHOP).',{exact:false}).waitFor();assert((await page.locator('main').innerText()).includes('Intended team (TINTENDED)'));assert.equal(await page.getByRole('button',{name:'Create connection in this workspace',exact:true}).count(),0);assert.equal(deploys,1);pass('Mismatched workspace is explained with both identities and cannot create an app');
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));assert.deepEqual(errors,[]);assert(!(await page.evaluate(()=>JSON.stringify({...sessionStorage}))).includes('fixture-private-code'));
  pass('352px layout, secret-free browser storage and zero script errors');
  const result={passed:true,checks,scope:'Actual Chromium with synthetic local operation owner and Slack authorization/deploy boundary. No native install or real Slack/model/service call.'};
  if(process.env.OBSERVATORY_QA_DIR)await fs.writeFile(path.join(process.env.OBSERVATORY_QA_DIR,'result.json'),JSON.stringify(result,null,2)+'\n');
  console.log(JSON.stringify(result,null,2));
}finally{await browser?.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
