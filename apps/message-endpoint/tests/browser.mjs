#!/usr/bin/env node
// Focused browser check. Module and browser cache are explicit test tooling;
// process/browser environments contain no inherited Treer/harness credentials.
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {mkdtempSync,rmSync,mkdirSync,chmodSync,readFileSync} from 'node:fs';
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
const fixture=spawn('/usr/bin/python3',[fileURLToPath(new URL('./http_fixture.py',import.meta.url)),resolve(binary),'--editable','--seed-recovery'],{env,stdio:['ignore','pipe','pipe']});
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
  await surface.getByText('Old updates were discarded. Check the hub clock; downtime over ten minutes skips older queued messages.',{exact:true}).waitFor();
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
 await card.getByLabel('Confirm pausing this endpoint at the displayed revision.',{exact:true}).check();const pausedSettings=edit.waitForResponse(r=>r.url().endsWith('/api/settings')&&r.request().method()==='GET');await card.getByRole('button',{name:'Pause endpoint',exact:true}).click();
 const conflictSettings=await (await pausedSettings).json();
 await card.getByText('3',{exact:true}).waitFor();
 // Keep production verification limits while exercising one more owner flow.
 await new Promise(resolve=>setTimeout(resolve,10500));
 await card.getByText('Resume endpoint',{exact:true}).click();
 const previewResponse=edit.waitForResponse(r=>r.url().endsWith('/api/configuration')&&r.request().method()==='POST');
 await card.getByRole('button',{name:'Preview resume conditions',exact:true}).click();
 const conflictPreview=await (await previewResponse).json();
 const resumeButton=card.getByRole('button',{name:'Resume and send pending messages',exact:true});
 await resumeButton.waitFor();
 await card.getByText(/Approximate oldest held age/).waitFor();
 const heldChoice=card.getByLabel('Held content from the pause');
 if(await heldChoice.inputValue()!=='discard')throw new Error('held content defaults to release');
 await heldChoice.selectOption('release');await heldChoice.selectOption('discard');
 if(!await resumeButton.isDisabled())throw new Error('resume armed before owner declarations');
 await card.getByLabel('I reviewed the fixed relay profile and release version.',{exact:true}).check();
 const resumeConfirm=card.getByLabel('Confirm resuming these exact objects and sending pending messages.',{exact:true});
 if(!await resumeConfirm.isDisabled())throw new Error('one owner declaration armed resume confirmation');
 await card.getByLabel('Isolation state (explicit choice required)',{exact:true}).selectOption('reviewed');
 await card.getByLabel('Evidence reference (ticket or manifest SHA, no secrets)',{exact:true}).fill('#159 synthetic release reference');
 await resumeConfirm.check();
 if(!await resumeButton.isDisabled())throw new Error('browser declarations bypassed default operator gate/unready account');
 if(!await edit.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('resume preview overflow at 390px');
 await edit.screenshot({path:join(screenshotPath,'owner-resume-390-light.png'),fullPage:true});
 await edit.getByRole('button',{name:'Dark',exact:true}).click();
 await edit.screenshot({path:join(screenshotPath,'owner-resume-390-dark.png'),fullPage:true});
 // UI-only negative flow: keep the real default-off assertions above. These
 // intercepted responses enable only this page's controls and reject its POST;
 // they are NOT actual owner activation, Core readback or Policy evidence.
 const conflict=await browser.newPage({viewport:{width:390,height:900},locale:'en-US'});
 const enabledSettings=structuredClone(conflictSettings);enabledSettings.settings.activation_enabled=true;
 await conflict.route('**/api/settings',r=>r.fulfill({json:enabledSettings}));
 await conflict.route('**/api/auth/config',r=>r.fulfill({json:{settings_url:config.url,login_url:config.url+'api/auth/start'}}));
 let conflictPosts=0;
 await conflict.route('**/api/configuration',r=>{
  const command=r.request().postDataJSON();
  if(command.action==='preview_resume')return r.fulfill({json:{...conflictPreview,activation_enabled:true,account_ready:true}});
  if(command.action==='resume'){conflictPosts++;return r.fulfill({status:409,json:{error:{code:'configuration_conflict',message:'configuration_conflict'}}});}
  throw new Error('unexpected conflict-fixture action');
 });
 await conflict.goto(config.url);
 const cc=conflict.locator('section').filter({has:conflict.locator('code').filter({hasText:/^ag_new$/})});
 await cc.getByText('Resume endpoint',{exact:true}).click();
 await cc.getByRole('button',{name:'Preview resume conditions',exact:true}).click();
 await cc.getByLabel('I reviewed the fixed relay profile and release version.',{exact:true}).check();
 await cc.getByLabel('Isolation state (explicit choice required)',{exact:true}).selectOption('reviewed');
 await cc.getByLabel('Evidence reference (ticket or manifest SHA, no secrets)',{exact:true}).fill('synthetic conflict fixture');
 await cc.getByLabel('Confirm resuming these exact objects and sending pending messages.',{exact:true}).check();
 await cc.getByRole('button',{name:'Resume and send pending messages',exact:true}).click();
 await cc.getByRole('alert').filter({hasText:'Conditions or metadata checks changed.'}).waitFor();
 if(await cc.getByRole('button',{name:'Resume and send pending messages',exact:true}).count())throw new Error('stale resume confirmation survived 409');
 if(conflictPosts!==1)throw new Error('resume was automatically retried');
 for(const width of [390,1280])for(const theme of ['Light','Dark']){
  await conflict.setViewportSize({width,height:900});if(await conflict.getByRole('button',{name:theme,exact:true}).count())await conflict.getByRole('button',{name:theme,exact:true}).click();
  if(!await conflict.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('conflict hint overflow');
  await conflict.screenshot({path:join(screenshotPath,`owner-resume-conflict-${width}-${theme.toLowerCase()}.png`),fullPage:true});
 }
 await conflict.getByLabel('Language').selectOption('zh');
 await cc.getByRole('alert').filter({hasText:'请重新预览当前恢复条件'}).waitFor();
 await conflict.setViewportSize({width:390,height:900});await conflict.screenshot({path:join(screenshotPath,'owner-resume-conflict-390-zh.png'),fullPage:true});
 await conflict.getByLabel('语言').selectOption('en');
 await cc.getByRole('button',{name:'Preview resume conditions',exact:true}).click();
 if(await cc.getByLabel('I reviewed the fixed relay profile and release version.',{exact:true}).isChecked())throw new Error('fresh preview reused stale declaration');
 if(!await cc.getByRole('button',{name:'Resume and send pending messages',exact:true}).isDisabled())throw new Error('fresh preview skipped reconfirmation');
 if(conflictPosts!==1)throw new Error('fresh preview retried resume');
 await conflict.close();
 // UI-only success fixture: actual commit/rollback and SQLite restart durability
 // are covered in Rust. These requests retain the real signed owner gateway.
 for(const choice of ['reviewed','unverified_accepted']) {
  const page=await browser.newPage({viewport:{width:390,height:900}});
  let stored=null,posted=0;
  const settings=structuredClone(conflictSettings);
  const endpoint=settings.settings.endpoints.find(v=>v.binding.endpoint_id==='ag_new');
  endpoint.paused=true;endpoint.isolation=null;
  await page.route('**/api/settings',r=>r.fulfill({json:settings}));
  await page.route('**/api/configuration',r=>{
   const command=r.request().postDataJSON();
   if(command.action==='preview_resume')return r.fulfill({json:{...conflictPreview,activation_enabled:true,account_ready:true}});
   if(command.action==='resume'){
    if(command.evidence.isolation!==choice||'isolation_reviewed' in command.evidence)throw new Error('wrong isolation wire choice');
    if(command.evidence.reference!=='isolation untested <script>window.injected=true</script>')throw new Error('reference changed');
    stored=choice;posted++;endpoint.isolation=choice;endpoint.paused=false;
    return r.fulfill({json:{status:'saved',paused:false,revision:4}});
   }
   throw new Error('unexpected choice command');
  });
  await page.goto(config.url);
  const section=page.locator('section').filter({has:page.locator('code').filter({hasText:/^ag_new$/})});
  await section.getByText('Resume endpoint',{exact:true}).click();
  await section.getByRole('button',{name:'Preview resume conditions',exact:true}).click();
  const isolation=section.getByLabel('Isolation state (explicit choice required)',{exact:true});
  if(await isolation.inputValue()!=='')throw new Error('isolation choice defaulted');
  await section.getByLabel('I reviewed the fixed relay profile and release version.',{exact:true}).check();
  await section.getByLabel('Evidence reference (ticket or manifest SHA, no secrets)',{exact:true}).fill('isolation untested <script>window.injected=true</script>');
  const confirm=section.getByLabel('Confirm resuming these exact objects and sending pending messages.',{exact:true});
  if(!await confirm.isDisabled())throw new Error('missing isolation choice armed resume');
  await isolation.selectOption(choice);
  for(const width of [390,1280])for(const theme of ['Light','Dark']) {
   await page.setViewportSize({width,height:900});
   if(await page.getByRole('button',{name:theme,exact:true}).count())await page.getByRole('button',{name:theme,exact:true}).click();
   if(!await page.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('isolation choice overflow');
   await page.screenshot({path:join(screenshotPath,`isolation-${choice}-${width}-${theme.toLowerCase()}.png`),fullPage:true});
  }
  await confirm.check();await section.getByRole('button',{name:'Resume and send pending messages',exact:true}).click();
  await section.getByText('Endpoint resumed. Downstream delivery is not verified.',{exact:true}).waitFor();
  if(posted!==1||stored!==choice)throw new Error('choice not submitted exactly once');
  await page.reload();
  if(choice==='unverified_accepted')await section.getByTestId('isolation-unverified').filter({hasText:'risk accepted by owner'}).waitFor();
  else if(await section.getByTestId('isolation-unverified').count())throw new Error('reviewed badge mislabeled');
  if(await page.evaluate(()=>window.injected))throw new Error('reference executed');
  endpoint.paused=true;await page.reload();
  if(choice==='unverified_accepted')await section.getByTestId('isolation-unverified').waitFor();
  await page.getByLabel('Language').selectOption('zh');
  if(choice==='unverified_accepted')await section.getByText('隔离未核验 · owner 已接受风险',{exact:true}).waitFor();
  await page.close();
 }


 await edit.getByRole('button',{name:'Light',exact:true}).click();
 await card.getByText('Recovery and audit',{exact:true}).click();
 await card.getByRole('button',{name:'Load recovery records',exact:true}).click();
 await card.getByText('No parked transfers.',{exact:true}).waitFor();
 await card.getByRole('heading',{name:'Hub owner audit',exact:true}).waitFor();
 await card.getByText('create_binding',{exact:false}).waitFor();
 if(await card.getByRole('button',{name:'Permanently discard',exact:true}).count())throw new Error('empty recovery offered destructive action');
 const recoveryCard=edit.locator('section').filter({has:edit.locator('code').filter({hasText:/^ag_demo$/})});
 await recoveryCard.getByText('Recovery and audit',{exact:true}).click();
 await recoveryCard.getByRole('button',{name:'Load recovery records',exact:true}).click();
 await recoveryCard.getByText('fixture_transfer',{exact:true}).waitFor();
 if(!await edit.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('parked recovery overflow at 390px');
 if(!await recoveryCard.locator('.recovery button,.recovery input[type=checkbox]').evaluateAll(elements=>elements.filter(el=>el.getBoundingClientRect().height>0).every(el=>el.matches('input')||el.getBoundingClientRect().height>=44)))throw new Error('recovery touch target below 44px');
 await edit.screenshot({path:join(screenshotPath,'owner-recovery-parked-390-light.png'),fullPage:true});
 const discard=recoveryCard.getByRole('button',{name:'Permanently discard',exact:true});
 if(!await discard.isDisabled())throw new Error('discard did not require explicit confirmation');
 await recoveryCard.getByLabel('Confirm retrying this transfer’s unsent chunks.',{exact:true}).check();
 if(!await discard.isDisabled())throw new Error('retry confirmation armed irreversible discard');
 await recoveryCard.getByLabel('Confirm permanently deleting this transfer’s unsent content. This cannot be undone.',{exact:true}).check();
 await discard.click();await recoveryCard.getByText('Updated. The endpoint remains paused.',{exact:true}).waitFor();
 try {await recoveryCard.getByText('No parked transfers.',{exact:true}).waitFor({timeout:8000});} catch(error) {await edit.screenshot({path:join(screenshotPath,'recovery-failure.png'),fullPage:true});console.error('Synthetic recovery diagnostic:',await recoveryCard.innerText());throw error;}
 await recoveryCard.getByText('discard_parked',{exact:false}).waitFor();
 if((await edit.locator('body').innerText()).includes('PRIVATE RECOVERY BODY'))throw new Error('recovery exposed message body');
 // This driver compresses many distinct owner flows into one source address.
 // Keep the real 20/10s verification budget; wait its window before archive flow.
 await new Promise(resolve=>setTimeout(resolve,10500));
 await recoveryCard.getByText('Archive and clear metadata',{exact:true}).click();
 await recoveryCard.getByRole('button',{name:'Prepare metadata archive',exact:true}).click();
 const clearArchive=recoveryCard.getByRole('button',{name:'Clear archived records',exact:true});
 await recoveryCard.getByRole('button',{name:'Download metadata JSONL',exact:true}).waitFor();
 if(!await clearArchive.isDisabled())throw new Error('archive clear did not require saved and irreversible confirmations');
 const archived=recoveryCard.getByLabel('I have saved and checked this JSONL archive.',{exact:true});
 if(!await archived.isDisabled())throw new Error('save declaration enabled before download request');
 const downloadEvent=edit.waitForEvent('download');await recoveryCard.getByRole('button',{name:'Download metadata JSONL',exact:true}).click();
 const downloaded=await downloadEvent,downloadPath=join(home,'metadata-export.json');await downloaded.saveAs(downloadPath);
 const bytes=readFileSync(downloadPath),exported=bytes.toString('utf8').trimEnd().split('\n').map(line=>JSON.parse(line));
 const digest=createHash('sha256').update(bytes).digest('hex');
 if(exported[0].version!==1||exported[0].kind!=='receipts'||exported[1]?.delivery_id!=='fixture_receipt'||'body' in exported[1]||!downloaded.suggestedFilename().includes(digest))throw new Error('wrong metadata download or checksum');
 await archived.check();if(!await clearArchive.isDisabled())throw new Error('saved confirmation alone armed permanent clear');
 await recoveryCard.getByLabel('Permanently clear these archived records. This cannot be undone.',{exact:true}).check();
 if(!await edit.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('archive overflow at 390px');
 await edit.screenshot({path:join(screenshotPath,'owner-archive-390-light.png'),fullPage:true});
 await edit.getByRole('button',{name:'Dark',exact:true}).click();
 await edit.screenshot({path:join(screenshotPath,'owner-archive-390-dark.png'),fullPage:true});
 await edit.getByRole('button',{name:'Light',exact:true}).click();
 await clearArchive.click();await recoveryCard.getByText('Confirmed records cleared. Endpoints remain paused.',{exact:true}).waitFor();
 await recoveryCard.getByLabel('Archive scope',{exact:false}).selectOption('audit');
 await recoveryCard.getByRole('button',{name:'Prepare metadata archive',exact:true}).click();
 await recoveryCard.getByRole('button',{name:'Download metadata JSONL',exact:true}).waitFor();
 if(await recoveryCard.getByRole('button',{name:'Clear archived records',exact:true}).count())throw new Error('audit offered evidence deletion');

 if(!await edit.locator('body').evaluate(el=>el.scrollWidth<=innerWidth))throw new Error('editing form overflow at 390px');
 await edit.screenshot({path:join(screenshotPath,'owner-edit-390-light.png'),fullPage:true});
 await edit.close();
 console.log('PASS: real hub, injected identity on wildcard + actual iframe prefix; 8 layout/theme screenshots + 390px confirmed owner create/bind/pause/default-off-resume/recovery/archive; fallback + forged rejection; UI-only 409 clears stale confirmation and requires re-preview (no auto retry)');
} finally {
 clearTimeout(timer);if(browser)await browser.close();fixture.kill('SIGTERM');
 if(fixture.exitCode===null)await once(fixture,'exit');lines.close();rmSync(home,{recursive:true,force:true});
 if(stderr)console.error('fixture diagnostic:',stderr.slice(0,1000));
}
