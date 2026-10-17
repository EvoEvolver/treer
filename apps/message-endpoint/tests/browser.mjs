#!/usr/bin/env node
// Focused browser check. Module and browser cache are explicit test tooling;
// process/browser environments contain no inherited Treer/harness credentials.
import {createRequire} from 'node:module';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {mkdtempSync,rmSync,mkdirSync,chmodSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const [binary,modulePath,browserPath,screenshotPath]=process.argv.slice(2);
if(!screenshotPath)throw new Error('usage: browser.mjs BINARY PLAYWRIGHT_MODULE BROWSER_CACHE SCREENSHOTS');
process.env.PLAYWRIGHT_BROWSERS_PATH=resolve(browserPath);
const {chromium}=createRequire(import.meta.url)(resolve(modulePath));
const home=mkdtempSync(join(tmpdir(),'endpoint-browser-'));chmodSync(home,0o700);
const env={HOME:home,TMPDIR:home,PATH:'/usr/bin:/bin:/usr/sbin:/sbin',PLAYWRIGHT_BROWSERS_PATH:resolve(browserPath)};
mkdirSync(screenshotPath,{recursive:true,mode:0o700});
const fixture=spawn('/usr/bin/python3',[fileURLToPath(new URL('./http_fixture.py',import.meta.url)),resolve(binary),'--editable'],{env,stdio:['ignore','pipe','pipe']});
let stderr='';fixture.stderr.on('data',chunk=>{stderr+=chunk.toString().slice(0,1000)});
const lines=createInterface({input:fixture.stdout});
const timer=setTimeout(()=>{fixture.kill('SIGTERM')},10000);
let browser;
try {
 const [line]=await once(lines,'line');clearTimeout(timer);const config=JSON.parse(line);
 browser=await chromium.launch({headless:true,env});
 for(const mode of ['wildcard','embedded']) for(const width of [1280,390]) for(const color of ['light','dark']) {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:color,locale:'en-US'});
  page.setDefaultTimeout(8000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(config[mode]);
  const surface=mode==='embedded'?page.frameLocator('iframe[title="Endpoint App"]'):page;
  await surface.getByText('Signed in as:',{exact:false}).waitFor();
  await surface.getByText('ag_demo',{exact:true}).waitFor();
  if(await surface.getByRole('link',{name:'Continue with Treer'}).count())throw new Error('injected owner was sent to login');
  if(await surface.getByRole('button',{name:'Create paused binding'}).count())throw new Error('embedded/forward view exposed editing');
  if(await surface.getByRole('link',{name:'Open settings'}).getAttribute('href')!==config.url)throw new Error('wrong dedicated editing URL');
  if(await surface.getByRole('link',{name:'Open this endpoint’s pause control'}).getAttribute('href')!==config.url+'#pause-ag_demo')throw new Error('missing endpoint-specific pause link');
  if(await surface.locator('img').count())throw new Error('metadata rendered HTML');
  const fits=await surface.locator('body').evaluate(el=>el.scrollWidth<=innerWidth);
  if(!fits)throw new Error(`horizontal overflow ${mode} ${width} ${color}`);
  await page.keyboard.press('Tab');
  const targets=await surface.locator('button,select,a.button').evaluateAll(elements=>elements.every(el=>el.getBoundingClientRect().height>=44));
  if(!targets)throw new Error('touch target below 44px');
  await surface.getByLabel('Language').selectOption('zh');
  await surface.getByRole('heading',{name:'消息端点',exact:true}).waitFor();
  await page.screenshot({path:join(screenshotPath,`${mode}-${width}-${color}.png`),fullPage:true});
  if(errors.length)throw new Error(errors.join('\n'));
  await page.close();
 }
 const page=await browser.newPage({locale:'en-US'});await page.goto(config.url);
 await page.getByRole('link',{name:'Continue with Treer',exact:true}).waitFor();
 const login=await page.getByRole('link',{name:'Continue with Treer',exact:true}).getAttribute('href');
 if(login!==config.url+'api/auth/start')throw new Error('OAuth fallback did not use configured dedicated App URL');
 await page.close();
 const forged=await browser.newPage({locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'forged'}});await forged.goto(config.url);
 await forged.getByRole('alert').waitFor();
 if(await forged.getByRole('link',{name:'Continue with Treer'}).count())throw new Error('forged identity fell back to login');
 await forged.close();
 const edit=await browser.newPage({viewport:{width:390,height:900},locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.owner.token'}});
 await edit.goto(config.url);await edit.getByRole('heading',{name:'Add a paused endpoint',exact:true}).waitFor();
 const create=edit.locator('form').first();
 await create.getByLabel('Endpoint Agent ID',{exact:true}).fill('ag_new');
 await create.getByLabel('Recipient Agent ID',{exact:true}).fill('ag_recipient');
 await create.getByRole('button',{name:'Preview binding from Core'}).click();
 await create.getByRole('heading',{name:'Confirm these current objects'}).waitFor();
 if(!await create.getByRole('button',{name:'Create paused binding'}).isDisabled())throw new Error('creation did not require confirmation');
 await create.getByRole('checkbox').check();await create.getByRole('button',{name:'Create paused binding'}).click();
 const card=edit.locator('section').filter({has:edit.locator('code').filter({hasText:/^ag_new$/})});
 await card.getByRole('heading',{name:'/team/ag_new',exact:true}).waitFor();
 await card.locator('summary').filter({hasText:'Configure endpoint'}).click();
 await card.getByLabel('Telegram account configuration ID (no token)',{exact:true}).fill('bot_fixture');
 await card.getByLabel('Chat ID',{exact:true}).fill('123');
 await card.getByLabel('Allowed human Telegram IDs (comma separated)',{exact:true}).fill('456');
 await card.getByLabel('Confirm the exact target and allowed senders.',{exact:true}).check();await card.getByRole('button',{name:'Bind Telegram target'}).click();
 await card.getByText('2',{exact:true}).waitFor();
 // Safety pause is its own form: invalid unrelated target fields cannot block it.
 await card.getByLabel('Chat ID',{exact:true}).fill('');
 await card.getByLabel('Confirm pausing this endpoint at the displayed revision.',{exact:true}).check();await card.getByRole('button',{name:'Pause endpoint',exact:true}).click();
 await card.getByText('3',{exact:true}).waitFor();
 if(!await edit.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('editing form overflow at 390px');
 await edit.screenshot({path:join(screenshotPath,'owner-edit-390-light.png'),fullPage:true});
 await edit.close();
 console.log('PASS: real hub, injected identity on wildcard + actual iframe prefix; 8 layout/theme screenshots + 390px confirmed owner create/bind/pause; fallback + forged rejection');
} finally {
 clearTimeout(timer);if(browser)await browser.close();fixture.kill('SIGTERM');
 if(fixture.exitCode===null)await once(fixture,'exit');lines.close();rmSync(home,{recursive:true,force:true});
 if(stderr)console.error('fixture diagnostic:',stderr.slice(0,1000));
}
