import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
const {chromium}=await import(process.env.PLAYWRIGHT_MODULE||'playwright');
const output=process.env.MACHINE_HELPER_QA_DIR;
const browser=await chromium.launch({headless:true,args:['--no-sandbox'],...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH}:{})});
try{
 const page=await browser.newPage({viewport:{width:1000,height:850}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(process.env.MACHINE_HELPER_LOCAL_URL);await page.getByText('Disposable constellation',{exact:false}).waitFor();
 assert(!page.url().includes('#'),'launch capability removed from address bar');
 if(process.env.MACHINE_HELPER_PAIR_CODE){
  await page.getByLabel('Code from Observatory').fill(process.env.MACHINE_HELPER_PAIR_CODE.replaceAll('-',''));
  assert.match(await page.getByLabel('Code from Observatory').inputValue(),/^[A-Z0-9]{4}-[A-Z0-9]{4}$/);
  await page.getByLabel('Keep connected when I sign in').uncheck();
  await page.getByRole('button',{name:'Pair this computer',exact:true}).click();
  await page.getByRole('button',{name:'Check saved pairing',exact:true}).waitFor();
  assert(await page.locator('#pair').isHidden());
 }else{
  await page.getByText('This computer is in your constellation.',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Disconnect this computer',exact:true}).waitFor();
 }
 const state=process.env.MACHINE_HELPER_PAIR_CODE?'pending':'connected';
 if(output){await fs.mkdir(output,{recursive:true});await page.screenshot({path:path.join(output,state+'-desktop.png'),fullPage:true});}
 await page.setViewportSize({width:390,height:844});assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 if(output)await page.screenshot({path:path.join(output,state+'-mobile.png'),fullPage:true});
 assert.deepEqual(errors,[]);console.log('PASS helper browser: actual state, code formatting, no repeated pairing, capability privacy, desktop/mobile, no script errors');
}finally{await browser.close();}
