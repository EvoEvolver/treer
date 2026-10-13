// Local Policy UI regression: no deployed service or real credential.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
const {chromium}=await import(process.env.APPS_SECURITY_PLAYWRIGHT_MODULE || 'playwright');
const policyRoot=fileURLToPath(new URL('../../policy',import.meta.url));
const credential='synthetic-policy-admin-credential-32-chars';
const fixture=`
import sys,tempfile,json
from pathlib import Path
from http.server import ThreadingHTTPServer
sys.path.insert(0,sys.argv[1])
import policy
with tempfile.TemporaryDirectory() as root:
 policy.PolicyHandler.store=policy.PolicyStore(Path(root)/'policy.json','ws_test')
 server=ThreadingHTTPServer(('127.0.0.1',0),policy.PolicyHandler)
 server.admin_token='synthetic-policy-admin-credential-32-chars'
 print(json.dumps({'url':'http://127.0.0.1:%s/' % server.server_address[1]}),flush=True)
 server.serve_forever()
`;
for(const width of [1280,390]) test(`Policy guarded writes and literal rendering ${width}px`,async()=>{
 const child=spawn('python3',['-u','-c',fixture,policyRoot],{env:{PATH:process.env.PATH,HOME:'/nonexistent',PYTHONUTF8:'1'},stdio:['ignore','pipe','pipe']});
 let errors='';child.stderr.on('data',b=>errors+=b);
 const {url}=await new Promise((resolve,reject)=>{
  createInterface({input:child.stdout}).once('line',line=>resolve(JSON.parse(line)));
  child.once('exit',()=>reject(new Error(errors)));
 });
 const browser=await chromium.launch({env:{PATH:process.env.PATH,HOME:'/tmp'}});
 try {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:width===390?'dark':'light'});
  const failures=[];page.on('pageerror',e=>failures.push(e.message));
  await page.goto(url);await page.locator('#header-revision').filter({hasText:'rev 1'}).waitFor();
  await page.locator('[data-view="editor"]').click();
  await page.locator('#publish-button').click();
  await page.locator('#editor-error').filter({hasText:'credential required'}).waitFor();
  await page.locator('#admin-token').fill(credential);
  await page.locator('#publish-button').click();
  await page.locator('#header-revision').filter({hasText:'rev 2'}).waitFor();
  assert.equal(await page.evaluate(()=>localStorage.length+sessionStorage.length),0);
  assert.equal(await page.locator('#admin-token').getAttribute('type'),'password');
  await page.route('**/v1/policy/test',route=>route.fulfill({json:{decision:'allow',revision:2,effect:'allow',mode:'enforce',matched_rule_id:'<img src=x onerror=alert(1)>'}}));
  await page.locator('[data-view="simulator"]').click();await page.locator('#test-button').click();
  await page.locator('#decision p').filter({hasText:'<img src=x onerror=alert(1)>'}).waitFor();
  assert.equal(await page.locator('#decision img').count(),0);
  assert.deepEqual(failures,[]);
  await mkdir('/tmp/apps-security-browser',{recursive:true});
  await page.screenshot({path:`/tmp/apps-security-browser/policy-${width}.png`,fullPage:true});
 }finally {await browser.close();child.kill();}
});

const mailFixture=fileURLToPath(new URL('./mail_fixture.py',import.meta.url));
for(const width of [1280,390]) for(const tunnel of [false,true]) test(`Mail injected human ${tunnel?'iframe':'ingress'} ${width}px`,async()=>{
 const child=spawn('python3',['-u',mailFixture],{env:{PATH:process.env.PATH,HOME:'/nonexistent',PYTHONUTF8:'1'},stdio:['ignore','pipe','pipe']});
 let errors='';child.stderr.on('data',b=>errors+=b);
 const {url}=await new Promise((resolve,reject)=>{
  createInterface({input:child.stdout}).once('line',line=>resolve(JSON.parse(line)));
  child.once('exit',()=>reject(new Error(errors)));
 });
 const browser=await chromium.launch({env:{PATH:process.env.PATH,HOME:'/tmp'}});
 try {
  const page=await browser.newPage({viewport:{width,height:900},colorScheme:width===390?'dark':'light'});
  const failures=[];page.on('pageerror',e=>failures.push(e.message));
  const oauth=[];page.on('request',r=>{if(r.url().includes('oauth') || r.url().includes('auth/start'))oauth.push(r.url());});
  await page.goto(url+(tunnel?'control':''));
  const scope=tunnel?page.frameLocator('iframe'):page;
  await scope.locator('.message-body').filter({hasText:'<img src=x onerror=alert(1)>'}).waitFor();
  assert.equal(await scope.getByText('Continue with Treer').count(),0);
  assert.equal(await scope.locator('img').count(),0);
  assert.deepEqual(oauth,[]);assert.deepEqual(failures,[]);
  await page.screenshot({path:`/tmp/apps-security-browser/mail-${width}-${tunnel?'iframe':'ingress'}.png`,fullPage:true});
 }finally {await browser.close();child.kill();}
});
