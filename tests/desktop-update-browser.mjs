// Actual update/recovery page against disposable native installations. No agents/models.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {createHash,randomUUID} from 'node:crypto';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const binary=path.resolve(process.env.OBSERVATORY_BINARY);
const artifacts=path.resolve(process.env.OBSERVATORY_QA_DIR||'private/desktop-update-browser');
const root=await fs.mkdtemp(path.join(os.tmpdir(),'observatory update browser '));
const processes=[],checks=[],consoleErrors=[];let browser;
const hash=b=>createHash('sha256').update(b).digest('hex');
const bytes=await fs.readFile(binary),name=path.basename(binary);
const target=({arm64:'aarch64',x64:'x86_64'}[os.arch()])+'-'+({linux:'unknown-linux-gnu',darwin:'apple-darwin',win32:'pc-windows-msvc'}[os.platform()]);
const version=await new Promise((resolve,reject)=>{const p=spawn(binary,['--version']);let s='';p.stdout.on('data',x=>s+=x);p.on('exit',c=>c?reject(Error('version failed')):resolve(s.trim().split(' ').at(-1)))});
async function stop(p){if(p.exitCode!==null||p.signalCode!==null)return;const ended=once(p,'exit');p.kill();await ended}
async function launch(exe,home){
 const p=spawn(exe,['--home',home,'--no-browser'],{stdio:['ignore','pipe','pipe']});processes.push(p);let errors='';p.stderr.on('data',x=>errors+=x);
 const lines=createInterface({input:p.stdout});
 const first=await Promise.race([once(lines,'line').then(([s])=>JSON.parse(s)),once(p,'exit').then(()=>{throw Error('native exit '+errors)}),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('native launch timed out')),30000);t.unref()})]);return{process:p,...first};
}
async function packageAt(folder,marker){await fs.mkdir(folder,{recursive:true});const exe=path.join(folder,name);await fs.writeFile(exe,Buffer.concat([bytes,Buffer.from(marker)]));await fs.chmod(exe,0o700);
 if(os.platform()==='darwin'){const p=spawn('/usr/bin/codesign',['--force','--sign','-',exe]);assert.equal((await once(p,'exit'))[0],0)}
 await fs.writeFile(path.join(folder,'observatory-package.json'),JSON.stringify({schema:1,component:'agent-observatory',target,version,binary:name,binary_sha256:hash(await fs.readFile(exe))}));return exe;
}
async function api(origin,token,route,body){const r=await fetch(origin+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},...(body?{body:JSON.stringify(body)}:{})});assert(r.ok,await r.clone().text());return r.json()}
try{
 await fs.mkdir(artifacts,{recursive:true});browser=await chromium.launch({headless:true,args:['--no-sandbox'],...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH}:{})});
 for(const [label,width,failStart] of [['desktop',1180,false],['mobile',390,true]]){
  const folder=path.join(root,label),home=path.join(folder,'installation');
  const old=await packageAt(path.join(folder,'old'),'');const next=await packageAt(path.join(folder,'new'),'\nlocal browser fixture '+label+'\n');
  let origin,control,offer,context;
  try{
   const first=await launch(old,home);origin=new URL(first.url).origin;
   await api(origin,new URL(first.url).hash.slice(1),'/desktop/create',{request_id:randomUUID(),name:'Browser fixture',password:'Disposable Browser Password 932!',start_at_signin:false});
   control=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim();const initial=await api(origin,control,'/api/v1/status');
   const preserved=await Promise.all(['config.json','secrets/control-token','secrets/local-password.json'].map(n=>fs.readFile(path.join(home,n))));
   offer=await launch(next,home);const journalPath=path.join(home,'desktop-update.json'),j=JSON.parse(await fs.readFile(journalPath));
   if(failStart){j.candidate.version='fixture-rejected-startup';await fs.writeFile(journalPath,JSON.stringify(j))}
   context=await browser.newContext({viewport:{width,height:900}});const page=await context.newPage();page.on('pageerror',e=>consoleErrors.push(e.message));
   await page.goto(offer.url);await page.getByRole('heading',{name:'A newer Observatory is ready.'}).waitFor();
   assert.equal(new URL(page.url()).hash,'');assert((await context.request.get(new URL('/desktop/nunito.woff2',offer.url).href)).ok());assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
   await page.screenshot({path:path.join(artifacts,label+'-offer.png'),fullPage:true});checks.push(label+': readable offer, no horizontal overflow, private capability removed from URL');
   let posts=0;await page.route('**/apply',async route=>{posts++;const r=await route.fetch();assert(r.ok());await route.abort('failed')});
   await page.keyboard.press('Tab');assert.equal(await page.evaluate(()=>document.activeElement.id),'apply');await page.keyboard.press('Enter');
   await page.waitForFunction(()=>document.querySelector('#message').textContent.length>0);await page.reload();
   await page.getByRole('button',{name:'Open Observatory',exact:true}).waitFor({state:'visible',timeout:90000});
   assert.equal(posts,1);const final=JSON.parse(await fs.readFile(journalPath));assert.equal(final.request_id,j.request_id);assert.equal(final.phase,failStart?'rolled_back':'applied');
   const expected=failStart?old:next;const actual=await api(origin,control,'/api/v1/status');assert.equal(actual.id,initial.id);assert.equal(actual.binary_sha256,hash(await fs.readFile(expected)));assert.notEqual(actual.pid,initial.pid);
   for(const [i,n] of ['config.json','secrets/control-token','secrets/local-password.json'].entries())assert.deepEqual(await fs.readFile(path.join(home,n)),preserved[i]);
   await page.screenshot({path:path.join(artifacts,label+'-complete.png'),fullPage:true});checks.push(label+': keyboard action, lost committed reply, reload uses original request without repeat dispatch; '+final.phase);
   const open=page.getByRole('button',{name:'Open Observatory',exact:true});await open.focus();await page.keyboard.press('Enter');await page.waitForURL(url=>url.origin===origin);await page.getByRole('button',{name:'Sign in',exact:true}).waitFor();
   assert.equal(new URL(page.url()).origin,origin);checks.push(label+': keyboard reopen reaches same installation and preserves account/credentials');
  }finally{
   await context?.close();
   if(origin&&control){try{const p=path.join(home,'desktop-update.json'),j=JSON.parse(await fs.readFile(p));j.phase='recovering';await fs.writeFile(p,JSON.stringify(j));await api(origin,control,'/api/v1/native-update/stop',{request_id:j.request_id,version})}catch{}}
   if(offer)await stop(offer.process);
  }
 }
 assert.deepEqual(consoleErrors,[]);
 await fs.writeFile(path.join(artifacts,'result.json'),JSON.stringify({source:process.env.OBSERVATORY_SOURCE||'local candidate; exact binary hash is authoritative for this run',binary_sha256:hash(bytes),checks,console_errors:consoleErrors,model_calls:0,agent_operations:0,production_operations:0,limits:'Linux ARM Chromium browser; two real binary variants at same version, inert trailing bytes. Mobile is 390px viewport, not physical phone. Failed startup uses explicit private fixture version mismatch.'},null,2)+'\n');
}finally{await browser?.close();for(const p of processes)await stop(p);await new Promise(r=>setTimeout(r,500));await fs.rm(root,{recursive:true,force:true})}
