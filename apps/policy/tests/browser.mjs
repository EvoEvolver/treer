// Synthetic loopback services only; no inherited Treer credentials/config.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.POLICY_PLAYWRIGHT_MODULE || 'playwright');
const fixture=fileURLToPath(new URL('./browser_fixture.py',import.meta.url));
for(const width of [1280,390]) for(const tunnel of [false,true]) test(`Policy v2 ${tunnel?'tunnel':'ingress'} ${width}px`,async()=>{
 const child=spawn('python3',['-u',fixture],{env:{PATH:process.env.PATH,HOME:'/nonexistent',PYTHONUTF8:'1'},stdio:['ignore','pipe','pipe']});
 let errors='';child.stderr.on('data',b=>errors+=b);
 const {url}=await new Promise((resolve,reject)=>{createInterface({input:child.stdout}).once('line',line=>resolve(JSON.parse(line)));child.once('exit',()=>reject(new Error(errors)));});
 let browser;
 try {
  browser=await chromium.launch({env:{PATH:process.env.PATH,HOME:'/tmp'}});
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:width===390?'dark':'light'});
  const failures=[];page.on('pageerror',e=>failures.push(e.message));
  page.on('dialog',dialog=>dialog.accept());
  await page.goto(url+(tunnel?'control':''));const ui=tunnel?page.frameLocator('iframe'):page;
  await ui.locator('#identity-status').filter({hasText:'Treer: Owner'}).waitFor();
  assert.equal(await ui.locator('#login-link').isVisible(),false);
  await ui.locator('#publish-authority').filter({hasText:'Owner session'}).waitFor();
  await ui.locator('[data-view="templates"]').click();
  await ui.locator('#template-actor').fill('/team');
  await ui.locator('#template-list button').first().click();
  await ui.locator('#rule-list h3').filter({hasText:'same-folder'}).waitFor();
  await ui.locator('#admin-token').fill('synthetic-operator-secret-at-least-32-chars');
  await ui.locator('#validate-button').click();await ui.locator('#toast').filter({hasText:'Draft validated'}).waitFor();
  await ui.locator('#rule-id').fill('<img src=x onerror=alert(1)>');
  await ui.locator('#rule-actor-path').fill('/team/**');
  await ui.locator('#rule-relation').selectOption('same_folder');
  await ui.locator('#apply-rule').click();
  assert.equal(await ui.locator('#rule-list img').count(),0);
  await ui.locator('#publish-button').click();await ui.locator('#header-revision').filter({hasText:'rev 2'}).waitFor();
  await ui.locator('[data-view="simulator"]').click();
  await ui.locator('#actor-path').fill('/team/agent');await ui.locator('#target-path').fill('/team/reviewer');
  await ui.locator('#test-button').click();await ui.locator('#decision p').filter({hasText:'computed deny'}).waitFor();
  await ui.locator('[data-view="logs"]').click();await ui.locator('#logs-list').filter({hasText:'agent.prompt'}).waitFor();
  await ui.locator('[data-view="history"]').click();
  await ui.getByRole('button',{name:'Restore revision 1',exact:true}).click();await ui.locator('#header-revision').filter({hasText:'rev 3'}).waitFor();
  assert.equal(await ui.locator('#metric-mode').textContent(),'monitor');
  assert.deepEqual(failures,[]);
  const documentSize=await (tunnel?page.frames()[1]:page).evaluate(()=>({width:document.documentElement.clientWidth,scroll:document.documentElement.scrollWidth,storage:localStorage.length+sessionStorage.length}));
  assert.ok(documentSize.scroll<=documentSize.width,JSON.stringify(documentSize));assert.equal(documentSize.storage,0);
  await mkdir('/tmp/policy-path-browser',{recursive:true});
  await page.screenshot({path:`/tmp/policy-path-browser/policy-${width}-${tunnel?'tunnel':'ingress'}.png`,fullPage:true});
  await ui.locator('[data-view="editor"]').click();
  await page.screenshot({path:`/tmp/policy-path-browser/editor-${width}-${tunnel?'tunnel':'ingress'}.png`,fullPage:true});
  await page.goto(url+'fallback/');await page.locator('#login-link').waitFor({state:'visible'});
 } finally {await browser?.close();child.kill();}
});

for(const width of [1280,390]) test(`Operator authority warning and member publication ${width}px`,async()=>{
 const child=spawn('python3',['-u',fixture,'--operator'],{env:{PATH:process.env.PATH,HOME:'/nonexistent',PYTHONUTF8:'1'},stdio:['ignore','pipe','pipe']});
 let errors='';child.stderr.on('data',b=>errors+=b);
 const {url}=await new Promise((resolve,reject)=>{createInterface({input:child.stdout}).once('line',line=>resolve(JSON.parse(line)));child.once('exit',()=>reject(new Error(errors)));});
 let browser;
 try {
  browser=await chromium.launch({env:{PATH:process.env.PATH,HOME:'/tmp'}});
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:width===390?'dark':'light'});
  const failures=[];page.on('pageerror',e=>failures.push(e.message));
  await page.goto(url);
  await page.locator('#identity-status').filter({hasText:'Treer: Member'}).waitFor();
  await page.locator('#publish-authority').filter({hasText:'Provider operator'}).waitFor();
  await page.locator('#authority-warning').filter({hasText:'without further human approval'}).waitFor();
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
  await mkdir('/tmp/policy-operator-browser',{recursive:true});
  await page.screenshot({path:`/tmp/policy-operator-browser/authority-${width}.png`,fullPage:true});
  await page.locator('[data-view="templates"]').click();
  await page.locator('#template-list button').first().click();
  await page.locator('#admin-token').fill('synthetic-operator-secret-at-least-32-chars');
  await page.locator('#publish-button').click();
  await page.locator('#header-revision').filter({hasText:'rev 2'}).waitFor();
  await page.locator('[data-view="simulator"]').click();
  await page.locator('#actor-path').fill('/team/editor');await page.locator('#target-path').fill('/team/worker');
  await page.locator('#test-button').click();await page.locator('#decision p').filter({hasText:'operator-rule'}).waitFor();
  assert.deepEqual(failures,[]);
  assert.equal(await page.evaluate(()=>localStorage.length+sessionStorage.length),0);
 } finally {await browser?.close();child.kill();}
});
