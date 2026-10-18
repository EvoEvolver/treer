#!/usr/bin/env node
// Focused route-collision check: no real identities, credentials or Telegram.
import {createRequire} from 'node:module';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {mkdtempSync,rmSync,mkdirSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const [binary,modulePath,browserPath,screenshots]=process.argv.slice(2);
if(!screenshots)throw new Error('usage: discovery-browser.mjs BINARY PLAYWRIGHT_MODULE BROWSER_CACHE SCREENSHOTS');
process.env.PLAYWRIGHT_BROWSERS_PATH=resolve(browserPath);
const {chromium}=createRequire(import.meta.url)(resolve(modulePath));
const home=mkdtempSync(join(tmpdir(),'endpoint-discovery-'));
const env={HOME:home,TMPDIR:home,PATH:'/usr/bin:/bin:/usr/sbin:/sbin',PLAYWRIGHT_BROWSERS_PATH:resolve(browserPath)};
mkdirSync(screenshots,{recursive:true,mode:0o700});
const fixture=spawn('/usr/bin/python3',[fileURLToPath(new URL('./http_fixture.py',import.meta.url)),resolve(binary),'--editable'],{env,stdio:['ignore','pipe','pipe']});
const lines=createInterface({input:fixture.stdout});let browser,diagnostic='';
fixture.stderr.on('data',chunk=>{diagnostic+=chunk.toString().slice(0,1000)});
const timer=setTimeout(()=>fixture.kill('SIGTERM'),10000);
try {
 const [line]=await once(lines,'line');clearTimeout(timer);const config=JSON.parse(line);
 browser=await chromium.launch({headless:true,env});
 for(const width of [1280,390])for(const colorScheme of ['light','dark']) {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme,locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.owner.token'}});
  let reserved=0;
  await page.route('**/api/auth/config',r=>{reserved++;return r.fulfill({json:{github:true,google:false,capabilities:[]}});});
  await page.route('**/api/auth/logout',r=>{reserved++;return r.fulfill({json:{logged_out:true}});});
  await page.goto(config.url);
  await page.getByRole('heading',{name:'Add a paused endpoint',exact:true}).waitFor();
  if(!await page.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('discovery form overflow');
  await page.screenshot({path:join(screenshots,`discovery-owner-${width}-${colorScheme}.png`),fullPage:true});
  const logout=page.waitForResponse(r=>r.url().endsWith('/api/endpoint/auth/logout')&&r.request().method()==='POST');
  await page.getByRole('button',{name:'Sign out',exact:true}).click();
  if((await logout).status()!==200)throw new Error('App logout failed');
  if(reserved!==0)throw new Error('App called Proxy-reserved authentication');
  // A configured foreign dedicated origin stays read-only, even for an owner.
  await page.route('**/api/endpoint/auth/config',r=>r.fulfill({json:{settings_url:'https://other.example/',login_url:'https://other.example/api/auth/start'}}));
  await page.reload();
  await page.getByRole('heading',{name:'Edit in the dedicated App',exact:true}).waitFor();
  if(await page.getByRole('heading',{name:'Add a paused endpoint',exact:true}).count())throw new Error('foreign discovery origin exposed editing');
  if(await page.getByRole('link',{name:'Open settings',exact:true}).getAttribute('href')!=='https://other.example/')throw new Error('foreign editing destination lost');
  await page.close();
 }
 console.log('PASS: dedicated owner create form desktop/390 light/dark; Proxy-reserved config/logout never requested; real CSRF-checked App logout; foreign configured origin remains read-only');
} finally {
 clearTimeout(timer);if(browser)await browser.close();fixture.kill('SIGTERM');
 if(fixture.exitCode===null)await once(fixture,'exit');lines.close();rmSync(home,{recursive:true,force:true});
 if(diagnostic)console.error('fixture diagnostic:',diagnostic.slice(0,1000));
}
