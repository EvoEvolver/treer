#!/usr/bin/env node
// UI behavior only; token verification/storage have separate real Rust tests.
// Every identity, token and HTTP service here is synthetic and local.
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
import {resolve,join} from 'node:path';
import {mkdir,mkdtemp,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
const [binary,modulePath,browserPath,screenshotPath]=process.argv.slice(2);
const home=await mkdtemp(join(tmpdir(),'endpoint-token-browser-'));
process.env.PLAYWRIGHT_BROWSERS_PATH=resolve(browserPath);
const {chromium}=await import('file://'+resolve(modulePath)+'/index.mjs');
const env={HOME:home,TMPDIR:home,PATH:'/usr/bin:/bin:/usr/sbin:/sbin',PLAYWRIGHT_BROWSERS_PATH:resolve(browserPath)};
await mkdir(screenshotPath,{recursive:true});
const fixture=spawn('/usr/bin/python3',[fileURLToPath(new URL('./http_fixture.py',import.meta.url)),resolve(binary),'--editable','--token-slots'],{env,stdio:['ignore','pipe','pipe']});
let stderr='';fixture.stderr.on('data',chunk=>{stderr+=chunk.toString()});
const lines=createInterface({input:fixture.stdout});
const timer=setTimeout(()=>fixture.kill('SIGTERM'),10000);
let browser;
const synthetic='12345:ABCDEFGHIJKLMNOPQRSTUVWXYZ_23456789';
try {
 const [line]=await once(lines,'line');clearTimeout(timer);const config=JSON.parse(line);
 browser=await chromium.launch({headless:true,env});
 for(const width of [1280,390]) for(const color of ['light','dark']) {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:color,locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.owner.token'}});
  page.setDefaultTimeout(8000);await page.goto(config.url);
  const panel=page.locator('section').filter({has:page.getByRole('heading',{name:'Telegram bot credentials',exact:true})});
  await panel.locator('summary').click();
  const input=panel.getByLabel('New bot token (write-only)',{exact:true});
  if(await input.getAttribute('type')!=='password'||await input.getAttribute('autocomplete')!=='off')throw new Error('not a write-only password field');
  await input.fill('bad');await panel.getByRole('checkbox').check();await panel.getByRole('button',{name:'Verify and save',exact:true}).click();
  await panel.getByRole('alert').waitFor();
  if(await input.inputValue()!=='')throw new Error('password retained after rejected submission');
  // This success is deliberately a UI double, never a real Telegram request.
  await page.route('**/api/telegram/token',async route=>{
   const body=route.request().postDataJSON();
   if(body.token!==synthetic||body.action!=='set')throw new Error('wrong token request');
   await route.fulfill({json:{status:'telegram_token_saved',account:{account_id:'bot_fixture',bot_id:12345,configured:true,last4:'6789',restart_required:true}}});
  });
  await input.fill(synthetic);await panel.getByRole('checkbox').check();await panel.getByRole('button',{name:'Verify and save',exact:true}).click();
  await panel.getByText('Change saved. Telegram workers are stopped.',{exact:true}).waitFor();
  if(await input.inputValue()!=='')throw new Error('password retained after successful submission');
  if(await page.locator('body').innerText().then(text=>text.includes(synthetic)))throw new Error('token echoed into DOM');
  const persisted=await page.evaluate(()=>JSON.stringify({local:{...localStorage},session:{...sessionStorage}}));
  if(persisted.includes(synthetic))throw new Error('token persisted in browser storage');
  await page.unroute('**/api/telegram/token');
  await panel.getByRole('checkbox').check();await panel.getByRole('button',{name:'Remove credentials and stop',exact:true}).click();
  await panel.getByText('Operator hub restart required. Endpoints stay paused.',{exact:true}).waitFor();
  if(!await page.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('horizontal overflow');
  await page.screenshot({path:join(screenshotPath,`token-${width}-${color}.png`),fullPage:true});
  await page.close();
 }
 if(stderr.includes(synthetic))throw new Error('secret in child stderr');
 console.log('token-browser: PASS (desktop/390 light/dark; password clears on failure/success; remove; no DOM/storage echo)');
} finally {
 clearTimeout(timer);await browser?.close();fixture.kill('SIGTERM');await once(fixture,'exit');await rm(home,{recursive:true,force:true});
}
