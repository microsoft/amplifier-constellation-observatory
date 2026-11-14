import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';

const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.OBSERVATORY_BINARY||path.join(root,'target/debug/agent-observatory');
const home=await fs.mkdtemp(path.join(os.tmpdir(),'observatory-delayed-view-'));
const results=[],errors=[];
let child,browser;
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function until(check,label){for(let i=0;i<200;i++){if(await check())return;await pause(10);}throw Error('Timed out: '+label);}

try {
  execFileSync(binary,['--home',home,'init','--owner','fixture-owner','--bind','127.0.0.1'],{stdio:'pipe'});
  const config=JSON.parse(await fs.readFile(path.join(home,'config.json'),'utf8'));
  const token=(await fs.readFile(path.join(home,'secrets/control-token'),'utf8')).trim();
  const cookie='delayed-view-fixture';
  execFileSync('python3',['-c',"import sqlite3,hashlib,sys,time; db=sqlite3.connect(sys.argv[1]); db.execute('INSERT INTO browser_sessions VALUES(?,?,?,?)',(hashlib.sha256(sys.argv[2].encode()).hexdigest(),'fixture-owner','fixture-csrf',int(time.time()*1000)+3600000)); db.commit()",path.join(home,'state.sqlite3'),cookie]);
  child=spawn(binary,['--home',home,'serve'],{stdio:'ignore'});
  browser=await chromium.launch({headless:true,args:['--no-sandbox']});
  const base=`https://127.0.0.1:${config.port}`;
  const control=await browser.newContext({ignoreHTTPSErrors:true});
  await until(async()=>{try{return(await control.request.get(base+'/healthz')).ok();}catch{return false;}},'fixture service');
  async function api(resource,body,method='POST'){
    const response=await control.request.fetch(base+'/api/v1/'+resource,{method,headers:{Authorization:'Bearer '+token},...(body===undefined?{}:{data:body})});
    const value=await response.json();assert(response.ok(),JSON.stringify(value));return value;
  }
  await api('credentials',{name:'fixture-management',value:'fixture-only'});
  const endpoint=await api('endpoints',{label:'Fixture agent',kind:'agent',management:{provider:'agent-instance',base_url:'https://fixture.invalid',access:'manage',credential_ref:'fixture-management',expected_id:'fixture-agent'}});
  const other=await api('endpoints',{label:'Other fixture agent',kind:'agent',ui_url:'https://other-fixture.invalid'});
  const commands=patch=>Object.entries(patch).map(([field,value])=>({op:'set',field,value}));
  async function change(id,patch){const current=await api('views/'+id,undefined,'GET');return api(`views/${id}/commands`,{expected_revision:current.revision,controller:'fixture-peer',commands:commands(patch)});}

  async function scenario(name,initial,exercise){
    const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1450,height:1000}});
    const created=await api('views',{}),saved=await change(created.id,initial);
    await context.addCookies([{name:'obs_'+config.id.replaceAll('-',''),value:cookie,url:base,secure:true,httpOnly:true,sameSite:'Strict'}]);
    const page=await context.newPage(),posts=[],effects=[];
    page.on('pageerror',e=>errors.push(e.message));
    page.on('request',request=>{
      if(request.method()!=='POST')return;
      const pathname=new URL(request.url()).pathname;
      if(pathname===`/api/v1/views/${created.id}/commands`)posts.push(request.postDataJSON());
      else if(!pathname.endsWith('/ack')&&!pathname.endsWith('/operations/config'))effects.push(pathname);
    });
    if(process.env.OBSERVATORY_APP_SOURCE){
      const source=await fs.readFile(process.env.OBSERVATORY_APP_SOURCE,'utf8');
      await page.route(base+'/app.js',route=>route.fulfill({status:200,contentType:'text/javascript',body:source}));
    }
    await page.route(`**/api/v1/endpoints/${endpoint.id}/operations/config`,route=>route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({config:{label:'initial'}})}));
    await page.addInitScript(id=>sessionStorage.setItem('observatory-view',id),created.id);
    // Leave network and the actual UI running, while placing the real poll before
    // the delayed input callback. No production helper is replaced or called.
    await page.clock.install({time:new Date('2026-01-01T00:00:00Z')});
    await page.clock.pauseAt(new Date('2026-01-01T00:00:01Z'));
    try {
      await page.goto(base+'/advanced');
      await page.locator('#view-label').waitFor();
      await until(async()=>(await api('views/'+created.id,undefined,'GET')).rendered_revision>=saved.revision,'initial render acknowledgment');
      const read=()=>api('views/'+created.id,undefined,'GET');
      const peer=patch=>change(created.id,patch);
      const poll=async updated=>{
        const ack=page.waitForRequest(r=>r.url()===`${base}/api/v1/views/${created.id}/ack`&&r.postDataJSON()?.revision===updated.revision);
        await page.clock.runFor(20);await ack;
      };
      await exercise({page,posts,read,peer,poll,id:created.id});
      assert.deepEqual(effects,[],'view edits must not dispatch business effects');
      results.push({name,status:'passed',view_requests:posts.map(body=>({expected_revision:body.expected_revision,fields:body.commands.map(command=>command.field)})),effects});console.log('PASS '+name);
    } catch(error) {
      results.push({name,status:'failed',error:error.message,view_requests:posts.map(body=>({expected_revision:body.expected_revision,fields:body.commands.map(command=>command.field)})),effects});console.error('FAIL '+name+': '+error.message);
    } finally {await context.close();}
  }

  await scenario('An unchanged draft saves its original values and preserves other draft fields',
    {screen:'agents',modal:'connect',draft:{label:'Initial',command:'keep-this-command'}},
    async({page,read})=>{
      await page.getByLabel('Name',{exact:true}).fill('Local edit');
      await page.clock.runFor(300);
      await until(async()=>(await read()).draft.label==='Local edit','ordinary delayed draft');
      assert.equal((await read()).draft.command,'keep-this-command');
      await page.getByLabel('Name',{exact:true}).fill('Second local edit');
      await page.clock.runFor(300);
      await until(async()=>(await read()).draft.label==='Second local edit','same form after its own saved edit');
      assert.equal((await read()).draft.command,'keep-this-command');
    });

  for(const edit of [
    {name:'draft',initial:{screen:'agents',modal:'connect',draft:{label:'Initial',command:'keep-this-command'}},selector:'#f-label',value:'Local edit after scroll',field:'draft',saved:v=>v.draft.label},
    {name:'configuration',initial:{screen:'agents',selected_id:endpoint.id,tab:'configuration',draft:{}},selector:'#f-configuration',value:'{"label":"local after scroll"}',field:'draft',saved:v=>v.draft.configuration,prepare:async page=>{await page.getByRole('button',{name:'Load configuration',exact:true}).click();await page.getByLabel('Configuration JSON',{exact:true}).waitFor();}},
    {name:'filter',initial:{screen:'agents',filter:'',focus:null},selector:'#search',value:'Local search after scroll',field:'filter',saved:v=>v.filter},
  ])await scenario(`An older automatic scroll does not discard a later ${edit.name} edit`,edit.initial,
    async({page,posts,read})=>{
      if(edit.prepare)await edit.prepare(page);
      // Real scrolling schedules its normal 400ms save. A later substantive
      // edit must not lose its 250/300/350ms save to that earlier timer.
      await page.evaluate(()=>{window.fixtureScrollObserved=false;window.addEventListener('scroll',()=>{window.fixtureScrollObserved=true;},{once:true});document.body.style.minHeight='3000px';window.scrollTo(0,100);});
      await page.clock.runFor(20);
      await until(async()=>page.evaluate(()=>window.fixtureScrollObserved),'real scroll event before later input');
      assert.equal(await page.evaluate(()=>window.scrollY),100);
      await page.clock.runFor(200);
      assert.equal(posts.filter(body=>body.commands.some(command=>command.field==='scroll')).length,0,'scroll save must still be pending when the later edit begins');
      // Avoid Playwright's automatic scrolling changing the chosen ordering;
      // dispatch through the real element's ordinary input handler instead.
      await page.evaluate(({selector,value})=>{const input=document.querySelector(selector);input.value=value;input.dispatchEvent(new Event('input',{bubbles:true}));},{selector:edit.selector,value:edit.value});
      await page.clock.runFor(200);
      if(posts.some(body=>body.commands.some(command=>command.field==='scroll'))){
        await until(async()=>(await read()).scroll?.y===100,'earlier scroll completion');
      }
      await page.clock.runFor(200);
      await until(async()=>edit.saved(await read())===edit.value||(await page.locator('#toast').textContent()).includes('changed'),`${edit.name} disposition`);
      const saved=await read();
      assert.equal(edit.saved(saved),edit.value,'automatic same-view scrolling must not invalidate a later substantive edit');
      assert.equal(posts.filter(body=>body.commands.some(command=>command.field==='scroll')).length,0,'the superseded automatic scroll must not write after the later edit');
      assert.equal(posts.filter(body=>body.commands.some(command=>command.field===edit.field)).length,1);
      if(edit.name==='draft')assert.equal(saved.draft.command,'keep-this-command');
      if(edit.name==='filter')assert.equal(saved.focus,'search');
    });

  for(const peerChange of [false,true])await scenario(
    peerChange?'An in-flight local scroll never upgrades a later draft past a peer edit':'A later draft survives its own in-flight scroll acknowledgment',
    {screen:'agents',modal:'connect',draft:{label:'Initial',command:'keep-this-command'}},
    async({page,posts,read,peer,id})=>{
      let heldResponse=null,release;
      const released=new Promise(resolve=>{release=resolve;});
      await page.route(`**/api/v1/views/${id}/commands`,async route=>{
        const body=route.request().postDataJSON();
        if(!heldResponse&&body.commands.length===1&&body.commands[0].field==='scroll'){
          const response=await route.fetch();
          heldResponse=await response.json();
          await released;
          await route.fulfill({response});
        }else await route.continue();
      });
      try{
        await page.evaluate(()=>{window.fixtureScrollObserved=false;window.addEventListener('scroll',()=>{window.fixtureScrollObserved=true;},{once:true});document.body.style.minHeight='3000px';window.scrollTo(0,100);});
        await page.clock.runFor(20);
        await until(async()=>page.evaluate(()=>window.fixtureScrollObserved),'in-flight scroll event');
        await page.clock.runFor(420);
        await until(async()=>heldResponse!==null,'committed scroll response held before delivery');
        assert.equal((await read()).revision,heldResponse.revision);
        const local='Local edit while scroll reply is held';
        await page.evaluate(value=>{const input=document.querySelector('#f-label');input.value=value;input.dispatchEvent(new Event('input',{bubbles:true}));},local);
        await page.clock.runFor(300);
        assert.equal(posts.filter(body=>body.commands.some(command=>command.field==='draft')).length,0,'draft waits behind the actual in-flight action');
        const updated=peerChange?await peer({draft:{label:'Newer peer draft',command:'keep-this-command'}}):null;
        release();
        await until(async()=>{const current=await read(),refused=(await page.locator('#toast').textContent()).includes('changed');return peerChange?(await page.getByLabel('Name',{exact:true}).inputValue())==='Newer peer draft'||refused:current.draft.label===local||refused;},'in-flight draft disposition');
        await page.clock.runFor(500);
        const saved=await read(),draftPosts=posts.filter(body=>body.commands.some(command=>command.field==='draft'));
        assert.equal(saved.draft.label,peerChange?'Newer peer draft':local);
        assert.equal(saved.draft.command,'keep-this-command');
        if(peerChange)assert(draftPosts.length<=1,'local refusal or one conflicting request, never replay');
        else assert.equal(draftPosts.length,1,'one substantive request, never replay');
        if(draftPosts.length)assert.equal(draftPosts[0].expected_revision,heldResponse.revision,'only the exact acknowledged local scroll transition may be crossed');
        if(peerChange)assert.equal(saved.revision,updated.revision);
      }finally{release();}
    });

  await scenario('A newer same-field draft observed by polling wins over an older delayed form',
    {screen:'agents',modal:'connect',draft:{label:'Initial'}},
    async({page,posts,read,peer,poll})=>{
      await page.clock.runFor(1180);
      await page.getByLabel('Name',{exact:true}).fill('Old local draft');
      const updated=await peer({draft:{label:'Newer peer draft'},focus:'f-label'});
      await poll(updated);
      assert.equal(await page.getByLabel('Name',{exact:true}).inputValue(),'Newer peer draft');
      await page.clock.runFor(350);
      await until(async()=>posts.length>0||(await page.locator('#toast').textContent()).includes('changed'),'draft disposition');
      assert.equal((await read()).draft.label,'Newer peer draft');
      assert.equal((await read()).focus,'f-label');
      assert.equal(posts.length,0,'stale intent must not adopt the newer revision');
    });

  await scenario('A newer configuration draft survives the detached old textarea callback',
    {screen:'agents',selected_id:endpoint.id,tab:'configuration',draft:{}},
    async({page,posts,read,peer,poll})=>{
      await page.getByRole('button',{name:'Load configuration',exact:true}).click();
      await page.getByLabel('Configuration JSON',{exact:true}).waitFor();
      await page.clock.runFor(1180);
      await page.getByLabel('Configuration JSON',{exact:true}).fill('{"label":"old local"}');
      const updated=await peer({draft:{configuration:'{"label":"new peer"}'}});
      await poll(updated);await page.clock.runFor(400);
      await until(async()=>posts.length>0||(await page.locator('#toast').textContent()).includes('changed'),'configuration disposition');
      assert.equal((await read()).draft.configuration,'{"label":"new peer"}');
      assert.equal(posts.length,0);
    });

  await scenario('A newer filter and focus survive an older delayed search input',
    {screen:'agents',filter:'',focus:null},
    async({page,posts,read,peer,poll})=>{
      await page.clock.runFor(1180);
      await page.getByLabel('Find an agent',{exact:true}).fill('Old search');
      const updated=await peer({filter:'New search',focus:null});
      await poll(updated);await page.clock.runFor(450);
      await until(async()=>posts.length>0||(await page.locator('#toast').textContent()).includes('changed'),'filter disposition');
      const current=await read();assert.equal(current.filter,'New search');assert.equal(current.focus,null);
      assert.equal(posts.length,0);
    });

  await scenario('A delayed draft does not move into a newly selected resource',
    {screen:'agents',selected_id:endpoint.id,modal:'edit',draft:{label:'First agent'}},
    async({page,posts,read,peer,poll})=>{
      await page.clock.runFor(1180);
      await page.getByLabel('Name',{exact:true}).fill('Old resource draft');
      const updated=await peer({selected_id:other.id,draft:{label:'Other resource draft'}});
      await poll(updated);await page.clock.runFor(350);
      await until(async()=>posts.length>0||(await page.locator('#toast').textContent()).includes('changed'),'resource disposition');
      const current=await read();assert.equal(current.selected_id,other.id);assert.equal(current.draft.label,'Other resource draft');
      assert.equal(posts.length,0);
    });

  await scenario('A delayed scroll does not mutate a newer screen',
    {screen:'agents',scroll:null},
    async({page,posts,read,peer,poll})=>{
      await page.clock.runFor(1180);
      await page.evaluate(()=>window.dispatchEvent(new Event('scroll')));
      const updated=await peer({screen:'activity',scroll:null});
      await poll(updated);await page.clock.runFor(500);
      await until(async()=>posts.length>0||(await page.locator('#toast').textContent()).includes('changed'),'scroll disposition');
      const current=await read();assert.equal(current.screen,'activity');assert.equal(current.scroll,null);
      assert.equal(posts.length,0);
    });

  await scenario('An unobserved remote change returns one conflict without replay',
    {screen:'agents',modal:'connect',draft:{label:'Initial'}},
    async({page,posts,read,peer})=>{
      await page.getByLabel('Name',{exact:true}).fill('Old local draft');
      const updated=await peer({draft:{label:'Newer peer draft'}});
      const conflict=page.waitForResponse(r=>r.url().endsWith('/commands')&&r.status()===409);
      await page.clock.runFor(300);await conflict;
      await until(async()=>(await page.getByLabel('Name',{exact:true}).inputValue())==='Newer peer draft','conflict redraw');
      await page.clock.runFor(500);
      assert.equal((await read()).draft.label,'Newer peer draft');
      const draftPosts=posts.filter(body=>body.commands.some(command=>command.field==='draft'));
      assert.equal(draftPosts.length,1);assert.equal(draftPosts[0].expected_revision,updated.revision-1);
    });

  assert.deepEqual(errors,[]);
  await control.close();
} finally {
  if(browser)await browser.close();
  if(child&&child.exitCode===null){
    const exited=once(child,'exit');child.kill();
    let timeout;try{await Promise.race([exited,new Promise((_,reject)=>{timeout=setTimeout(()=>reject(Error('Fixture server did not exit')),5000);})]);}finally{clearTimeout(timeout);}
  }
  await fs.rm(home,{recursive:true,force:true});
}
if(process.env.OBSERVATORY_RESULT)await fs.writeFile(process.env.OBSERVATORY_RESULT,JSON.stringify({results,errors,fixture_home_removed:true},null,2)+'\n');
console.log(`${results.filter(r=>r.status==='passed').length}/${results.length} delayed-view browser cases passed`);
if(results.some(r=>r.status==='failed'))process.exitCode=1;
