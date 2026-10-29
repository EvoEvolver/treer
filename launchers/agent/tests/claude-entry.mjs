import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,readFile,realpath,rm,writeFile,symlink,link} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {access} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {createRequire} from 'node:module';
import {hermeticEnv,localFetch as fetch} from './hermetic.mjs';
import {fakePrivateState} from './fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
async function until(read,accept,waitMs=10000){const end=Date.now()+waitMs;while(Date.now()<end){try{const v=await read();if(accept(v))return v;}catch{}await new Promise(r=>setTimeout(r,20));}throw Error('Claude fixture deadline');}
async function fixture(t,extra={},setup=async()=>({})){
  const root=await realpath(await mkdtemp(join(tmpdir(),'rust231-'))),cwd=join(root,'cwd'),state=join(root,'private'),port=join(root,'port');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const initial=await setup({root,cwd,state});
  const children=[];let logs='';
  const launch=async(overrides={},args=[])=>{
    await rm(port,{force:true});
    const env=hermeticEnv({HOME:root,AGENT_UI_CWD:cwd,TREER_RUST_SPIKE_STATE_DIR:state,AGENT_UI_PUBLIC_DIR:resolve('public'),AGENT_UI_PORT_FILE:port,AGENT_UI_PORT:'0',AGENT_UI_HARNESS:'claude',CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-stream.mjs')]),CLAUDE_FIXTURE_LOG:join(root,'calls'),...extra,...overrides});
    env.RUST_SPIKE_REGISTER='0';
    const child=spawn(binary,args,{cwd,env,stdio:['ignore','pipe','pipe']});children.push(child);
    child.stderr.on('data',b=>logs+=b);child.stdout.on('data',b=>logs+=b);
    let base;try{base=await until(async()=>`http://127.0.0.1:${Number(await readFile(port,'utf8'))}`,v=>!v.endsWith('NaN'));}catch(error){throw Error(error.message+' '+logs);}
    const snapshot=()=>fetch(base+'/api/state').then(r=>r.json());
    try{await until(snapshot,s=>s.ready);}catch(error){throw Error(error.message+' '+logs);}
    return {child,base,snapshot,post:body=>fetch(base+'/api/prompt',{method:'POST',body:JSON.stringify(body)})};
  };
  t.after(async()=>{for(const c of children)if(c.exitCode===null&&c.signalCode===null){const exit=once(c,'exit');c.kill('SIGTERM');await exit;}await rm(root,{recursive:true,force:true});});
  return {...await launch(initial.env,initial.args),launch,state,root,logs:()=>logs};
}
async function stop(child){const exit=once(child,'exit');child.kill('SIGTERM');assert.equal((await exit)[0],0);}
const inputCalls=async f=>(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
test('Claude scheduled command output preserves queued receipt and next prompt exactly once',async t=>{
 const f=await fixture(t,{RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-scheduled.mjs')])});
 await f.post({prompt:'own setup',mode:'queue'});
 await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
 await writeFile(join(f.root,'cwd/start-scheduled'),'go');
 await until(()=>access(join(f.root,'calls.scheduled')).then(()=>true),Boolean);
 await f.post({prompt:'next ordinary input',mode:'queue'});
 const before=await until(f.snapshot,s=>s.thread.activeTurnId&&s.queue.length===0);
 await writeFile(join(f.root,'cwd/finish-scheduled'),'go');
 await until(()=>access(join(f.root,'calls.result')).then(()=>true),Boolean);
 assert.equal((await (await fetch(f.base+'/v1/queue')).json()).submissions[1].state,'running');
 assert.equal((await f.snapshot()).thread.turns.some(t=>t.historySource==='native-scheduled-command'),false,'result alone does not publish scheduled output');
 await writeFile(join(f.root,'cwd/complete-command'),'go');
 let s=await until(f.snapshot,s=>s.thread.turns.some(t=>t.historySource==='native-scheduled-command'));
 assert.equal(s.thread.activeTurnId,before.thread.activeTurnId);
 assert.equal(s.thread.model,'claude-current');
 assert.equal((await (await fetch(f.base+'/v1/queue')).json()).submissions[1].state,'running');
 const output=s.thread.turns.find(t=>t.historySource==='native-scheduled-command');
 assert.equal(output.projectionOnly,true);assert.equal(output.nativeCommandId,'91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2');
 await writeFile(join(f.root,'cwd/finish-input'),'go');
 s=await until(f.snapshot,s=>s.thread.turns.length===3&&s.thread.status==='idle');
 assert.equal(s.runtime.provider.transport,'open');assert.equal(s.runtime.state,'idle');
 assert.equal((await inputCalls(f)).length,2);
 assert.ok((await (await fetch(f.base+'/v1/queue')).json()).submissions.every(r=>r.state==='done'&&r.error===null));
 await stop(f.child);s=await (await f.launch()).snapshot();
 assert.equal(s.thread.turns.filter(t=>t.historySource==='native-scheduled-command').length,1);
 assert.equal((await inputCalls(f)).length,2);
});
for(const fault of ['NO_COMMAND','BORROWED_INPUT','WRONG_COMMAND'])test('Claude scheduled command fault '+fault+' is skipped without blocking or completing waiting input',async t=>{
 const f=await fixture(t,{RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-scheduled.mjs')]),['CLAUDE_FIXTURE_'+fault]:'1'});
 await f.post({prompt:'setup',mode:'queue'});await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
 await writeFile(join(f.root,'cwd/start-scheduled'),'go');await until(()=>access(join(f.root,'calls.scheduled')).then(()=>true),Boolean);
 if(fault!=='NO_COMMAND'){
  await f.post({prompt:'must not borrow scheduled result',mode:'queue'});await until(f.snapshot,s=>!!s.thread.activeTurnId);
  await writeFile(join(f.root,'cwd/finish-scheduled'),'go');await writeFile(join(f.root,'cwd/complete-command'),'go');
  await until(()=>access(join(f.root,'calls.result')).then(()=>true),Boolean);await new Promise(r=>setTimeout(r,300));
 }
 let s=await f.snapshot();
 assert.notEqual(s.runtime.state,'blocked');assert.equal(s.runtime.provider.transport,'open');
 assert.equal(s.thread.turns.some(t=>t.historySource==='native-scheduled-command'&&t.items.some(i=>i.text==='must not borrow scheduled result')),false);
 let q=(await (await fetch(f.base+'/v1/queue')).json()).submissions;
 assert.equal(q[0].state,'done');if(q[1])assert.equal(q[1].state,'running','the scheduled result never settles waiting input');
 if(fault!=='NO_COMMAND'){
  await writeFile(join(f.root,'cwd/finish-input'),'go');
  await until(async()=>(await (await fetch(f.base+'/v1/queue')).json()).submissions,q=>q[1].state==='done');
 }
 assert.equal((await inputCalls(f)).length,fault==='NO_COMMAND'?1:2,'nothing is replayed');
});
test('Claude task-origin followup never blocks, completes queued input or changes its model',async t=>{
  const f=await fixture(t,{RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-task-followup.mjs')])});
  await f.post({prompt:'original input',mode:'queue'});
  let s=await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
  const first=s.thread.turns[0].id;
  await writeFile(join(f.root,'cwd/start-followup'),'go');
  await until(()=>access(join(f.root,'calls.followup')).then(()=>true),Boolean);
  await f.post({prompt:'queued while native task speaks',mode:'queue'});
  s=await until(f.snapshot,s=>s.thread.activeTurnId&&s.queue.length===0);
  const active=s.thread.activeTurnId;
  assert.notEqual(active,first);assert.equal(s.runtime.state,'working');
  await writeFile(join(f.root,'cwd/finish-followup'),'go');
  s=await until(f.snapshot,s=>s.thread.turns.some(t=>t.historySource==='native-task-notification'));
  const pending=await (await fetch(f.base+'/v1/queue')).json();
  assert.equal(pending.submissions[1].state,'running','task result must not settle queued user input');
  assert.equal(s.thread.activeTurnId,active);assert.equal(s.runtime.state,'working');
  await writeFile(join(f.root,'cwd/finish-input'),'go');
  s=await until(f.snapshot,s=>s.thread.turns.length===3&&s.thread.status==='idle');
  assert.equal(s.runtime.state,'idle');assert.equal(s.runtime.provider.transport,'open');
  const output=s.thread.turns.find(t=>t.historySource==='native-task-notification');
  assert.ok(output);assert.equal(output.projectionOnly,true);assert.equal(output.taskRunId,'own-run');
  assert.equal(output.items.some(i=>i.kind==='userMessage'),false);
  assert.equal(s.thread.model,'claude-current','task output must not change selected model');
  const receipts=await (await fetch(f.base+'/v1/queue')).json();
  assert.equal(receipts.submissions.length,2);
  assert.ok(receipts.submissions.every(r=>r.state==='done'&&r.error===null));
  assert.deepEqual(new Set(receipts.submissions.map(r=>r.turn_id)),new Set([first,active]));
  assert.equal((await inputCalls(f)).length,2);
  const thread=s.thread.id;await stop(f.child);const resumed=await f.launch();
  s=await resumed.snapshot();assert.equal(s.thread.id,thread);
  assert.equal(s.thread.turns.filter(t=>t.historySource==='native-task-notification').length,1);
  assert.equal((await inputCalls(f)).length,2,'restart never replays task followup or user input');
});
for(const fault of ['BAD_TASK_ORIGIN','UNSOLICITED'])test('Claude task-followup fault '+fault+' is skipped without blocking, replay or false settlement',async t=>{
  const f=await fixture(t,{RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-task-followup.mjs')]),['CLAUDE_FIXTURE_'+fault]:'1'});
  await f.post({prompt:'original input',mode:'queue'});
  await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  await writeFile(join(f.root,'cwd/start-followup'),'go');
  await until(()=>access(join(f.root,'calls.followup')).then(()=>true),Boolean);
  if(fault==='BAD_TASK_ORIGIN') {
    await f.post({prompt:'never settled by wrong task result',mode:'queue'});
    await until(f.snapshot,s=>!!s.thread.activeTurnId);
    await writeFile(join(f.root,'cwd/finish-followup'),'go');
  }
  await new Promise(r=>setTimeout(r,300));
  const s=await f.snapshot();
  assert.notEqual(s.runtime.state,'blocked');assert.equal(s.runtime.provider.transport,'open');
  assert.equal(s.thread.turns.some(t=>t.historySource==='native-task-notification'),false);
  let receipts=await (await fetch(f.base+'/v1/queue')).json();
  assert.equal(receipts.submissions[0].state,'done');
  if(fault==='BAD_TASK_ORIGIN'){
    assert.equal(receipts.submissions[1].state,'running','a wrong-origin result never settles user input');
    await writeFile(join(f.root,'cwd/finish-input'),'go');
    receipts=await until(async()=>(await fetch(f.base+'/v1/queue')).json(),r=>r.submissions[1].state==='done');
  }
  assert.equal((await inputCalls(f)).length,fault==='BAD_TASK_ORIGIN'?2:1);
});
test('Claude unknown frames are skipped and permission requests are allowed without blocking',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_ODDITIES:'1'});
  assert.equal((await f.post({prompt:'odd frames',mode:'queue'})).status,200);
  const s=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  assert.equal(s.runtime.state,'idle');assert.equal(s.runtime.provider.transport,'open');
  assert.equal((await (await fetch(f.base+'/v1/queue')).json()).submissions[0].state,'done');
  const reply=await until(async()=>JSON.parse((await readFile(join(f.root,'calls.replies'),'utf8')).trim().split('\n')[0]),Boolean);
  assert.equal(reply.response.request_id,'perm-1');assert.equal(reply.response.response.behavior,'allow');
  assert.deepEqual(reply.response.response.updatedInput,{command:'synthetic'});
});
test('Claude provider crash restarts the provider, resumes the session and keeps the queue moving',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_CRASH_ONCE:'crash-marker',CLAUDE_FIXTURE_SPAWN_LOG:'spawns'});
  const thread=(await f.snapshot()).thread.id;
  assert.equal((await f.post({prompt:'crashes the provider',mode:'queue'})).status,200);
  let s=await until(f.snapshot,s=>s.runtime.recovery.restarts>=1&&s.ready&&s.runtime.state==='idle',15000);
  assert.equal(s.thread.id,thread);assert.equal(s.runtime.recovery.state,'running');
  const first=(await (await fetch(f.base+'/v1/queue')).json()).submissions[0];
  assert.equal(first.state,'interrupted','a prompt running in the dead process is reported, not replayed');
  assert.equal((await f.post({prompt:'after the restart',mode:'queue'})).status,200);
  s=await until(f.snapshot,s=>s.thread.status==='idle'&&s.queue.length===0&&s.thread.turns.length===2);
  assert.equal((await (await fetch(f.base+'/v1/queue')).json()).submissions[1].state,'done');
  const calls=await inputCalls(f);
  assert.equal(calls.length,2);assert.equal(calls[1].resume,true);assert.equal(calls[1].session,thread);
  assert.equal((await readFile(join(f.root,'cwd/spawns'),'utf8')).trim().split('\n').length,2);
});
const goalCall=(f,body,method='PUT')=>{const bytes=JSON.stringify(body);return fetch(f.base+'/api/goal',{method,body:bytes,headers:{'content-type':'application/json','content-length':Buffer.byteLength(bytes)}});};
test('Claude native goals discover explicitly, ACK before sync, preserve FIFO and never interrupt',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_GOAL_ACK_DELAY:'300'});
  const initial=await f.snapshot();assert.deepEqual(initial.harness.capabilities.goal,{pause:false,budget:false});
  for(const input of [{objective:'work',tokenBudget:100},{status:'paused'},{objective:'clear'},{objective:'work',roundBudget:null}])assert.equal((await goalCall(f,input)).status,400);
  const objective='Finish the exact native condition';
  assert.equal((await goalCall(f,{objective,operationId:'goal-one'})).status,200);
  assert.equal((await f.snapshot()).goal.syncStatus,'desynced','local write alone is not native goal ACK');
  assert.equal((await goalCall(f,{clear:true},'DELETE')).status,400,'busy clear must not interrupt');
  const follow=await f.post({prompt:'ordinary follow-up',mode:'queue'});assert.equal(follow.status,200,JSON.stringify({body:await follow.text(),state:await f.snapshot(),logs:f.logs()}));
  let state=await until(f.snapshot,s=>s.goal?.syncStatus==='synced');assert.equal(state.goal.objective,objective);assert.equal(state.goal.status,'active');
  state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===2);
  assert.equal(state.goalDelivery.status,'sent');
  assert.deepEqual((await inputCalls(f)).map(c=>c.text),['/goal '+objective,'ordinary follow-up']);
  assert.equal((await goalCall(f,{objective,operationId:'goal-one'})).status,200,'same operation does not replay');
  assert.equal((await goalCall(f,{clear:true,operationId:'goal-clear'},'DELETE')).status,200);
  state=await until(f.snapshot,s=>s.goal===null&&s.thread.status==='idle'&&s.thread.turns.length===3);
  assert.equal(state.goalDelivery.status,'sent');assert.equal((await inputCalls(f)).length,3);
  const controls=(await readFile(join(f.root,'calls.controls'),'utf8')).trim().split('\n').map(JSON.parse);assert.equal(controls.some(c=>c.request.subtype==='interrupt'),false);
});
test('Claude goal missing ACK is unknown, next prompt runs once, restart never reapplies goal',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_GOAL_NO_ACK:'1'});
  await goalCall(f,{objective:'unacknowledged objective',operationId:'unknown-goal'});
  let state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.goalDelivery?.status==='failed');assert.equal(state.goal.syncStatus,'desynced');assert.equal(state.goal.nativeObjective,null);assert.match(state.goalDelivery.error,/unknown/);
  assert.equal((await goalCall(f,{objective:'another objective'})).status,400,'unknown command cannot be silently retried or overlapped');
  assert.equal((await f.post({prompt:'next prompt',mode:'queue'})).status,200);
  state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===2);assert.equal(state.runtime.state,'idle');
  const thread=state.thread.id;await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.goalDelivery.status,'failed');assert.deepEqual((await inputCalls(f)).map(c=>c.text),['/goal unacknowledged objective','next prompt']);
});
for(const hook of [false,true])test('Claude exact native goal completion and saved-goal restart provenance remain separate '+(hook?'hook':'attachment'),async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_GOAL_COMPLETION:'1',...(hook?{CLAUDE_FIXTURE_GOAL_HOOK:'1'}:{})});
  await goalCall(f,{objective:'exact objective'});
  let state=await until(f.snapshot,s=>s.goal?.syncStatus==='synced');assert.equal(state.goal.status,'active','unrelated goal completion cannot finish current goal');
  state=await until(f.snapshot,s=>s.goal?.status==='done'&&s.thread.status==='idle');
  const id=state.goal.id,thread=state.thread.id;await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.goal.id,id);assert.equal(state.goal.syncStatus,'desynced');assert.match(state.goal.syncError,/unconfirmed/);assert.equal((await inputCalls(f)).length,1);
  const noGoal=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_NO_GOAL:'1'});assert.equal((await noGoal.snapshot()).harness.capabilities.goal,null);assert.equal((await goalCall(noGoal,{objective:'not supported'})).status,400);
});
test('Claude real thirty-second goal ACK deadline ignores late ACK and preserves following prompt exactly once',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_GOAL_ACK_DELAY:'31000'});
  await goalCall(f,{objective:'slow goal'});await f.post({prompt:'after slow goal',mode:'queue'});
  let state=await until(f.snapshot,s=>s.goalDelivery?.status==='failed',35000);
  assert.match(state.goalDelivery.error,/unknown/);assert.equal(state.goal.nativeObjective,null);assert.equal(state.queue.length,1);assert.equal(state.runtime.state,'working');
  state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===2);
  assert.equal(state.goalDelivery.status,'failed');assert.equal(state.goal.syncStatus,'desynced','expired ACK cannot confirm selection');
  assert.deepEqual((await inputCalls(f)).map(c=>c.text),['/goal slow goal','after slow goal']);assert.equal(state.runtime.state,'idle');
});
test('Claude restart during native goal command interrupts its receipt and never replays input',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_GOAL_ACK_DELAY:'3000'});
  await goalCall(f,{objective:'pending across restart'});
  const before=await until(f.snapshot,s=>s.thread.status==='running'&&s.goalDelivery?.status==='pending');
  await stop(f.child);const resumed=await f.launch();const after=await resumed.snapshot();
  assert.equal(after.thread.id,before.thread.id);assert.equal(after.runtime.state,'idle');assert.equal(after.goal.syncStatus,'desynced');assert.equal(after.goalDelivery.status,'failed');
  assert.equal((await (await fetch(resumed.base+'/v1/queue')).json()).submissions[0].state,'interrupted');
  assert.equal((await inputCalls(f)).length,1,'neither goal nor original input is replayed');
});
test('Claude goal uses existing shared accessible editor without unsupported budget or pause controls',{skip:!process.env.CLAUDE_GOALS_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('.','package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_GOALS:'1',CLAUDE_FIXTURE_MODELS:'1',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'goals-fixture'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  for(const width of [1280,390]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'goals-fixture'}});await page.goto(f.base+'/?lang=en');
    await page.getByRole('button',{name:'Composer actions',exact:true}).click();await page.getByRole('menuitem',{name:'Set goal',exact:true}).click();
    await expect(page.locator('#goal-budget')).toHaveCount(0);await expect(page.locator('#goal-round-budget')).toHaveCount(0);
    await page.locator('#goal-objective').fill('Browser goal '+width);await page.getByRole('button',{name:'Save goal',exact:true}).click();
    await until(f.snapshot,s=>s.goal?.syncStatus==='synced'&&s.thread.status==='idle');
    await page.getByRole('toolbar',{name:'Run status'}).getByRole('button',{name:/^Goal/}).click();
    await expect(page.locator('.goal-text')).toHaveText('Browser goal '+width);await expect(page.getByRole('button',{name:'Pause goal',exact:true})).toHaveCount(0);
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    await page.getByRole('button',{name:'Clear goal',exact:true}).click();await page.getByRole('button',{name:'Confirm clear goal',exact:true}).click();
    await until(f.snapshot,s=>s.goal===null&&s.thread.status==='idle');await page.close();
  }
});
test('Claude native tasks outlive turn completion, confined output and record-only clear preserve FIFO and restart',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_TASKS:'1',CLAUDE_FIXTURE_STEER:'1',CLAUDE_FIXTURE_TASK_ASYNC:'task-complete-ready'},async({root,cwd})=>{
    await writeFile(join(cwd,'task.output'),'x'.repeat(70000)+'own output tail');
    await writeFile(join(root,'outside'),'outside never exposed');
    await symlink(join(root,'outside'),join(cwd,'link.output'));await link(join(root,'outside'),join(cwd,'hard.output'));
    return {};
  });
  assert.equal((await f.snapshot()).harness.capabilities.backgroundTasks,true);
  await f.post({prompt:'start native tasks'});
  let state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1&&s.backgroundTasks.length===5);
  assert.equal(state.backgroundTasks.find(t=>t.id==='running-shell').status,'running','result never completes a task');
  assert.equal(state.backgroundTasks.find(t=>t.id==='running-shell').description,'Updated native shell');
  const output=await fetch(f.base+'/api/tasks/running-shell/output?tail=999999');assert.equal(output.status,200);
  const body=await output.json();assert.equal(body.content.length,65536);assert.equal(body.truncated,true);assert.ok(body.content.endsWith('own output tail'));
  for(const id of ['outside','symlink','hardlink']){const r=await fetch(f.base+`/api/tasks/${id}/output`);assert.equal(r.status,400);assert.equal(JSON.stringify(await r.json()).includes('outside never exposed'),false);}
  assert.equal((await fetch(f.base+'/api/tasks/not-registered/output')).status,404);
  assert.equal((await fetch(f.base+'/api/tasks/output')).status,404);
  assert.equal((await fetch(f.base+'/api/tasks/running-shell/output?tail=0')).status,400);
  assert.equal((await fetch(f.base+'/api/tasks/running-shell/output',{headers:{origin:'https://foreign.invalid'}})).status,403);
  const clear=()=>fetch(f.base+'/api/tasks/clear-finished',{method:'POST',body:'{}'});
  let cleared=await (await clear()).json();assert.equal(cleared.removed,1);assert.equal(cleared.backgroundTasks.length,4);
  assert.equal((await readFile(join(f.root,'cwd/task.output'),'utf8')).length,70015,'clearing records never touches output');
  await writeFile(join(f.root,'cwd/task-complete-ready'),'allow own native task notification');
  state=await until(f.snapshot,s=>s.backgroundTasks.find(t=>t.id==='running-shell')?.status==='completed');
  assert.equal(state.thread.status,'idle');assert.equal(state.thread.turns.length,1);assert.equal((await inputCalls(f)).length,1,'idle task event never manufactures input or a turn');
  for(let i=0;i<2;i++)assert.equal((await f.post({prompt:'finish task',mode:'queue'})).status,200);
  state=await until(f.snapshot,s=>s.thread.turns.length===3&&s.thread.status==='idle');
  assert.equal(state.backgroundTasks.find(t=>t.id==='running-shell').status,'completed');
  assert.deepEqual((await inputCalls(f)).map(c=>c.text),['start native tasks','finish task','finish task']);
  const thread=state.thread.id;await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.deepEqual(state.backgroundTasks.map(t=>[t.id,t.status]),[['running-shell','completed']]);
  assert.equal((await inputCalls(f)).length,3,'restart does not replay tasks or input');
});
test('Claude native tasks use shared accessible panel and bounded output on desktop and mobile', {skip:!process.env.CLAUDE_TASKS_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('.','package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_TASKS:'1',CLAUDE_FIXTURE_STEER:'1',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'tasks-fixture'},async({cwd,state})=>{await writeFile(join(cwd,'task.output'),'own browser task output');return {env:{TREER_AGENT_PRIVATE_STATE_DIR:state}};});
  await f.post({prompt:'start native tasks'});await until(f.snapshot,s=>s.backgroundTasks.length===5);
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  for(const width of [1280,390]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'tasks-fixture'}});await page.goto(f.base+'/?lang=en');
    if(width===390){await page.getByRole('button',{name:'Menu',exact:true}).click();await page.getByRole('menuitem',{name:/Tasks/}).click();}
    else await page.locator('.actions > button[aria-controls=tasks]').click();
    await expect(page.locator('#tasks')).toBeVisible();
    const task=page.locator('#tasks details').filter({hasText:'Updated native shell'});
    await task.locator('summary').focus();await page.keyboard.press('Enter');
    await expect(task.locator('.task-output')).toHaveText('own browser task output');
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    await page.getByRole('button',{name:'Close background tasks',exact:true}).click();await expect(page.locator('#tasks')).toBeHidden();await page.close();
  }
});
test('Claude validated images reach native input, steering and queued FIFO without embedding bytes in history',async t=>{
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_STEER:'1',CLAUDE_FIXTURE_IMAGES:'1',CLAUDE_FIXTURE_HOLD:'finish-ready',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  const gif=Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==','base64');
  // Valid GIF comments put this well over the ordinary 128KiB RPC frame cap.
  const image=Buffer.concat([gif.subarray(0,-1),Buffer.from([0x21,0xfe]),...Array(8192).fill(Buffer.concat([Buffer.from([255]),Buffer.alloc(255,65)])),Buffer.from([0,0x3b])]);
  const upload=async bytes=>fetch(f.base+'/api/attachments',{method:'POST',body:bytes});
  assert.equal((await upload(Buffer.from('not an image'))).status,400);
  const attachments=[];for(let i=0;i<3;i++){const r=await upload(image);assert.equal(r.status,201);attachments.push((await r.json()).attachment);}
  assert.equal((await f.snapshot()).harness.capabilities.images,true);
  assert.equal((await f.post({attachment_ids:['img_00000000000000000000000000000000'],prompt:'missing'})).status,400);
  assert.equal((await f.post({attachment_ids:[attachments[0].id,attachments[0].id],prompt:'duplicate'})).status,400);
  assert.equal((await f.post({attachment_ids:[attachments[0].id],prompt:''})).status,200);
  const working=await until(f.snapshot,s=>s.thread.activeTurnId);const turn=working.thread.activeTurnId;
  const steer=await f.post({attachment_ids:[attachments[1].id],prompt:'look at this',mode:'steer'});assert.equal(steer.status,200);assert.equal((await steer.json()).submission.disposition,'steered');
  await f.post({attachment_ids:[attachments[2].id],prompt:'',mode:'queue'});
  let state=await until(f.snapshot,s=>s.thread.turns[0].items.filter(i=>i.kind==='userMessage').length===2);
  assert.equal(state.thread.activeTurnId,turn);assert.equal(state.queue.length,1);
  await writeFile(join(f.root,'cwd/finish-ready'),'release own image turn');
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle');
  const expected={mime:'image/gif',bytes:image.length,sha256:createHash('sha256').update(image).digest('hex')};
  assert.deepEqual((await inputCalls(f)).map(c=>c.images),[[expected],[expected],[expected]]);
  const users=state.thread.turns.flatMap(t=>t.items.filter(i=>i.kind==='userMessage'));
  assert.deepEqual(users.flatMap(i=>i.attachments),attachments);
  assert.equal(JSON.stringify(state).includes(image.toString('base64')),false);
  await stop(f.child);const resumed=await f.launch({TREER_AGENT_PRIVATE_STATE_DIR:f.state});state=await resumed.snapshot();
  assert.deepEqual(state.thread.turns.flatMap(t=>t.items.filter(i=>i.kind==='userMessage')).flatMap(i=>i.attachments),attachments);
  const response=await fetch(resumed.base+'/api/attachments/'+attachments[0].id);assert.equal(response.status,200);assert.deepEqual(Buffer.from(await response.arrayBuffer()),image);
  assert.equal((await inputCalls(f)).length,3,'restart does not replay image inputs');
});
test('Claude shared UI uploads and sends native image-only input and image steer', {skip:!process.env.CLAUDE_IMAGES_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('.','package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_STEER:'1',CLAUDE_FIXTURE_IMAGES:'1',CLAUDE_FIXTURE_HOLD:'finish-ready',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'image-fixture'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  const image=Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==','base64');
  for(const width of [1280,390]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'image-fixture'}});
    await page.goto(f.base+'/?lang=en');
    const uploaded=page.waitForResponse(r=>r.url().endsWith('/api/attachments')&&r.request().method()==='POST');
    await page.locator('input[type=file]').setInputFiles({name:'own.gif',mimeType:'image/gif',buffer:image});assert.equal((await uploaded).status(),201);
    const response=page.waitForResponse(r=>r.url().endsWith('/api/prompt')&&r.request().method()==='POST');
    await page.locator('#send').click();assert.equal((await response).status(),200);
    await expect(page.getByRole('textbox',{name:/Message/})).toHaveValue('');
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);await page.close();
  }
  const calls=await inputCalls(f);assert.deepEqual(calls.map(c=>c.type),['accepted','steered']);assert.ok(calls.every(c=>c.images.length===1));
});
test('Claude shared UI sends steer to actual Rust entry on desktop and mobile', {skip:!process.env.CLAUDE_STEER_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('.','package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_STEER:'1',CLAUDE_FIXTURE_HOLD:'finish-ready',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'steer-fixture'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  await f.post({prompt:'initial UI turn'});await until(f.snapshot,s=>s.thread.activeTurnId);
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  for(const width of [1280,390]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'steer-fixture'}});
    await page.goto(f.base+'/?lang=en');
    const box=page.getByRole('textbox',{name:/Message/});await box.fill('UI steer '+width);
    const reply=page.waitForResponse(r=>r.url().endsWith('/api/prompt')&&r.request().method()==='POST');
    await page.getByRole('button',{name:'Steer current run',exact:true}).click();
    const response=await reply;assert.equal(response.status(),200);assert.equal((await response.json()).submission.disposition,'steered');
    await expect(box).toHaveValue('');
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    await page.close();
  }
  assert.deepEqual((await inputCalls(f)).map(c=>c.text),['initial UI turn','UI steer 1280','UI steer 390']);
});
test('Claude working steering retains same turn, repeated text occurrences and queued FIFO exactly once',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_STEER:'1',CLAUDE_FIXTURE_HOLD:'finish-ready',CLAUDE_FIXTURE_MODELS:'1'});
  const initial=await f.snapshot();assert.equal(initial.harness.capabilities.steer,true);
  await f.post({prompt:'initial turn'});
  const active=await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  const turn=active.thread.activeTurnId,session=active.thread.id;
  await f.post({prompt:'queued ordinary input',mode:'queue'});
  for(let i=0;i<2;i++){
    const response=await f.post({prompt:'same steer text',mode:'steer'});assert.equal(response.status,200);
    assert.equal((await response.json()).submission.disposition,'steered');
  }
  let state=await until(f.snapshot,s=>s.thread.turns[0].items.filter(i=>i.kind==='userMessage'&&i.text==='same steer text').length===2);
  assert.equal(state.thread.activeTurnId,turn);assert.equal(state.thread.id,session);assert.equal(state.queue.length,1);
  assert.deepEqual((await inputCalls(f)).map(c=>[c.type,c.text]),[['accepted','initial turn'],['steered','same steer text'],['steered','same steer text']]);
  await writeFile(join(f.root,'cwd/finish-ready'),'release own synthetic turn');
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle'&&s.queue.length===0);
  assert.ok(state.thread.turns.every(t=>t.status==='completed'));assert.equal(state.thread.id,session);
  assert.equal(state.thread.turns[0].id,turn);
  const items=state.thread.turns[0].items;
  assert.equal(items.filter(i=>i.kind==='userMessage').length,3);
  assert.ok(items.findIndex(i=>i.kind==='commandExecution')<items.findIndex(i=>i.text==='same steer text'));
  const calls=await inputCalls(f);assert.deepEqual(calls.map(c=>c.text),['initial turn','same steer text','same steer text','queued ordinary input']);
  assert.equal(new Set(calls.map(c=>c.uuid)).size,4);assert.ok(calls.every(c=>c.session===session));
  assert.ok(!(await modelCalls(f)).some(c=>c.request.subtype==='interrupt'));
  await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,session);assert.equal(state.thread.turns[0].items.filter(i=>i.kind==='userMessage').length,3);
  assert.equal((await inputCalls(f)).length,4,'restart never replays any input');
});
test('Claude idle steering is a normal prompt and concurrent ordinary inputs are sent once',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_STEER:'1'});
  const response=await f.post({prompt:'idle steer',mode:'steer'});assert.equal(response.status,200);
  assert.equal((await response.json()).submission.disposition,'sent');
  const responses=await Promise.all(['next A','next B'].map(prompt=>f.post({prompt,mode:'queue'})));
  assert.ok(responses.every(r=>r.status===200));
  const state=await until(f.snapshot,s=>s.thread.turns.length===3&&s.thread.status==='idle');
  const calls=await inputCalls(f);assert.deepEqual(calls.map(c=>c.type),['accepted','accepted','accepted']);
  assert.deepEqual(calls.map(c=>c.text),['idle steer','next A','next B']);
  assert.ok(state.thread.turns.every(t=>t.status==='completed'));
});
const settings=(f,body)=>fetch(f.base+'/api/settings',{method:'POST',body:JSON.stringify(body)});
const modelCalls=async f=>(await readFile(join(f.root,'calls.controls'),'utf8')).trim().split('\n').map(JSON.parse);
test('Claude effort discovers catalog and live setter, validates combined settings before mutation and resets explicitly',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1'});
  let state=await f.snapshot();const thread=state.thread.id;
  assert.deepEqual(state.modelOptions[0].supportedReasoningEfforts.map(e=>e.reasoningEffort),['low','medium','high']);
  assert.equal(state.thread.effortSelectionStatus,'unknown');
  assert.equal(state.harness.capabilities.sessionTransfer,null);
  assert.equal((await settings(f,{model:'legacy',reasoningEffort:'high'})).status,400);
  assert.equal((await modelCalls(f)).length,2,'invalid mixed setting sends neither model nor effort');
  assert.equal((await settings(f,{model:'opus',reasoningEffort:'high'})).status,200);
  state=await f.snapshot();assert.equal(state.thread.model,'claude-opus-test');assert.equal(state.thread.reasoningEffort,'high');assert.equal(state.thread.effortSelectionStatus,'confirmed');
  assert.equal((await settings(f,{reasoningEffort:null})).status,200);
  assert.equal((await f.snapshot()).thread.reasoningEffort,null);assert.equal((await f.snapshot()).thread.effortSelectionStatus,'confirmed');
  assert.equal((await settings(f,{reasoningEffort:'ultra'})).status,400);
  await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.thread.reasoningEffort,null);assert.equal(state.thread.effortSelectionStatus,'unknown','a default effort is not re-sent, so the fresh provider is not claimed');
  assert.equal(state.thread.model,'claude-opus-test','saved model is re-applied and ACKed by the resumed provider');
});
test('Claude model and effort selection survive a provider crash and a launcher restart',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1',CLAUDE_FIXTURE_CRASH_ONCE:'crash-marker'});
  const thread=(await f.snapshot()).thread.id;
  assert.equal((await settings(f,{model:'opus',reasoningEffort:'high'})).status,200);
  const selected=s=>s.thread.model==='claude-opus-test'&&s.thread.modelSelectionStatus==='confirmed'&&s.thread.reasoningEffort==='high'&&s.thread.effortSelectionStatus==='confirmed';
  assert.ok(selected(await f.snapshot()));
  assert.equal((await f.post({prompt:'crashes the provider',mode:'queue'})).status,200);
  let s=await until(f.snapshot,s=>s.runtime.recovery.restarts>=1&&s.ready&&s.runtime.state==='idle',15000);
  assert.equal(s.thread.id,thread);assert.ok(selected(s),JSON.stringify(s.thread));
  assert.equal((await f.post({prompt:'after provider restart',mode:'queue'})).status,200);
  await until(f.snapshot,s=>s.thread.status==='idle'&&s.queue.length===0&&s.thread.turns.length===2);
  await stop(f.child);const resumed=await f.launch();s=await resumed.snapshot();
  assert.equal(s.thread.id,thread);assert.ok(selected(s),JSON.stringify(s.thread));
  assert.equal((await resumed.post({prompt:'after launcher restart',mode:'queue'})).status,200);
  await until(resumed.snapshot,s=>s.thread.status==='idle'&&s.queue.length===0&&s.thread.turns.length===3);
  const calls=(await inputCalls(f)).slice(1);
  assert.deepEqual(calls.map(c=>[c.text,c.resume,c.model,c.effort]),[['after provider restart',true,'claude-opus-test','high'],['after launcher restart',true,'claude-opus-test','high']]);
  const controls=await modelCalls(f),inits=controls.flatMap((c,i)=>c.request.subtype==='initialize'?[i]:[]);
  assert.equal(inits.length,3);
  for(const start of inits.slice(1)){
    const next=controls.slice(start+1).findIndex(c=>c.request.subtype==='initialize'),window=controls.slice(start+1,next<0?undefined:start+1+next);
    assert.deepEqual(window.filter(c=>c.request.subtype==='set_model').map(c=>c.request.model),['opus'],'each resumed process gets the saved model once');
    assert.deepEqual(window.filter(c=>Object.hasOwn(c.request.settings||{},'effortLevel')).map(c=>c.request.settings.effortLevel),['high']);
  }
});
for(const env of [{CLAUDE_FIXTURE_NO_EFFORT:'1'},{CLAUDE_FIXTURE_PROBE_DELAY:'5500'}])test('Claude effort unsupported or absent probe ACK hides capability without killing prompt transport '+JSON.stringify(env),async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1',...env});
  assert.ok((await f.snapshot()).modelOptions.every(m=>m.supportedReasoningEfforts.length===0));
  assert.equal((await settings(f,{reasoningEffort:'high'})).status,400);
  await f.post({prompt:'probe failure does not block prompt'});
  await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  await new Promise(r=>setTimeout(r,650));
  assert.ok((await f.snapshot()).modelOptions.every(m=>m.supportedReasoningEfforts.length===0),'late probe ACK cannot enable setter');
  assert.equal((await inputCalls(f)).length,1);assert.equal((await f.snapshot()).runtime.provider.transport,'open');
});
test('Claude effort slow ACK pumps completion, preserves pending display, serializes prompt and truthful partial rejection',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1',CLAUDE_FIXTURE_EFFORT_DELAY:'1200',CLAUDE_FIXTURE_MODEL_BURST:'1',CLAUDE_FIXTURE_WRONG_MODEL_ACK:'1',CLAUDE_FIXTURE_REFUSE_EFFORT:'medium'});
  await f.post({prompt:'active A'});
  const changing=settings(f,{reasoningEffort:'high'});
  await until(async()=>modelCalls(f),calls=>calls.some(c=>c.request.settings?.effortLevel==='high'));
  const prompt=f.post({prompt:'next B',mode:'queue'});
  assert.equal((await f.snapshot()).thread.reasoningEffort,null,'wrong or pending ACK cannot publish selection');
  assert.equal((await changing).status,200);assert.equal((await prompt).status,200);
  await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===2);
  assert.deepEqual((await inputCalls(f)).map(c=>[c.text,c.effort]),[['active A',null],['next B','high']]);
  assert.equal((await settings(f,{model:'opus',reasoningEffort:'medium'})).status,400);
  let state=await f.snapshot();assert.equal(state.thread.model,'claude-opus-test','model ACK remains truthful when subsequent effort refuses');assert.equal(state.thread.reasoningEffort,'high');
  await f.post({prompt:'after rejected effort'});await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===3);
  assert.deepEqual((await inputCalls(f)).map(c=>c.text),['active A','next B','after rejected effort']);
  assert.ok(!(await modelCalls(f)).some(c=>c.request.subtype==='interrupt'));
});
test('Claude effort real fifteen-second timeout is unknown; next prompt once, late ACK cannot overwrite newer effort',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1',CLAUDE_FIXTURE_TIMEOUT_EFFORT:'high'});
  const changing=settings(f,{reasoningEffort:'high'});
  await until(async()=>modelCalls(f),calls=>calls.some(c=>c.request.settings?.effortLevel==='high'));
  const prompt=f.post({prompt:'after unknown effort',mode:'queue'});
  assert.equal((await changing).status,400);assert.equal((await prompt).status,200);
  let state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  assert.equal(state.thread.reasoningEffort,null);assert.equal(state.thread.effortSelectionStatus,'unknown');assert.equal(state.thread.model,'claude-sonnet-test');
  assert.equal((await settings(f,{reasoningEffort:'low'})).status,200);
  await new Promise(r=>setTimeout(r,3200));state=await f.snapshot();
  assert.equal(state.thread.reasoningEffort,'low');assert.equal(state.thread.effortSelectionStatus,'confirmed');assert.equal(state.runtime.provider.transport,'open');
  assert.deepEqual((await inputCalls(f)).map(c=>[c.text,c.effort]),[['after unknown effort','high']]);
  assert.equal((await modelCalls(f)).filter(c=>Object.hasOwn(c.request.settings||{},'effortLevel')).length,2,'no implicit effort retry or per-prompt override');
});
test('Claude effort shared browser shows unknown/ACK/refusal with accessible desktop and mobile controls',{skip:!process.env.CLAUDE_EFFORT_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('.','package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_EFFORTS:'1',CLAUDE_FIXTURE_EFFORT_DELAY:'1200',CLAUDE_FIXTURE_REFUSE_EFFORT:'medium',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'effort-fixture'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  const pages=[];
  for(const [lang,theme,width] of [['en','light',1280],['zh','dark',390],['en','dark',390],['zh','light',1280]]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'effort-fixture'}});pages.push(page);
    await page.goto(f.base+'/?lang='+lang+'&theme='+theme);
    await page.getByRole('button',{name:lang==='zh'?'输入选项':'Composer actions',exact:true}).click();
    await page.getByRole('menuitem',{name:lang==='zh'?/^模型/:/^Model/}).click();
    await expect(page.locator('#effort-select option:checked')).toHaveText(lang==='zh'?'思考强度未知':'Effort unknown');
    await page.locator('#effort-select').focus();await expect(page.locator('#effort-select')).toBeFocused();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  }
  const page=pages[0],response=page.waitForResponse(r=>r.url().endsWith('/api/settings'));
  await page.locator('#effort-select').selectOption('high');
  await expect(page.locator('#effort-select')).toBeDisabled();await expect(page.locator('#effort-select')).toHaveValue('__unknown');
  assert.equal((await response).status(),200);
  for(const p of pages)await expect(p.locator('#effort-select')).toHaveValue('high');
  const refused=page.waitForResponse(r=>r.url().endsWith('/api/settings'));await page.locator('#effort-select').selectOption('medium');assert.equal((await refused).status(),400);
  await expect(page.locator('#effort-select')).toBeEnabled();await expect(page.locator('#effort-select')).toHaveValue('high');
  const cleared=page.waitForResponse(r=>r.url().endsWith('/api/settings'));await page.locator('#effort-select').selectOption('');assert.equal((await cleared).status(),200);
  await expect(page.locator('#effort-select option:checked')).toHaveText('Default');
  const shots=process.env.CLAUDE_EFFORT_SHOTS;if(shots){await mkdir(shots,{recursive:true});for(let i=0;i<pages.length;i++)await pages[i].screenshot({path:join(shots,'effort-'+i+'.png')});}
});
test('Claude model catalog aliases and acknowledged switching preserve session and reject mixed effort atomically',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1'});
  let state=await f.snapshot();const thread=state.thread.id;
  assert.equal(state.thread.model,'claude-sonnet-test');
  assert.deepEqual(state.modelOptions.map(m=>[m.id,m.model]),[['sonnet','claude-sonnet-test'],['opus','claude-opus-test'],['legacy','claude-legacy-test']]);
  assert.ok(state.modelOptions.every(m=>m.supportedReasoningEfforts.length===0));
  assert.equal((await settings(f,{model:'opus',reasoningEffort:'high'})).status,400);
  assert.equal((await modelCalls(f)).length,1,'unsupported effort cannot partially change model');
  assert.equal((await settings(f,{model:'opus'})).status,200);
  state=await f.snapshot();assert.equal(state.thread.model,'claude-opus-test');assert.equal(state.thread.id,thread);
  assert.equal((await settings(f,{model:'invalid'})).status,400);
  assert.equal((await f.snapshot()).thread.model,'claude-opus-test');
  assert.equal((await f.post({prompt:'after model rejection'})).status,200);
  state=await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
  assert.equal(state.thread.turns[0].status,'completed');assert.equal(state.thread.model,'claude-opus-test');
  await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.thread.model,'claude-opus-test','restart re-applies the saved selection');assert.equal(state.thread.modelSelectionStatus,'confirmed','only the resumed provider ACK confirms it');
  const controls=await modelCalls(f),second=controls.findLastIndex(c=>c.request.subtype==='initialize');
  assert.deepEqual(controls.slice(second).filter(c=>c.request.subtype==='set_model').map(c=>c.request.model),['opus']);
  await stop(resumed.child);
  const journalPath=join(f.state,'rust-spike.json'),journal=JSON.parse(await readFile(journalPath,'utf8'));
  assert.deepEqual(journal.selection,{model:'opus'});
  journal.selection={model:'invalid'};await writeFile(journalPath,JSON.stringify(journal));
  state=await (await f.launch()).snapshot();
  assert.equal(state.ready,true,'a refused saved model never blocks startup');assert.equal(state.thread.model,'claude-sonnet-test');
  assert.equal(JSON.parse(await readFile(journalPath,'utf8')).selection,undefined,'a refused saved model is dropped');
});
test('Claude missing model catalog learns only actual reported model without a version table',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_NO_MODELS:'1'});
  assert.deepEqual((await f.snapshot()).modelOptions,[]);assert.equal((await f.snapshot()).thread.model,null);
  await f.post({prompt:'discover actual model'});
  const state=await until(f.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  assert.equal(state.thread.model,'claude-sonnet-test');assert.deepEqual(state.modelOptions.map(m=>m.model),['claude-sonnet-test']);
});
test('Claude slow model ACK pumps active completion and serializes concurrent prompt exactly once',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_MODEL_DELAY:'1200',CLAUDE_FIXTURE_MODEL_BURST:'1',CLAUDE_FIXTURE_WRONG_MODEL_ACK:'1'});
  await f.post({prompt:'first existing turn'});
  await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  const changing=settings(f,{model:'opus'});
  await until(async()=>modelCalls(f),calls=>calls.some(c=>c.request.subtype==='set_model'));
  const queued=f.post({prompt:'during switch',mode:'queue'});
  await new Promise(r=>setTimeout(r,450));
  let state=await f.snapshot();assert.equal(state.thread.model,'claude-sonnet-test','wrong ID and pending ACK must not change selected model');
  assert.equal(state.thread.turns[0].status,'completed','completion must be pumped before ACK');
  assert.equal((await changing).status,200);assert.equal((await queued).status,200);
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle');
  assert.equal(state.thread.model,'claude-opus-test');
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.map(c=>c.text),['first existing turn','during switch']);
  assert.deepEqual(calls.map(c=>c.model),['claude-sonnet-test','claude-opus-test']);
  assert.ok(state.thread.turns.every(t=>t.status==='completed'));
  assert.equal((await modelCalls(f)).filter(c=>c.request.subtype==='initialize').length,1);
  assert.ok(!(await modelCalls(f)).some(c=>c.request.subtype==='interrupt'));
});
test('Claude model ACK is not undone by old active turn model output arriving later',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_OLD_MODEL_AFTER_ACK:'1',CLAUDE_FIXTURE_HOLD:'finish-ready'});
  await f.post({prompt:'old turn'});
  await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  assert.equal((await settings(f,{model:'opus'})).status,200);
  let state=await until(f.snapshot,s=>s.thread.turns[0].items.some(i=>i.text==='old turn tail'));
  assert.equal(state.thread.model,'claude-opus-test');assert.equal(state.thread.status,'running');
  await f.post({prompt:'new turn',mode:'queue'});await writeFile(join(f.root,'cwd/finish-ready'),'release own turn');
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle');
  assert.equal(state.thread.model,'claude-opus-test');
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.deepEqual(calls.map(c=>c.model),['claude-sonnet-test','claude-opus-test']);
});
test('Claude model response timeout remains unknown without killing provider or losing queued prompt; late ACK cannot overwrite newer setting',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_MODEL_DELAY:'18000',CLAUDE_FIXTURE_MODEL_DELAY_ONCE:'1',CLAUDE_FIXTURE_INPUT_GATE:'input-ready'});
  const changing=settings(f,{model:'opus'});
  await until(async()=>modelCalls(f),calls=>calls.some(c=>c.request.subtype==='set_model'));
  const queued=f.post({prompt:'after unknown switch',mode:'queue'});
  const response=await changing;assert.equal(response.status,400);assert.equal((await response.json()).error.code,'operation_refused');
  const unknown=await f.snapshot();assert.equal(unknown.thread.model,null);assert.equal(unknown.thread.modelSelectionStatus,'unknown');
  assert.equal((await queued).status,200);
  await writeFile(join(f.root,'cwd/input-ready'),'release own synthetic input');
  let state=await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
  assert.equal(state.thread.model,'claude-opus-test','fresh provider init can resolve unknown outcome');
  assert.equal((await settings(f,{model:'legacy'})).status,200);
  await new Promise(r=>setTimeout(r,3500));
  state=await f.snapshot();assert.equal(state.ready,true);assert.equal(state.thread.status,'idle');assert.equal(state.thread.model,'claude-legacy-test');assert.equal(state.runtime.provider.transport,'open');
  assert.equal(state.thread.turns[0].status,'completed');
  assert.equal((await modelCalls(f)).filter(c=>c.request.subtype==='set_model').length,2,'one explicit newer setting, never retry mutation');
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1);
  assert.equal(f.child.exitCode,null);
});
test('Claude busy model timeout cannot be confirmed by old input reports; next queued input may report its actual model',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_MODEL_DELAY:'18000',CLAUDE_FIXTURE_OLD_MODEL_AFTER_TIMEOUT:'1',CLAUDE_FIXTURE_HOLD:'finish-ready'});
  await f.post({prompt:'old running turn'});
  await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  const changing=settings(f,{model:'opus'});
  await until(async()=>modelCalls(f),calls=>calls.some(c=>c.request.subtype==='set_model'));
  const queued=f.post({prompt:'new input after ambiguity',mode:'queue'});
  assert.equal((await changing).status,400);assert.equal((await queued).status,200);
  let state=await until(f.snapshot,s=>s.thread.turns[0].items.some(i=>i.text==='old timeout tail'));
  assert.equal(state.thread.model,null);assert.equal(state.thread.modelSelectionStatus,'unknown');assert.equal(state.thread.status,'running');
  assert.equal(state.queue.length,1);
  await writeFile(join(f.root,'cwd/finish-ready'),'release own result');
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle');
  assert.equal(state.thread.model,'claude-opus-test');
  assert.deepEqual((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse).map(c=>c.text),['old running turn','new input after ambiguity']);
});
test('Claude model browser uses actual Rust assets and shows ACK selection or localized unknown', {skip:!process.env.CLAUDE_MODEL_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve('package.json'))('@playwright/test');
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const f=await fixture(t,{CLAUDE_FIXTURE_MODELS:'1',CLAUDE_FIXTURE_MODEL_DELAY:'1200',CLAUDE_FIXTURE_TIMEOUT_MODEL:'timeout-choice',TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'model-fixture'},async({state})=>({env:{TREER_AGENT_PRIVATE_STATE_DIR:state}}));
  const browser=await chromium.launch({env:hermeticEnv({HOME:process.env.HOME})});t.after(()=>browser.close());
  const shots=process.env.CLAUDE_MODEL_SHOTS; if(shots)await mkdir(shots,{recursive:true});
  const pages=[];
  for(const [lang,theme,width] of [['en','light',1280],['zh','dark',390],['en','dark',390],['zh','light',1280]]){
    const page=await browser.newPage({viewport:{width,height:850},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'model-fixture'}});pages.push({page,lang,theme,width});
    await page.goto(f.base+`/?lang=${lang}&theme=${theme}`);
    await page.locator('.model').click();await expect(page.locator('#model-select')).toHaveValue('claude-sonnet-test');
    await page.locator('#model-select').focus();await expect(page.locator('#model-select')).toBeFocused();
  }
  const first=pages[0].page;
  const ack=first.waitForResponse(r=>r.url().endsWith('/api/settings'));
  await first.locator('#model-select').selectOption('claude-opus-test');
  assert.equal((await f.snapshot()).thread.model,'claude-sonnet-test');
  await expect(first.locator('#model-select')).toBeDisabled();
  assert.equal((await ack).status(),200);
  for(const {page} of pages)await expect(page.locator('#model-select')).toHaveValue('claude-opus-test');
  assert.equal((await settings(f,{model:'timeout-choice'})).status,400);
  for(const {page,lang,theme,width} of pages){
    await expect(page.locator('#model-select option:checked')).toHaveText(lang==='zh'?'模型未知':'Model unknown');
    await expect(page.locator('.model')).toContainText(lang==='zh'?'模型未知':'Model unknown');
    await expect(page.locator('#model-select')).toBeEnabled();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
    if(shots)await page.screenshot({path:join(shots,`${lang}-${theme}-${width}-unknown.png`)});
  }
  assert.equal(f.child.exitCode,null);assert.equal((await f.snapshot()).runtime.provider.transport,'open');
});
test('same text in a final-only different assistant message retains both identities after restart',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_DISTINCT_FINAL:'1'});
  assert.equal((await f.post({prompt:'two distinct assistant messages'})).status,200);
  let state=await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle');
  const matching=state.thread.turns[0].items.filter(i=>i.text==='thinking first');
  assert.equal(matching.length,2);assert.equal(new Set(matching.map(i=>i.id)).size,2);
  assert.ok(matching[1].id.endsWith('-different:0'));
  await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.deepEqual(state.thread.turns[0].items.filter(i=>i.text==='thinking first'),matching);
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1);
});
test('actual Claude entry queues two turns across early idle and retains tools after result/restart',async t=>{
  const f=await fixture(t);let state=await f.snapshot();assert.equal(state.harness.id,'claude');assert.equal(state.harness.capabilities.steer,true);
  assert.equal((await f.post({prompt:'first'})).status,200);
  await until(f.snapshot,s=>s.thread.activeTurnId);
  assert.equal((await f.post({prompt:'second',mode:'queue'})).status,200);
  state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle'&&s.queue.length===0);
  for(const turn of state.thread.turns){
    assert.equal(turn.status,'completed');
    assert.equal(turn.items.filter(i=>i.kind==='userMessage').length,1);
    const tools=turn.items.filter(i=>i.kind==='commandExecution');assert.equal(tools.length,1);
    assert.equal(tools[0].detailText,'synthetic retained output');assert.equal(tools[0].status,'completed');
  }
  const thread=state.thread.id,ids=state.thread.turns.map(t=>t.id);
  await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.deepEqual(state.thread.turns.map(t=>t.id),ids);
  assert.equal(state.thread.turns[0].items.find(i=>i.kind==='commandExecution').detailText,'synthetic retained output');
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,2,'restart must not replay accepted users');
  assert.equal((await resumed.post({prompt:'third'})).status,200);
  await until(resumed.snapshot,s=>s.thread.turns.length===3&&s.thread.status==='idle');
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);assert.equal(calls[2].resume,true);assert.equal(calls[2].session,thread);
});
test('restart between tool result and turn result interrupts the receipt, keeps the tool and continues the queue',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created'});
  assert.equal((await f.post({prompt:'hold result'})).status,200);
  let state=await until(f.snapshot,s=>s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution'&&i.status==='completed')));
  const thread=state.thread.id;await stop(f.child);const resumed=await f.launch({CLAUDE_FIXTURE_HOLD:''});state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.runtime.state,'idle');
  assert.equal(state.thread.turns[0].status,'interrupted');
  assert.equal(state.thread.turns[0].items.find(i=>i.kind==='commandExecution').detailText,'synthetic retained output');
  const receipt=(await (await fetch(resumed.base+'/v1/queue')).json()).submissions[0];
  assert.equal(receipt.state,'interrupted');assert.match(receipt.error.message,/not replayed/);
  assert.equal((await resumed.post({prompt:'next input after restart',mode:'queue'})).status,200);
  await until(resumed.snapshot,s=>s.thread.status==='idle'&&s.thread.turns.length===2);
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.length,2,'the interrupted input is never replayed');assert.equal(calls[1].resume,true);assert.equal(calls[1].session,thread);
});
test('actual Claude interrupt is correlated and only result completes the interrupted turn',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created'});
  assert.equal((await f.post({prompt:'interrupt this turn'})).status,200);
  await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  assert.equal((await fetch(f.base+'/api/interrupt',{method:'POST',body:'{}'})).status,200);
  const state=await until(f.snapshot,s=>s.thread.status==='idle');
  assert.equal(state.thread.turns.at(-1).status,'interrupted');
  assert.equal(state.thread.turns.at(-1).items.find(i=>i.kind==='commandExecution').detailText,'synthetic retained output');
});
test('refused interrupt does not relabel a successful provider result',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created',CLAUDE_FIXTURE_REFUSE_INTERRUPT:'1'});
  assert.equal((await f.post({prompt:'finish normally despite refusal'})).status,200);
  await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  assert.equal((await fetch(f.base+'/api/interrupt',{method:'POST',body:'{}'})).status,400);
  const state=await until(f.snapshot,s=>s.thread.status==='idle');
  assert.equal(state.thread.turns.length,1);
  assert.equal(state.thread.turns[0].status,'completed');
  assert.equal(state.thread.turns[0].items.find(i=>i.kind==='commandExecution').detailText,'synthetic retained output');
});
