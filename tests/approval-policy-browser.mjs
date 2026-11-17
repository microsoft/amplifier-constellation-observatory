// Actual browser and maintained UI, with a finite local API fixture. No real agent is contacted.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import {fileURLToPath} from 'node:url';
import {randomUUID,createHash} from 'node:crypto';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const sourceHashes=Object.fromEntries(await Promise.all(['ui/approval-policy.js','ui/world.js','ui/world.css','ui/app.js','ui/app.css','tests/approval-policy-browser.mjs'].map(async name=>[name,createHash('sha256').update(await fs.readFile(path.join(root,name))).digest('hex')])));
const artifacts=process.env.OBSERVATORY_QA_DIR||path.join(root,'private','approval-policy-browser');
const targets=Array.from({length:17},(_,i)=>({endpoint_id:randomUUID(),label:i===0?'Anchor':i===1?'Polaris':`Agent ${i+1}`,endpoint_revision:1,instance_id:randomUUID(),policy_revision:i<15?4:null,mode:i<15?'ask':null,status:i===15?'offline':i===16?'unsupported':'ready'}));
const endpoints=targets.map(t=>({id:t.endpoint_id,revision:1,kind:'agent',label:t.label,management:{provider:'agent-instance',access:'manage',expected_id:t.instance_id,base_url:'https://settings.invalid'},ui_url:'https://settings.invalid'}));
let view={id:randomUUID(),revision:1,world:{}},operation=null,loseReply=true,forbidden=false;
const writes=[],requests=[],errors=[],checks=[],captures=[];
const pass=name=>{checks.push(name);console.log('PASS '+name)};
const status=()=>({mode:targets.every(t=>t.mode==='automatic')?'automatic':'mixed',complete:false,observed_at:Date.now(),targets,preview:{targets:targets.map(({endpoint_id,endpoint_revision,instance_id,policy_revision})=>({endpoint_id,endpoint_revision,instance_id,policy_revision}))},last_operation:operation});
const server=http.createServer(async(req,res)=>{
 try{
  const url=new URL(req.url,'http://fixture');let raw='';for await(const chunk of req)raw+=chunk;const body=raw?JSON.parse(raw):null;
  requests.push({method:req.method,path:url.pathname});const json=(value,code=200)=>{res.writeHead(code,{'Content-Type':'application/json'});res.end(JSON.stringify(value))};
  if(url.pathname==='/auth/session')return json({authenticated:true,owner:'fixture-owner',csrf:'fixture-csrf'});
  const api=url.pathname.replace('/api/v1/','');
  if(api==='approval-policy'){if(forbidden)return json({error:'Administrator required'},403);return json(status())}
  if(api==='approval-policy/operations'&&req.method==='POST'){
   writes.push(structuredClone(body));assert.equal(writes.length,1,'Browser must not resend');assert.equal(body.targets.length,17);assert.equal(body.mode,'automatic');
   operation={id:body.request_id,mode:body.mode,status:'partial',created_at:Date.now(),updated_at:Date.now(),targets:targets.map(t=>({...t,status:t.status==='ready'?'succeeded':t.status,observed:t.status==='ready'?{mode:'automatic',policy_revision:5,matches_requested_mode:true}:null}))};
   for(const t of targets)if(t.status==='ready'){t.mode='automatic';t.policy_revision=5}
   if(loseReply){loseReply=false;return json({error:'Response unavailable after the operation was recorded'},500)}return json(operation);
  }
  if(api.startsWith('approval-policy/operations/'))return operation?.id===api.split('/').at(-1)?json(operation):json({error:'Not found'},404);
  if(api==='endpoints')return json({endpoints});if(['hosts','places','jobs'].includes(api))return json({[api]:[]});
  if(api.startsWith('endpoints/'))return json(api.endsWith('inspect')?{health:'reachable'}:{services:[],connections:[]});
  if(api==='views'&&req.method==='POST')return json(view);
  if(api.startsWith('views/')){if(api.endsWith('/commands')){view={...view,revision:view.revision+1};for(const c of body.commands)view[c.field]=c.value}return json(view)}
  if(url.pathname.startsWith('/api/')||url.pathname.startsWith('/auth/'))return json({error:'Unexpected fixture request'},404);
  const file=url.pathname==='/'?'world-index.html':url.pathname.slice(1);if(file.includes('..'))return json({},400);
  const data=await fs.readFile(path.join(root,'ui',file));const type=file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.html')?'text/html':file.endsWith('.woff2')?'font/woff2':'image/webp';res.writeHead(200,{'Content-Type':type});res.end(data);
 }catch(error){errors.push(String(error));res.writeHead(500);res.end('{}')}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
let browser;
try{
 browser=await chromium.launch({headless:true});const context=await browser.newContext({viewport:{width:1280,height:1000}});const page=await context.newPage();page.on('pageerror',e=>errors.push(String(e)));
 await page.goto(base);await page.locator('[data-action=approvals]').click();await page.locator('.ow-approval-agents li').last().waitFor();
 assert.equal(await page.locator('.ow-approval-agents li').count(),17);assert.equal(writes.length,0);assert.equal(await page.locator('[data-action=approvals-apply]').isEnabled(),false);pass('Approvals opens from main world, shows the full 17-agent cohort and sends no policy write');
 await page.getByRole('radio',{name:/Automatic/}).check();assert.equal(writes.length,0);assert(await page.locator('[data-action=approvals-apply]').isEnabled());
 await fs.mkdir(artifacts,{recursive:true});const desktop=path.join(artifacts,'desktop-approvals.png');await page.screenshot({path:desktop,fullPage:true});captures.push(desktop);
 await page.locator('[data-action=approvals-apply]').click();await page.getByRole('heading',{name:'15 of 17 agents confirmed Automatic'}).waitFor();assert.equal(writes.length,1);assert.deepEqual(writes[0].targets.map(t=>t.endpoint_id),targets.map(t=>t.endpoint_id));pass('Explicit Apply includes offscreen/unavailable agents and lost response recovers with GET only');
 assert(await page.locator('.ow-approval-agents').getByText('Not reachable',{exact:true}).count());assert(await page.locator('.ow-approval-agents').getByText('Not supported here',{exact:true}).count());pass('Partial outcome keeps unavailable and unsupported agents visible');
 await page.reload();await page.locator('[data-action=approvals]').click();await page.getByRole('heading',{name:'15 of 17 agents confirmed Automatic'}).waitFor();assert.equal(writes.length,1);pass('Reload restores the existing operation without another policy write');
 const mobileContext=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});const mobile=await mobileContext.newPage();mobile.on('pageerror',e=>errors.push(String(e)));await mobile.goto(base+'/?approvals=1');await mobile.getByRole('heading',{name:'15 of 17 agents confirmed Automatic'}).waitFor();assert.equal(writes.length,1);pass('New browser context recovers the server-owned operation without local storage');
 const mobileShot=path.join(artifacts,'mobile-approvals.png');await mobile.screenshot({path:mobileShot,fullPage:true});captures.push(mobileShot);
 const overflow=await mobile.evaluate(()=>document.documentElement.scrollWidth>innerWidth);assert.equal(overflow,false);const box=await mobile.locator('#ow-dialog').boundingBox();assert(box.width<=390);pass('Mobile layout fits without horizontal page overflow');
 await mobile.keyboard.press('Escape');await mobile.locator('#ow-dialog').waitFor({state:'hidden'});await mobile.locator('[data-action=approvals]').click();await mobile.locator('.ow-approval-agents li').first().waitFor();assert.equal(writes.length,1);pass('Closing and reopening never resubmits the change');
 forbidden=true;await page.reload();await page.locator('[data-action=approvals]').click();await page.getByText('An Observatory administrator can manage approval settings.',{exact:false}).waitFor();assert.equal(await page.locator('[data-action=approvals-apply]').count(),0);assert.equal(writes.length,1);pass('Missing administrator access shows a readable refusal without write controls');
 assert.deepEqual(errors,[]);console.log(JSON.stringify({checks,captures,policyPosts:writes.length,scope:'Maintained browser UI and local API fixture; no real agents, services, or policy changes.'},null,2));
 await fs.writeFile(path.join(artifacts,'result.json'),JSON.stringify({checks,captures,policyPosts:writes.length,errors,sourceHashes,scope:'Maintained browser UI and local API fixture; no real agents, services, or policy changes.'},null,2)+'\n');
}finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve))}
