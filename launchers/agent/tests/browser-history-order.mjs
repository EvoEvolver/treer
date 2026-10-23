// Built UI + actual Rust AIS + producer-shaped synthetic history/local receipts.
// No managed Agent, provider-private history, credentials or installed lifecycle.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,readFile,realpath,rm,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {chromium,expect} from '@playwright/test';
import {fakePrivateState} from './fake-private-state.mjs';
import {hermeticEnv,localFetch as fetch} from './hermetic.mjs';

assert.ok(process.env.RUST_LAUNCHER_BIN,'fixture Rust image required, never a shipping image');
const app=resolve(import.meta.dirname,'..');
const root=await realpath(await mkdtemp(join(tmpdir(),'history239-')));
const cwd=join(root,'cwd'),state=join(root,'state'),port=join(root,'port');
await mkdir(cwd);await mkdir(state,{mode:0o700});
const controller=await fakePrivateState('ag_history_fixture','fixture-only');
const env=hermeticEnv({HOME:root,CODEX_HOME:join(root,'codex'),TREER_AGENT_ID:'ag_history_fixture',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,TREER_AGENT_PRIVATE_STATE_DIR:state,
  AGENT_UI_CWD:cwd,AGENT_UI_PUBLIC_DIR:join(app,'public'),AGENT_UI_PORT_FILE:port,AGENT_UI_PORT:'0',AGENT_UI_INSTANCE_ID:'history-fixture',AGENT_UI_MESSAGE_WATCH:'0',RUST_SPIKE_REGISTER:'0',
  CODEX_BIN:resolve(app,'../agent-rust/tests/fixtures/history-order-provider.mjs'),ORDER_HISTORY_FILE:join(root,'provider-history.json')});
const children=[];let browser,logs='',base;
const until=async(read,predicate)=>{for(let n=0;n<300;n++){const value=await read();if(predicate(value))return value;await new Promise(r=>setTimeout(r,30));}throw Error('history239 deadline '+logs);};
const launch=async()=>{
  const child=spawn(process.env.RUST_LAUNCHER_BIN,[],{cwd,env,stdio:['ignore','pipe','pipe']});children.push(child);
  child.stdout.resume();child.stderr.on('data',b=>{logs=(logs+b).slice(-4096);});
  await until(async()=>{if(child.exitCode!==null)throw Error(logs);try{return Number(await readFile(port,'utf8'));}catch{return 0;}},Boolean);
  base='http://127.0.0.1:'+Number(await readFile(port,'utf8'));
  await until(()=>fetch(base+'/api/state').then(r=>r.json()),s=>s.ready);
  return child;
};
const headers={'x-treer-interface-source':'controller-api','x-treer-agent-id':'ag_history_fixture','x-treer-interface-instance':'history-fixture',host:'agent-interface'};
const post=async body=>{const response=await fetch(base+'/v1/prompts',{method:'POST',headers,body:JSON.stringify(body)});assert.equal(response.status,202);return response.json();};
const report=[];
try {
  let child=await launch();
  const first=await post({text:'First local receipt',operation_id:'browser-first'});
  await until(()=>fetch(base+'/api/state').then(r=>r.json()),s=>s.thread.turns.at(-1)?.items.some(i=>i.kind==='commandExecution'));
  const steering=await post({text:'Steering after observed tool',delivery:'steer',operation_id:'browser-steer'});
  await until(()=>fetch(base+'/api/state').then(r=>r.json()),s=>s.thread.turns.at(-1)?.status==='completed');
  browser=await chromium.launch({headless:true});
  const check=async(page)=>{
    await expect(page.locator('.chat')).toContainText('assistant after steering');
    await expect(page.locator('.chat .msg.user')).toHaveCount(14);
    const messages=await page.locator('.chat .msg').allTextContents();
    for(let n=0;n<12;n++){assert.ok(messages[n*2].includes('past question '+n));assert.ok(messages[n*2+1].includes('past answer '+n));}
    const items=await page.locator('.chat .msg,.chat .tool .n').allTextContents();
    const order=['First local receipt','assistant before steering','synthetic read only','Steering after observed tool','assistant after steering'].map(text=>items.findIndex(item=>item.includes(text)));
    assert.ok(order.every((at,i)=>at>=0&&(!i||at>order[i-1])),JSON.stringify({items,order}));
    await expect(page.locator('h1')).not.toContainText('Rust experiment');
    await expect.poll(()=>page.locator('#thread').evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop)).toBeLessThan(24);
  };
  for(const width of [1280,390]) {
    const context=await browser.newContext({viewport:{width,height:900},reducedMotion:'reduce',extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_history_fixture','x-treer-interface-instance':'history-fixture'}});
    try {
      const page=await context.newPage(),errors=[];page.on('pageerror',e=>errors.push(e.message));
      await page.goto(base+'/?lang='+(width===390?'zh':'en'));await check(page);
      await page.reload();await check(page);
      if(process.env.HISTORY_SCREENSHOTS)await page.screenshot({path:join(process.env.HISTORY_SCREENSHOTS,'actual-history-bottom-'+width+'.png')});
      const thread=page.locator('#thread');await thread.hover();await page.mouse.wheel(0,-700);
      await expect.poll(()=>thread.evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop)).toBeGreaterThan(200);
      const before=await thread.evaluate(e=>e.scrollTop);
      // A same-state HTTP reload must not override an intentional scroll-up.
      await page.waitForTimeout(300);assert.ok(Math.abs((await thread.evaluate(e=>e.scrollTop))-before)<40);
      assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));assert.deepEqual(errors,[]);
      if(process.env.HISTORY_SCREENSHOTS)await page.screenshot({path:join(process.env.HISTORY_SCREENSHOTS,'actual-history-'+width+'.png')});
      report.push({width,actualRustAIS:true,providerLocalReceiptOrder:true,duplicateUsers:false,initialBottom:true,refreshBottom:true,userScrollPreserved:true});
    } finally {await context.close();}
  }
  const exit=once(child,'exit');child.kill('SIGTERM');assert.equal((await exit)[0],0);await rm(port);
  env.AGENT_UI_SESSION_ID='order-thread';child=await launch();
  const context=await browser.newContext({extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_history_fixture','x-treer-interface-instance':'history-fixture'}});
  try {const page=await context.newPage();await page.goto(base+'/?lang=en');await check(page);}finally{await context.close();}
  const snapshot=await fetch(base+'/api/state').then(r=>r.json());
  const users=snapshot.thread.turns.flatMap(t=>t.items).filter(i=>i.submissionId);
  assert.deepEqual(users.map(i=>i.submissionId),[first.submission.id,steering.submission.id]);
  report.push({restart:true,receiptOrderAndIds:true,originalThread:true});
  if(process.env.HISTORY_RESULT)await writeFile(process.env.HISTORY_RESULT,JSON.stringify(report,null,2));
  console.log(JSON.stringify(report));
} finally {
  await browser?.close();
  for(const child of children)if(child.exitCode===null&&child.signalCode===null){const exit=once(child,'exit');child.kill('SIGTERM');await exit;}
  await controller.close();await rm(root,{recursive:true,force:true});
}
