import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {createRequire} from 'node:module';
import {mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
const require=createRequire(import.meta.url);
const {chromium}=require(process.env.TELEGRAM_PLAYWRIGHT_MODULE||'playwright');
const screenshots=process.env.HTTP_SCREENSHOT_DIR||'/tmp/treer-http-142-screenshots';
const clean=()=>Object.fromEntries(Object.entries(process.env).filter(([k])=>!/^(TREER_|AGENT_UI_|CODEX_UI_|CODEX_)/.test(k)&&!/^(http_proxy|https_proxy|all_proxy|no_proxy)$/i.test(k)));
async function start(){
 const fixture=spawn('python3',['-I',fileURLToPath(new URL('./browser_fixture.py',import.meta.url))],{env:clean(),stdio:['ignore','pipe','pipe']});
 let stderr='';fixture.stderr.on('data',v=>stderr+=v);const lines=createInterface({input:fixture.stdout});
 const [line]=await once(lines,'line');return {...JSON.parse(line),fixture,lines,stderr:()=>stderr};
}
for(const width of [1280,390])for(const tunnel of [false,true])test(`HTTP settings and two queues ${width}px ${tunnel?'tunnel':'workspace ingress'}`,async()=>{
 const f=await start();const browser=await chromium.launch({env:clean()});
 try{
  const context=await browser.newContext({viewport:{width,height:900}});const page=await context.newPage();page.setDefaultTimeout(8000);
  const errors=[];page.on('pageerror',e=>errors.push(e.message));page.on('dialog',d=>d.accept());
  // No browser-supplied identity: the gateway injects it on every request.
  await page.goto(f.url+(tunnel?f.prefix:'/workspace/'));
  await page.locator('#identity').filter({hasText:'owner'}).waitFor();
  assert.equal(await page.getByText(/Sign in|OAuth/).count(),0);
  assert.match(await page.locator('#poc').innerText(),/\/one\/Same name/);assert.match(await page.locator('#poc').innerText(),/\/two\/Same name/);
  const hostile='<img src=x onerror=window.xss=1>';
  await page.locator('#label').fill(hostile);await page.locator('#poc').selectOption('ag_poc');
  await page.locator('#create').click();await page.locator('#code-box').waitFor({state:'visible'});
  const code=await page.locator('#pair-code').innerText();assert.match(code,/^[a-f0-9]{32}\.[A-Z0-9]{10}$/);
  assert.equal(await page.evaluate(()=>document.activeElement.id),'pair-code');
  assert.equal(await page.locator('#clients img').count(),0);assert.equal(await page.evaluate(()=>window.xss),undefined);
  async function request(path,body,token){const r=await fetch(f.url+'/public/'+path,{method:body===undefined?'GET':'POST',headers:{...(body===undefined?{}:{'Content-Type':'application/json'}),...(token?{Authorization:'Bearer '+token}:{})},body:body===undefined?undefined:JSON.stringify(body)});return {status:r.status,value:await r.json()};}
  const paired=await request('v1/pair',{code});assert.equal(paired.status,200);const token=paired.value.token;
  const input=await request('v1/inbox',{body:'Hello from outside',idempotency_key:'fixture'},token);assert.equal(input.status,202);
  const output=await request('v1/outbox?timeout=5',undefined,token);assert.equal(output.status,200);assert.equal(output.value.items[0].body,'Hello from POC');
  assert.equal((await request('v1/outbox/ack',{up_to:output.value.items[0].seq},token)).status,200);
  assert.equal((await request('v1/outbox',undefined,token)).value.items.length,0);
  await page.locator('#dismiss').click();assert.equal(await page.locator('#pair-code').innerText(),'');assert.equal(await page.evaluate(()=>document.activeElement.id),'create');
  await page.reload();await page.locator('#clients article').waitFor();assert(!(await page.content()).includes(code));assert(!(await page.content()).includes(token));
  assert.equal(await page.evaluate(()=>JSON.stringify([localStorage,sessionStorage]).includes('.'+document.getElementById('pair-code').textContent)&&!!document.getElementById('pair-code').textContent),false);
  await mkdir(screenshots,{recursive:true});
  await page.screenshot({path:`${screenshots}/${width}-${tunnel?'tunnel':'ingress'}-light.png`,fullPage:true});
  await page.locator('#language').selectOption('zh');await page.getByRole('heading',{name:'HTTP 消息入口',exact:true}).waitFor();await page.locator('#theme').click();
  assert.equal(await page.evaluate(()=>document.documentElement.dataset.theme),'dark');
  await page.screenshot({path:`${screenshots}/${width}-${tunnel?'tunnel':'ingress'}-dark-zh.png`,fullPage:true});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  await page.locator('#clients').getByRole('button',{name:'撤销',exact:true}).click();await page.getByText(/已撤销 ·/).waitFor();
  assert.equal((await request('v1/outbox',undefined,token)).status,401);
  assert.equal((await request('',undefined,token)).status,404);
  assert.deepEqual(errors,[]);assert.equal(f.stderr(),'');
 }finally{await browser.close();f.fixture.kill('SIGTERM');await once(f.fixture,'exit');f.lines.close();}
});

test('public forwarding fixture preserves a real 30-second poll and pause interrupts it',async()=>{
 const f=await start();
 try{
  const control=(path,body)=>fetch(f.url+'/workspace/'+path,{method:'POST',headers:{'Content-Type':'application/json','X-Http-Settings':'1',Origin:f.url},body:JSON.stringify(body)});
  const made=await (await control('v1/pairings',{label:'30-second poll',poc:'ag_poc'})).json();
  const pair=await (await fetch(f.url+'/public/v1/pair',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({code:made.code})})).json();
  const headers={Authorization:'Bearer '+pair.token};const started=Date.now();
  const r=await fetch(f.url+'/public/v1/outbox?timeout=30',{headers});assert.equal(r.status,200);assert.deepEqual((await r.json()).items,[]);
  assert(Date.now()-started>=29500);assert(Date.now()-started<35000);
  const waiting=fetch(f.url+'/public/v1/outbox?timeout=30',{headers});
  await new Promise(resolve=>setTimeout(resolve,300));
  assert.equal((await control('v1/pause',{paused:true})).status,200);
  assert.equal((await waiting).status,503);
 }finally{f.fixture.kill('SIGTERM');await once(f.fixture,'exit');f.lines.close();}
});

test('workspace UI opens inside the real iframe prefix without a login click',async()=>{
 const f=await start();const browser=await chromium.launch({env:clean()});
 try{
  const page=await browser.newPage({viewport:{width:390,height:900}});
  await page.goto(f.url+'/console');const frame=page.frameLocator('iframe');
  await frame.locator('#identity').filter({hasText:'owner'}).waitFor();
  await frame.locator('#label').fill('Embedded client');await frame.locator('#poc').selectOption('ag_poc');
  await frame.locator('#create').click();await frame.locator('#code-box').waitFor({state:'visible'});
  assert.equal(await frame.getByText(/Sign in|OAuth/).count(),0);
  assert.equal(await page.evaluate(()=>document.cookie),'');
 }finally{await browser.close();f.fixture.kill('SIGTERM');await once(f.fixture,'exit');f.lines.close();}
});
