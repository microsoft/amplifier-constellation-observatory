// Actual native first launch and browser setup in disposable homes. No startup
// registration, live agents, credentials, model calls, or production services.
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory');
const artifacts=process.env.OBSERVATORY_QA_DIR||path.join(root,'private','desktop-browser');
const temp=await fs.mkdtemp(path.join(os.tmpdir(),'observatory desktop browser '));
const checks=[],processes=[];let browser;
const pass=name=>{checks.push(name);console.log('PASS '+name)};
async function stop(child){if(child.exitCode!==null||child.signalCode!==null)return;const exited=once(child,'exit');child.kill();await exited}
async function start(home){
 const child=spawn(binary,['--home',home,'--no-browser'],{stdio:['ignore','pipe','pipe']});processes.push(child);
 let errors='';child.stderr.on('data',chunk=>errors+=chunk);
 const lines=createInterface({input:child.stdout});
 const first=await Promise.race([once(lines,'line').then(([line])=>JSON.parse(line)),once(child,'exit').then(()=>{throw Error('Native app exited: '+errors)}),new Promise((_,reject)=>{const timer=setTimeout(()=>reject(Error('Native app did not announce setup')),20000);timer.unref()})]);
 return{child,first};
}
try{
 await fs.mkdir(artifacts,{recursive:true});
 browser=await chromium.launch({headless:true,args:['--no-sandbox'],...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH}:{})});
 for(const [name,width,loseReply] of [['desktop',1180,false],['mobile',390,true]]){
  const home=path.join(temp,name+' installation'),{child,first}=await start(home);
  assert.equal(first.state,'setup');const context=await browser.newContext({viewport:{width,height:900},reducedMotion:'reduce'});
  const page=await context.newPage(),errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto(first.url);await page.getByRole('heading',{name:'Start your constellation.'}).waitFor();
  assert.equal(new URL(page.url()).hash,'');
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await page.screenshot({path:path.join(artifacts,name+'-setup.png'),fullPage:true});
  pass(name+': native app opens a readable local setup without exposing the capability in browser history');
  let posts=0;
  await page.route('**/desktop/create',async route=>{
   posts++;
   if(loseReply&&posts===1){const result=await route.fetch();assert(result.ok());await route.abort('failed')}
   else await route.continue();
  });
  await page.getByLabel('Your name',{exact:true}).fill('Native Browser');
  const password='Disposable local password for '+name;
  await page.getByLabel('Choose an Observatory password',{exact:true}).fill(password);
  await page.getByLabel('Open Observatory when I sign in').uncheck();
  await page.getByRole('button',{name:'Open my constellation'}).click();
  if(loseReply){await page.getByRole('button',{name:'Check setup',exact:true}).click()}
  await page.locator('#observatory-world').waitFor();
  await page.waitForFunction(()=>document.querySelector('.ow-top'));
  assert.equal(posts,loseReply?2:1);
  assert.equal(await page.getByRole('button',{name:'Sign in',exact:true}).count(),0);
  const stored=await page.evaluate(()=>JSON.stringify({local:{...localStorage},session:{...sessionStorage}}));
  assert(!stored.includes(password));assert(!stored.includes(new URL(first.url).hash.slice(1)));
  const saved=await fs.readFile(path.join(home,'config.json'));
  const secret=await fs.readFile(path.join(home,'secrets/local-password.json'));
  assert(!secret.includes(password));
  await page.screenshot({path:path.join(artifacts,name+'-ready.png'),fullPage:true});
  pass(name+': '+(loseReply?'lost committed setup reply recovers the same account without repeating creation':'one setup submission signs in directly'));
  await stop(child);
  const restarted=spawn(binary,['--home',home,'--no-browser'],{stdio:'ignore'});processes.push(restarted);
  await context.clearCookies();
  for(let attempt=0;;attempt++){try{await page.reload();break}catch(error){if(attempt>100)throw error;await new Promise(resolve=>setTimeout(resolve,50))}}
  await page.getByRole('button',{name:'Sign in',exact:true}).waitFor();
  await page.getByText('Use the Observatory account you created during setup.',{exact:true}).waitFor();
  await page.getByLabel('Username',{exact:true}).fill('Native Browser');await page.getByLabel('Password',{exact:true}).fill(password);
  await page.getByRole('button',{name:'Sign in',exact:true}).click();await page.waitForFunction(()=>document.querySelector('.ow-top'));
  assert.deepEqual(await fs.readFile(path.join(home,'config.json')),saved);assert.deepEqual(await fs.readFile(path.join(home,'secrets/local-password.json')),secret);
  assert.deepEqual(errors,[]);pass(name+': restart uses the same account and installation with no repeated setup');
  await context.close();await stop(restarted);
 }
 await fs.writeFile(path.join(artifacts,'result.json'),JSON.stringify({checks,model_calls:0,startup_registrations:0,live_operations:0},null,2)+'\n');
}finally{await browser?.close();for(const child of processes)await stop(child);await fs.rm(temp,{recursive:true,force:true})}
