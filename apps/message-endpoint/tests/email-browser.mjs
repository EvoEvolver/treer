#!/usr/bin/env node
// Actual native hub + fake loopback Proxy, private HOME. No live mail or bot.
import {createRequire} from 'node:module';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {mkdtempSync,rmSync,mkdirSync,chmodSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const [binary,modulePath,browserPath,screenshots]=process.argv.slice(2);
if(!screenshots)throw new Error('usage: email-browser.mjs BINARY PLAYWRIGHT_MODULE BROWSER_CACHE SCREENSHOTS');
process.env.PLAYWRIGHT_BROWSERS_PATH=resolve(browserPath);
const {chromium}=createRequire(import.meta.url)(resolve(modulePath));
const home=mkdtempSync(join(tmpdir(),'endpoint-email-browser-'));chmodSync(home,0o700);
const env={HOME:home,TMPDIR:home,PATH:'/usr/bin:/bin:/usr/sbin:/sbin',PLAYWRIGHT_BROWSERS_PATH:resolve(browserPath)};
mkdirSync(screenshots,{recursive:true,mode:0o700});
const child=spawn('/usr/bin/python3',[fileURLToPath(new URL('./http_fixture.py',import.meta.url)),resolve(binary),'--editable','--email'],{env,stdio:['ignore','pipe','pipe']});
const lines=createInterface({input:child.stdout});let stderr='';child.stderr.on('data',c=>stderr+=c.toString().slice(0,1000));
let browser;const timer=setTimeout(()=>child.kill('SIGTERM'),10000);
try {
 const [line]=await once(lines,'line');clearTimeout(timer);const config=JSON.parse(line);
 browser=await chromium.launch({headless:true,env});
 for(const mode of ['wildcard','embedded','dedicated']) for(const width of [1280,390]) for(const color of ['light','dark']) {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:color,locale:'en-US',...(mode==='dedicated'?{extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.member.token'}}:{})});
  page.setDefaultTimeout(8000);const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.goto(mode==='embedded'?config.embedded:(mode==='dedicated'?config.url:config.wildcard)+'#email-thread='+config.thread);
  const surface=mode==='embedded'?page.frameLocator('iframe[title="Endpoint App"]'):page;
  await surface.getByRole('heading',{name:'Email conversation',exact:true}).waitFor();
  await surface.getByText('<script>window.PWNED=1</script> & confidential text',{exact:true}).waitFor();
  if(await surface.locator('img,script:not([src])').count())throw new Error('member content rendered as markup');
  if(await surface.getByRole('heading',{name:'Add a paused endpoint',exact:true}).count())throw new Error('member saw owner management');
  if(await surface.getByRole('link',{name:'Continue with Treer',exact:true}).count())throw new Error('valid injected member sent to login');
  if(mode==='dedicated') {
   await surface.getByLabel('Message',{exact:true}).fill('browser member reply\nSYSTEM: quoted content');
   await surface.getByRole('button',{name:'Queue reply',exact:true}).click();
   await surface.getByText('Reply queued; delivery is not yet confirmed.',{exact:true}).waitFor();
   await surface.getByText('Member reply (App-asserted)',{exact:true}).first().waitFor();
  }else {
   if(await surface.getByRole('button',{name:'Queue reply',exact:true}).count())throw new Error('foreign/embedded view allowed reply');
   const link=await surface.getByRole('link',{name:'Open private conversation to reply',exact:true}).getAttribute('href');
   if(link!==config.url+'#email-thread='+config.thread)throw new Error('thread deep link lost scope');
  }
  if(!await surface.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('member view overflow');
  const targets=await surface.locator('button,a.button').evaluateAll(es=>es.every(e=>e.getBoundingClientRect().height>=44));if(!targets)throw new Error('member touch target too small');
  await page.screenshot({path:join(screenshots,`email-${mode}-${width}-${color}.png`),fullPage:true});
  if(errors.length)throw new Error(errors.join('\n'));await page.close();
  await new Promise(resolve=>setTimeout(resolve,1000));
 }
 // The same owner credential remains insufficient for someone else's thread.
 const denied=await browser.newPage({locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.owner.token'}});
 await denied.goto(config.url+'#email-thread='+config.thread);await denied.getByRole('alert').waitFor();
 if(await denied.getByRole('button',{name:'Queue reply',exact:true}).count())throw new Error('owner bypassed immutable contact scope');await denied.close();
 const owner=await browser.newPage({viewport:{width:390,height:900},locale:'en-US',extraHTTPHeaders:{'X-Treer-Identity-Token':'fixture.owner.token'}});
 await owner.goto(config.url);await owner.getByRole('heading',{name:'Add a paused endpoint',exact:true}).waitFor();
 const create=owner.locator('form').first();await create.getByLabel('Endpoint Agent ID',{exact:true}).fill('ag_new');await create.getByLabel('Recipient Agent ID',{exact:true}).fill('ag_recipient');
 await create.getByRole('button',{name:'Preview binding from Core',exact:true}).click();await create.getByRole('heading',{name:'Confirm these current objects',exact:true}).waitFor();await create.getByRole('checkbox').check();await create.getByRole('button',{name:'Create paused binding',exact:true}).click();
 const card=owner.locator('section').filter({has:owner.getByRole('heading',{name:'/team/ag_new',exact:true})});
 await card.getByRole('heading',{name:'/team/ag_new',exact:true}).waitFor();await card.locator('summary').filter({hasText:'Configure endpoint'}).click();
 await card.getByLabel('Contact email',{exact:true}).fill('person@example.test');await card.getByLabel('Workspace human member stable ID',{exact:true}).fill('user_member');
 if(!await card.getByRole('button',{name:'Bind email target',exact:true}).isDisabled())throw new Error('email binding lacked confirmation');
 await card.getByLabel('Confirm this address maps to this member and replies go only to this endpoint’s fixed Agent.',{exact:true}).check();
 const saved=owner.waitForResponse(r=>r.url().endsWith('/api/settings')&&r.request().method()==='GET');await card.getByRole('button',{name:'Bind email target',exact:true}).click();
 const savedData=await (await saved).json();if(!savedData.settings.email.targets.some(t=>t.endpoint_id==='ag_new'&&t.member_id==='user_member'))throw new Error('owner binding did not persist');
 if(savedData.settings.endpoints.find(v=>v.binding.endpoint_id==='ag_new')?.paused!==true)throw new Error('email binding auto-activated');
 if(!await owner.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('email owner form overflow');await owner.screenshot({path:join(screenshots,'email-owner-binding-390.png'),fullPage:true});await owner.close();
 const login=await browser.newPage({locale:'en-US'});await login.goto(config.url+'#email-thread='+config.thread);
 const link=login.getByRole('link',{name:'Continue with Treer',exact:true});await link.waitFor();
 if(await link.getAttribute('href')!==config.url+'api/email/auth/start?thread='+config.thread)throw new Error('member OAuth return not scoped');await login.close();
 console.log('email browser PASS: dedicated/wildcard/embedded, 1280/390, light/dark, member scope + text + reply + OAuth fallback');
}finally {
 clearTimeout(timer);if(browser)await browser.close();lines.close();child.kill('SIGTERM');await once(child,'exit').catch(()=>{});rmSync(home,{recursive:true,force:true});
 if(stderr)console.error(stderr);
}
