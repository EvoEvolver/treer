import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn,execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {once} from 'node:events';
import {mkdtemp,mkdir,readFile,realpath,rm,writeFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {access} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {createRequire} from 'node:module';
import {hermeticEnv,localFetch as fetch} from '../../agent/tests/hermetic.mjs';
import {fakePrivateState} from '../../agent/tests/fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
async function until(read,accept){const end=Date.now()+10000;while(Date.now()<end){try{const v=await read();if(accept(v))return v;}catch{}await new Promise(r=>setTimeout(r,20));}throw Error('Claude fixture deadline');}
async function fixture(t,extra={},setup=async()=>({})){
  const root=await realpath(await mkdtemp(join(tmpdir(),'rust231-'))),cwd=join(root,'cwd'),state=join(root,'private'),port=join(root,'port');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const initial=await setup({root,cwd,state});
  const children=[];let logs='';
  const launch=async(overrides={},args=[])=>{
    await rm(port,{force:true});
    const env=hermeticEnv({HOME:root,AGENT_UI_CWD:cwd,TREER_RUST_SPIKE_STATE_DIR:state,AGENT_UI_PUBLIC_DIR:resolve('../agent/public'),AGENT_UI_PORT_FILE:port,AGENT_UI_PORT:'0',AGENT_UI_HARNESS:'claude',CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-stream.mjs')]),CLAUDE_FIXTURE_LOG:join(root,'calls'),...extra,...overrides});
    env.RUST_SPIKE_REGISTER='0';
    const child=spawn(binary,args,{cwd,env,stdio:['ignore','pipe','pipe']});children.push(child);
    child.stderr.on('data',b=>logs+=b);child.stdout.on('data',b=>logs+=b);
    const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(port,'utf8'))}`,v=>!v.endsWith('NaN'));
    const snapshot=()=>fetch(base+'/api/state').then(r=>r.json());
    try{await until(snapshot,s=>s.ready);}catch(error){throw Error(error.message+' '+logs);}
    return {child,base,snapshot,post:body=>fetch(base+'/api/prompt',{method:'POST',body:JSON.stringify(body)})};
  };
  t.after(async()=>{for(const c of children)if(c.exitCode===null&&c.signalCode===null){const exit=once(c,'exit');c.kill('SIGTERM');await exit;}await rm(root,{recursive:true,force:true});});
  return {...await launch(initial.env,initial.args),launch,state,root,logs:()=>logs};
}
async function stop(child){const exit=once(child,'exit');child.kill('SIGTERM');assert.equal((await exit)[0],0);}
const inputCalls=async f=>(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
test('Claude shared UI sends steer to actual Rust entry on desktop and mobile', {skip:!process.env.CLAUDE_STEER_BROWSER},async t=>{
  const {chromium,expect}=createRequire(resolve(process.env.NODE_LAUNCHER_ROOT||'../agent','package.json'))('@playwright/test');
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
  assert.equal(state.thread.id,thread);assert.equal(state.thread.model,'claude-sonnet-test','restart reports new provider initialization rather than stale saved UI selection');
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
  const {chromium,expect}=createRequire(resolve('../agent/package.json'))('@playwright/test');
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
test('receiptless Soul Claude retains 21 terminal records through new turn, restart and exact Node rollback without replay',async t=>{
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const thread='synthetic-soul-existing-session';
  const tasks=Array.from({length:21},(_,i)=>({id:'old-task-'+i,kind:i%2?'agent':'shell',description:'completed work '+i,status:'completed',startedAt:'2026-10-21T01:00:00Z',endedAt:'2026-10-21T02:00:00Z',exitCode:0,summary:'retained result '+i,outputFile:'/synthetic/never-read/task-'+i}));
  const ui=JSON.stringify({threadId:thread})+'\n';
  const f=await fixture(t,{TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0'},async({root,cwd,state})=>{
    const source=join(cwd,'.treer/agents/ag_contract');await mkdir(source,{recursive:true,mode:0o700});
    await writeFile(join(source,'agent-ui.json'),ui,{mode:0o600});
    const seed=join(root,'supported-snapshot.json');
    await writeFile(seed,JSON.stringify({version:1,harness:'claude',agent:'ag_contract',cwd,threadId:thread,queueSha256:null,snapshot:{ready:true,harness:{id:'claude'},runtime:{state:'idle',error:null},thread:{id:thread,status:'idle',activeTurnId:null,turns:[]},queue:[],backgroundTasks:tasks,goal:null,goalDelivery:null}}),{mode:0o600});
    return {env:{TREER_AGENT_PRIVATE_STATE_DIR:state,AGENT_UI_SESSION_ID:thread,RUST_CLAUDE_IMPORT_SNAPSHOT:seed},args:['--import-claude-state']};
  });
  const check=state=>{assert.equal(state.thread.id,thread);assert.deepEqual(state.backgroundTasks,tasks);assert.equal(state.harness.capabilities.backgroundTasks,false);};
  check(await f.snapshot());await assert.rejects(access(join(f.root,'calls')),'import must not submit terminal tasks');
  const source=join(f.root,'cwd/.treer/agents/ag_contract');
  await assert.rejects(access(join(source,'queue.json')));
  assert.equal(await readFile(join(f.state,'node-backup-v1/agent-ui.json'),'utf8'),ui);
  assert.equal((await f.post({prompt:'one fresh request'})).status,200);
  check(await until(f.snapshot,s=>s.thread.turns.length===1&&s.thread.status==='idle'));
  await stop(f.child);
  const env={TREER_AGENT_PRIVATE_STATE_DIR:f.state,AGENT_UI_SESSION_ID:thread};
  const resumed=await f.launch(env);check(await resumed.snapshot());
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.length,1);assert.equal(calls[0].resume,true);assert.equal(calls[0].session,thread);
  assert.deepEqual(JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8')).claudeTerminalTasks,tasks);
  await stop(resumed.child);
  await promisify(execFile)(binary,['--restore-node-backup'],{cwd:join(f.root,'cwd'),env:hermeticEnv({HOME:f.root,AGENT_UI_CWD:join(f.root,'cwd'),TREER_AGENT_ID:'ag_contract',TREER_AGENT_PRIVATE_STATE_DIR:f.state,TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_HARNESS:'claude'}),timeout:5000});
  assert.equal(await readFile(join(source,'agent-ui.json'),'utf8'),ui);
  await assert.rejects(access(join(source,'queue.json')));
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1,'backup restoration must not start provider');
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
test('restart between tool result and turn result preserves uncertain receipt and tool without replay',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created'});
  assert.equal((await f.post({prompt:'hold result'})).status,200);
  let state=await until(f.snapshot,s=>s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution'&&i.status==='completed')));
  const thread=state.thread.id;await stop(f.child);const resumed=await f.launch();state=await resumed.snapshot();
  assert.equal(state.thread.id,thread);assert.equal(state.runtime.state,'blocked');
  assert.equal(state.thread.turns[0].items.find(i=>i.kind==='commandExecution').detailText,'synthetic retained output');
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1);
});
test('Claude cached terminal display cannot clear an uncertain acknowledged receipt on restart',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created'});
  assert.equal((await f.post({prompt:'uncertain provider result'})).status,200);
  await until(f.snapshot,s=>s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution'&&i.status==='completed')));
  await stop(f.child);
  const path=join(f.state,'rust-spike.json'),saved=JSON.parse(await readFile(path,'utf8'));
  assert.equal(saved.records[0].submission.state,'running');
  // Deliberately make the display checkpoint claim completion while the
  // independent durable dispatch receipt remains unresolved. It is not a
  // fresh provider response and must never authorize the queued next input.
  saved.claudeTurns[0].status='completed';
  await writeFile(path,JSON.stringify(saved),{mode:0o600});
  const resumed=await f.launch(),state=await resumed.snapshot();
  assert.equal(state.runtime.state,'blocked');
  assert.match(state.thread.lastError,/uncertain/);
  const retained=JSON.parse(await readFile(path,'utf8'));
  assert.equal(retained.records[0].submission.state,'failed');
  assert.match(retained.records[0].submission.error,/uncertain/);
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1);
});
test('Claude explicit unknown disposition uses current idle adapter, never cached terminal authority or input replay',async t=>{
  const f=await fixture(t,{CLAUDE_FIXTURE_HOLD:'never-created'});
  await f.post({prompt:'original ambiguous input'});
  let state=await until(f.snapshot,s=>s.thread.activeTurnId&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution'&&i.status==='completed')));
  let saved=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));
  const id=saved.records[0].submission.id,thread=state.thread.id;
  const action=(base,receipt)=>fetch(base+'/api/recovery',{method:'POST',body:JSON.stringify({action:'acknowledge_unknown',submission_id:id,thread_id:thread,expected_turn_id:receipt.turn_id,expected_error:receipt.error,accept_unknown_outcome:true})});
  assert.equal((await action(f.base,saved.records[0].submission)).status,400,'live active adapter must refuse');
  await stop(f.child);
  const resumed=await f.launch({CLAUDE_FIXTURE_HOLD:''});
  state=await resumed.snapshot();assert.equal(state.runtime.state,'blocked');
  saved=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));
  const original=saved.records[0].submission;
  const queued=await resumed.post({prompt:'new input after disposition',mode:'queue'});assert.equal(queued.status,200);
  assert.equal((await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').length,1);
  assert.equal((await action(resumed.base,original)).status,202);
  state=await until(resumed.snapshot,s=>s.thread.status==='idle'&&s.queue.length===0&&s.thread.turns.length===2);
  const result=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));
  assert.equal(result.records[0].submission.state,'interrupted');
  assert.equal(result.records[0].submission.recovery_disposition.providerOutcome,'unknown');
  assert.equal(result.records[0].submission.recovery_disposition.originalError,original.error);
  assert.equal(result.records[0].submission.recovery_disposition.idleEvidence,'initialized_current_adapter_no_active_request');
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.length,2);assert.equal(calls[1].resume,true);assert.equal(calls[1].session,thread);
  await stop(resumed.child);const again=await f.launch({CLAUDE_FIXTURE_HOLD:''});
  const retained=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));
  assert.deepEqual(retained.records[0].submission,result.records[0].submission);
  assert.equal((await again.snapshot()).thread.id,thread);
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
test('actual Claude import consumes supported snapshot plus exact Node queue backup and same-session new turn',async t=>{
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  const queue=JSON.stringify({version:1,records:[]})+'\n',thread='synthetic-existing-claude';
  const old={id:'old-turn',status:'completed',startedAt:null,error:null,items:[{id:'old-tool',kind:'commandExecution',text:'old synthetic command',detailText:'old retained output',status:'completed',sourceTurnId:'old-turn'}]};
  const f=await fixture(t,{TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,AGENT_UI_MESSAGE_WATCH:'0'},async({root,cwd,state})=>{
    const source=join(cwd,'.treer/agents/ag_contract');await mkdir(source,{recursive:true,mode:0o700});
    await writeFile(join(source,'agent-ui.json'),JSON.stringify({threadId:thread})+'\n',{mode:0o600});await writeFile(join(source,'queue.json'),queue,{mode:0o600});
    const seed=join(root,'supported-snapshot.json');
    await writeFile(seed,JSON.stringify({version:1,harness:'claude',agent:'ag_contract',cwd,threadId:thread,queueSha256:createHash('sha256').update(queue).digest('hex'),snapshot:{ready:true,harness:{id:'claude'},runtime:{state:'idle',error:null},thread:{id:thread,status:'idle',activeTurnId:null,turns:[old],hasOlderItems:true},queue:[],backgroundTasks:[],goal:null,goalDelivery:null}}),{mode:0o600});
    return {env:{TREER_AGENT_PRIVATE_STATE_DIR:state,AGENT_UI_SESSION_ID:thread,RUST_CLAUDE_IMPORT_SNAPSHOT:seed},args:['--import-claude-state']};
  });
  let state=await f.snapshot();assert.equal(state.harness.id,'claude');assert.equal(state.thread.id,thread);assert.equal(state.thread.historyUnavailable,true);
  assert.deepEqual(state.thread.turns[0].items,[old.items[0]]);
  assert.equal(await readFile(join(f.state,'node-backup-v1/queue.json'),'utf8'),queue);
  assert.equal((await f.post({prompt:'post import'})).status,200);state=await until(f.snapshot,s=>s.thread.turns.length===2&&s.thread.status==='idle');
  assert.equal(state.thread.turns[0].items[0].detailText,'old retained output');
  const calls=(await readFile(join(f.root,'calls'),'utf8')).trim().split('\n').map(JSON.parse);assert.equal(calls.length,1);assert.equal(calls[0].resume,true);assert.equal(calls[0].session,thread);
});
test('actual Claude import refuses mismatched or unsettled supported snapshots before provider/publication',async t=>{
  const controller=await fakePrivateState('ag_contract','fixture-only');t.after(()=>controller.close());
  for(const fault of ['session','queue-hash','active','duplicate-turn','oversize-window','active-task','duplicate-task','receiptless-hash','missing-queue-binding'])await t.test(fault,async t=>{
    const root=await realpath(await mkdtemp(join(tmpdir(),'rust231-refuse-')));
    t.after(()=>rm(root,{recursive:true,force:true}));
    const cwd=join(root,'cwd'),state=join(root,'private'),source=join(cwd,'.treer/agents/ag_contract');
    await mkdir(source,{recursive:true,mode:0o700});await mkdir(state,{mode:0o700});
    const thread='synthetic-existing-claude',queue=JSON.stringify({version:1,records:[]})+'\n';
    const ui=JSON.stringify({threadId:thread})+'\n';
    await writeFile(join(source,'agent-ui.json'),ui,{mode:0o600});await writeFile(join(source,'queue.json'),queue,{mode:0o600});
    const seed={version:1,harness:'claude',agent:'ag_contract',cwd,threadId:thread,queueSha256:createHash('sha256').update(queue).digest('hex'),snapshot:{ready:true,harness:{id:'claude'},runtime:{state:'idle',error:null},thread:{id:thread,status:'idle',activeTurnId:null,turns:[]},queue:[],backgroundTasks:[],goal:null,goalDelivery:null}};
    if(fault==='session')seed.snapshot.thread.id='another-session';
    if(fault==='queue-hash')seed.queueSha256='0'.repeat(64);
    if(fault==='active')seed.snapshot.thread.activeTurnId='unsettled';
    if(fault==='duplicate-turn')seed.snapshot.thread.turns=[{id:'same',status:'completed',items:[]},{id:'same',status:'completed',items:[]}];
    if(fault==='oversize-window')seed.snapshot.thread.turns=Array.from({length:51},(_,i)=>({id:'t'+i,status:'completed',items:[]}));
    const task={id:'task',kind:'shell',description:'retained',status:'completed',startedAt:null,endedAt:null,exitCode:0,summary:null};
    if(fault==='active-task')seed.snapshot.backgroundTasks=[{...task,status:'running'}];
    if(fault==='duplicate-task')seed.snapshot.backgroundTasks=[task,task];
    if(fault==='receiptless-hash')seed.queueSha256=null;
    if(fault==='missing-queue-binding')delete seed.queueSha256;
    const seedPath=join(root,'supported-snapshot.json');await writeFile(seedPath,JSON.stringify(seed),{mode:0o600});
    const log=join(root,'provider-spawn');
    const env=hermeticEnv({HOME:root,AGENT_UI_CWD:cwd,TREER_AGENT_PRIVATE_STATE_DIR:state,AGENT_UI_PORT:'0',RUST_SPIKE_REGISTER:'0',AGENT_UI_HARNESS:'claude',AGENT_UI_SESSION_ID:thread,TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-stream.mjs')]),CLAUDE_FIXTURE_SPAWN_LOG:log,RUST_CLAUDE_IMPORT_SNAPSHOT:seedPath});
    const child=spawn(binary,['--import-claude-state'],{cwd,env,stdio:['ignore','ignore','pipe']});
    let stderr='';child.stderr.on('data',b=>stderr+=b);
    const timer=setTimeout(()=>child.kill('SIGKILL'),5000);
    const [code]=await once(child,'exit');clearTimeout(timer);
    assert.equal(code,1);assert.match(stderr,/claude_state_import_failed/);
    assert.equal(await readFile(join(source,'queue.json'),'utf8'),queue);
    assert.equal(await readFile(join(source,'agent-ui.json'),'utf8'),ui);
    assert.equal(await readFile(join(state,'node-backup-v1/queue.json'),'utf8'),queue);
    await assert.rejects(access(join(state,'rust-spike.json')));
    await assert.rejects(access(log));
  });
});
