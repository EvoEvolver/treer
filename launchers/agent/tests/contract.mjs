import assert from 'node:assert/strict';
import test from 'node:test';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm, chmod, readdir, unlink, symlink, lstat, rename } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { connect } from 'node:net';
import { get as httpGet, request as httpRequest } from 'node:http';
import { hermeticEnv, localFetch as fetch } from './hermetic.mjs';
import { fakePrivateState } from './fake-private-state.mjs';

const launcherRoot = resolve('.');
const { WebSocket } = createRequire(join(launcherRoot, 'package.json'))('ws');
const binary = process.env.RUST_SPIKE_BIN;
const execFileAsync = promisify(execFile);
async function rssKiB(pid) {
  const {stdout}=await execFileAsync('/bin/ps',['-o','rss=','-p',String(pid)]);
  const rss=Number(stdout.trim());assert.ok(Number.isFinite(rss)&&rss>0);return rss;
}
async function heapSample(pid) {
  assert.equal(process.platform,'darwin','native heap profile requires macOS');
  // Only our fixture's exact launcher PID. No addresses/content, files, user
  // processes, provider config or MallocStackLogging changes are involved.
  const {stdout}=await execFileAsync('/usr/bin/heap',['-s','--noContent',String(pid)],{timeout:15000,maxBuffer:2*1024*1024});
  const totals=stdout.match(/All zones:\s+(\d+) nodes \((\d+) bytes\)/);
  assert.ok(totals,'native heap output must include aggregate allocation totals');
  return {nodes:Number(totals[1]),bytes:Number(totals[2])};
}
// Match Controller's NON_ALPHANUMERIC encoding, which also escapes '-' / '_'.
const controllerSegment = id => id.replace(/[^a-z0-9]/gi, c => '%'+c.charCodeAt(0).toString(16).toUpperCase());
assert.ok(binary, 'RUST_SPIKE_BIN required');

async function until(read, predicate, waitMs=12000) {
  const end = Date.now() + waitMs;
  while (Date.now() < end) { try {const value=await read();if(predicate(value))return value;}catch{} await new Promise(r=>setTimeout(r,20)); }
  throw Error('contract fixture deadline');
}
async function fixture(t, extra={}, controllerOptions={}) {
  const root=await realpath(await mkdtemp(join(tmpdir(),'rust158-')));
  const cwd=join(root,'cwd'), state=join(root,'private');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const controller=await fakePrivateState('ag_contract','fixture-only',controllerOptions);
  const portFile=join(root,'port');
  const env=hermeticEnv({ HOME:root, CODEX_HOME:join(root,'codex'), AGENT_UI_CWD:cwd, AGENT_UI_PUBLIC_DIR:join(launcherRoot,'public'), AGENT_UI_PORT_FILE:portFile, AGENT_UI_PORT:'0', TREER_AGENT_ID:'ag_contract', TREER_WORKLOAD_CREDENTIAL:'fixture-only', TREER_AGENT_SERVER_URL:controller.url, TREER_AGENT_PRIVATE_STATE_DIR:state, AGENT_UI_INSTANCE_ID:'contract-instance', AGENT_UI_MESSAGE_WATCH:'0', CODEX_BIN:join(launcherRoot,'tests/fake-codex.mjs'), FAKE_CODEX_RELAY_RECOVERY:'1', ...extra });
  env.RUST_SPIKE_CLI_LOG=join(root,'registration.json');
  // A managed Rust author may inherit registration=1; only the explicit
  // registration fixture may opt in, never the rest of this sealed suite.
  env.RUST_SPIKE_REGISTER=extra.RUST_SPIKE_REGISTER??'0';
  env.RUST_SPIKE_PROVIDER_LOG=join(root,'provider.jsonl');
  env.FAKE_CODEX_CALLS=env.RUST_SPIKE_PROVIDER_LOG;
  const children=[];let logs='';
  const launch=async (overrides,waitReady=true,rustArgs=[])=>{
    await rm(portFile,{force:true});
    const child=spawn(binary,rustArgs,{cwd,env:{...env,...overrides},stdio:['ignore','pipe','pipe']});
    children.push(child);child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
    const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(portFile,'utf8'))}`,x=>!x.endsWith('NaN'));
    if(waitReady)try {await until(()=>fetch(base+'/api/state').then(r=>r.json()),x=>x.ready===true);}catch(e){throw Error(e.message+' '+logs);}
    return {child,base};
  };
  t.after(async()=>{for(const child of children){if(child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await once(child,'exit');}}await controller.close();await rm(root,{recursive:true,force:true});});
  const {child,base}=await launch({});
  const headers={'x-treer-interface-source':'controller-api','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'contract-instance',host:'agent-interface'};
  const post=body=>fetch(base+'/v1/prompts',{method:'POST',headers,body:JSON.stringify(body)});
  return {base,post,headers,child,root,state,launch,controller,env,logs:()=>logs,trackChild:child=>children.push(child)};
}

for(const scenario of ['completed','failed','interrupted','inProgress','legacy','legacyInterrupted','missing','older'])test('restart recovery reconciles '+scenario+' without replaying accepted input',{timeout:20000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1'});
  const first=await s.post({text:'first input must never replay',operation_id:'recovery-first'}).then(r=>r.json());
  const receipt=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  await until(()=>receipt(first.submission.id),r=>r.submission?.state==='running'&&r.submission.turn_id==='accepted-0');
  const second=await s.post({text:'second input must retain FIFO',operation_id:'recovery-second'}).then(r=>r.json());
  assert.equal((await receipt(second.submission.id)).submission.state,'queued');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
  const journalPath=join(s.state,'rust-spike.json'),journal=JSON.parse(await readFile(journalPath,'utf8'));
  if(['legacy','legacyInterrupted'].includes(scenario)) {
    journal.records[0].submission.state='failed';
    journal.records[0].submission.error='Experiment restarted; dispatch outcome uncertain; inspect transcript before retry';
    await writeFile(journalPath,JSON.stringify(journal),{mode:0o600});
  }
  const restarted=await s.launch({RECOVERY_HOLD:'0',RECOVERY_STATUS:scenario==='legacyInterrupted'?'interrupted':['legacy','missing','older'].includes(scenario)?'completed':scenario,RECOVERY_MISSING:scenario==='missing'?'1':'0',RECOVERY_OLD:scenario==='older'?'1':'0'});
  const get=id=>fetch(restarted.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  if(scenario==='missing') {
    // History has no record of the accepted turn: it fails and the queue continues.
    await until(()=>get(second.submission.id),r=>r.submission?.state==='done');
    const first_=(await get(first.submission.id)).submission;
    assert.equal(first_.state,'failed');assert.match(first_.error,/not replayed/);
  } else {
    await until(()=>get(second.submission.id),r=>r.submission?.state==='done');
    assert.equal((await get(first.submission.id)).submission.state,['completed','legacy','inProgress','older'].includes(scenario)?'done':scenario==='legacyInterrupted'?'interrupted':scenario);
    const state=await fetch(restarted.base+'/api/state').then(r=>r.json());
    assert.equal(state.thread.id,'recovery-thread');assert.equal(state.runtime.state,'idle');
    assert.equal(state.queue.length,0);assert.equal(state.thread.lastError,null);
    const duplicate=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'first input must never replay',operation_id:'recovery-first'})}).then(r=>r.json());
    assert.equal(duplicate.duplicate,true);assert.equal(duplicate.submission.id,first.submission.id);
  }
  const calls=(await readFile(join(s.root,'provider.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  const inputs=calls.filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text);
  assert.deepEqual(inputs,['first input must never replay','second input must retain FIFO']);
  if(scenario==='older')assert.ok(calls.some(c=>c.method==='thread/read'),'older turn is actually fetched via provider protocol');
});

test('restart with history that lacks an accepted turn fails it and continues the queue without replay',{timeout:25000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1'});
  const first=await s.post({text:'never resend exact accepted input',operation_id:'live-exact-first'}).then(r=>r.json());
  const second=await s.post({text:'FIFO second',operation_id:'live-exact-second'}).then(r=>r.json());
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({RECOVERY_HOLD:'0',RECOVERY_LATE:'1',RECOVERY_OLD:'1'});
  const get=id=>fetch(restarted.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  await until(()=>get(second.submission.id),r=>r.submission?.state==='done',18000);
  assert.equal((await get(first.submission.id)).submission.state,'failed');
  assert.notEqual((await fetch(restarted.base+'/api/state').then(r=>r.json())).runtime.state,'blocked');
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['never resend exact accepted input','FIFO second']);
});

test('restart with an idle provider settles the stale accepted turn as interrupted and continues',{timeout:16000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1'});
  const first=await s.post({text:'one original input',operation_id:'projected-resume-first'}).then(r=>r.json());
  const second=await s.post({text:'runs after restart',operation_id:'projected-resume-second'}).then(r=>r.json());
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({RECOVERY_IDLE_RESUME:'1',RECOVERY_HOLD:'0'});
  const get=id=>fetch(restarted.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  const settled=(await get(first.submission.id)).submission;
  assert.equal(settled.state,'interrupted',JSON.stringify(settled)+s.logs()+JSON.stringify((await fetch(restarted.base+'/api/state').then(r=>r.json())).thread.turns));
  await until(()=>get(second.submission.id),x=>x.submission.state==='done');
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['one original input','runs after restart']);
});

test('stale background observation deadline cannot kill a healthy successor turn and late reply cannot settle it',{timeout:90000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1',RECOVERY_OBSERVER_STALL:'1'});
  const first=await s.post({text:'complete A while its read stalls',operation_id:'stalled-observer-A'}).then(r=>r.json());
  const second=await s.post({text:'B remains active past stale A deadline',operation_id:'stalled-observer-B'}).then(r=>r.json());
  const get=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  await until(()=>get(first.submission.id),x=>x.submission.state==='done');
  await until(()=>get(second.submission.id),x=>x.submission.state==='running');
  await new Promise(resolve=>setTimeout(resolve,63500));
  const active=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(active.runtime.provider.transport,'open',JSON.stringify(active.runtime));
  assert.equal(active.runtime.recovery.restarts,0,JSON.stringify(active.runtime));
  assert.equal(active.runtime.provider.observationTimeouts,1);
  assert.equal((await get(first.submission.id)).submission.state,'done');
  assert.equal((await get(second.submission.id)).submission.state,'running');
  assert.equal((await readFile(join(s.root,'cwd/recovery-provider-pids.jsonl'),'utf8')).trim().split('\n').length,1,'original healthy provider must not be killed/replaced');
  await until(()=>get(second.submission.id),x=>x.submission.state==='done');
  const third=await s.post({text:'C after late reply',operation_id:'stalled-observer-C'}).then(r=>r.json());
  await until(()=>get(third.submission.id),x=>x.submission.state==='done');
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['complete A while its read stalls','B remains active past stale A deadline','C after late reply']);
});

test('history idle summary cannot interrupt an executing turn or dispatch its successor before explicit completion',{timeout:22000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1',RECOVERY_IDLE_SUMMARY:'1'});
  const first=await s.post({text:'still executing input',operation_id:'idle-summary-first'}).then(r=>r.json());
  const second=await s.post({text:'must wait for actual completion',operation_id:'idle-summary-second'}).then(r=>r.json());
  const get=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.recovery.readCount>=1);
  assert.equal((await get(first.submission.id)).submission.state,'running','a fresh idle summary must not settle the receipt');
  assert.equal((await get(second.submission.id)).submission.state,'queued');
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
  await until(()=>get(first.submission.id),x=>x.submission.state==='done');
  await until(()=>get(second.submission.id),x=>x.submission.state==='done');
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.runtime.state,'idle');assert.equal(state.runtime.provider.transport,'open');
  assert.equal(state.runtime.recovery.restarts,0);
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['still executing input','must wait for actual completion']);
});

for(const old of [false,true,'resume-race'])test('provider crash restarts it, resumes the same thread and continues FIFO without replay '+old,{timeout:25000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1',RECOVERY_DISCONNECT:'1',RECOVERY_OLD_DISCONNECT:old===true?'1':'0',RECOVERY_RESUME_RACE:old==='resume-race'?'1':'0'});
  const first=await s.post({text:'one accepted input',operation_id:'reconnect-first'}).then(r=>r.json());
  const second=await s.post({text:'second queued',operation_id:'reconnect-second'}).then(r=>r.json());
  const get=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  try { await until(()=>get(first.submission.id),r=>['interrupted','done'].includes(r.submission?.state),18000); }
  catch(error) { throw Error(error.message+' '+JSON.stringify(await fetch(s.base+'/api/state').then(r=>r.json()))+' '+s.logs()+' '+JSON.stringify(await providerCalls(s))); }
  await until(()=>get(second.submission.id),r=>r.submission?.state==='running');
  const snapshot=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.recovery.restarts===1&&x.ready,15000);
  assert.equal(snapshot.thread.id,'recovery-thread');assert.equal(snapshot.runtime.recovery.state,'running');
  if(old===true) { assert.ok(snapshot.history.totalTurns>=66);assert.equal(snapshot.thread.hasOlderItems,true);assert.ok(snapshot.thread.turns.length<=50); }
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['one accepted input','second queued']);
  assert.equal((await fetch(s.base+'/v1/abort',{method:'POST',headers:s.headers,body:'{}'})).status,202);
});

test('provider restarts keep retrying with backoff when resume is refused, with no attempt cap',{timeout:30000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1',RECOVERY_DISCONNECT:'1',RECOVERY_REFUSE_RESUME:'1'});
  const first=await s.post({text:'accepted once',operation_id:'exhaust-first'}).then(r=>r.json());
  const second=await s.post({text:'FIFO preserved',operation_id:'exhaust-second'}).then(r=>r.json());
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.recovery.restarts>=4,26000);
  assert.equal(state.runtime.recovery.state,'restarting');assert.ok(state.runtime.recovery.consecutiveFailures>=4);
  assert.match(state.runtime.recovery.lastFailure,/restart failed/);
  // The interruption may be observed before the provider exits.
  assert.ok(['running','interrupted'].includes((await fetch(s.base+'/v1/submissions/'+first.submission.id,{headers:s.headers}).then(r=>r.json())).submission.state));
  const late=await s.post({text:'accepted while restarting',operation_id:'exhaust-third'});assert.equal(late.status,202);
  assert.ok((await providerCalls(s)).filter(c=>c.method==='turn/start').length<=2,'no turn is dispatched to a provider that cannot resume');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
});

test('provider restart with a slow resume leaves state reads and shutdown responsive',{timeout:15000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1',RECOVERY_DISCONNECT:'1',RECOVERY_STALL_RESUME:'1'});
  await s.post({text:'first only',operation_id:'slow-reconnect'});
  await until(()=>providerCalls(s),x=>x.filter(c=>c.method==='thread/resume').length>=1);
  const pids=(await readFile(join(s.root,'cwd/recovery-provider-pids.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(pids.length,2);
  const start=Date.now();assert.equal((await fetch(s.base+'/api/state')).status,200);assert.ok(Date.now()-start<1000);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;assert.ok(Date.now()-start<5000);
  await until(()=>Promise.resolve(pids.every(({pid})=>{try{process.kill(pid,0);return false;}catch(error){return error.code==='ESRCH';}})),Boolean,3000);
});

test('unusable private storage exits the launcher non-zero so the Host restarts it, without dispatching',{timeout:20000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_HOLD:'1'});
  await s.post({text:'first once',operation_id:'write-first'});
  const second=await s.post({text:'must remain queued',operation_id:'write-second'}).then(r=>r.json());
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({RECOVERY_LATE:'1',RECOVERY_HOLD:'0'});
  const path=join(s.state,'rust-spike.json');
  try {
    const before=await readFile(path,'utf8');
    await chmod(path,0o400);await chmod(s.state,0o500);
    const stop=once(restarted.child,'exit');
    assert.notEqual((await stop)[0],0);
    assert.equal(await readFile(path,'utf8'),before);
    assert.ok((await providerCalls(s)).filter(c=>c.method==='turn/start').length<=2);
    assert.ok(JSON.parse(before).records.some(r=>r.submission.id===second.submission.id));
  } finally {await chmod(s.state,0o700);await chmod(path,0o600);}
});

test('turn/start accepted without a turn id is reconciled from resumed history, never paused or replayed',{timeout:20000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/recovery-provider.mjs'),RECOVERY_NO_ID:'1'});
  const first=await s.post({text:'unknown accepted input',operation_id:'unknown-first'}).then(r=>r.json());
  const second=await s.post({text:'queued after reconciliation',operation_id:'unknown-second'}).then(r=>r.json());
  const get=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  await until(()=>get(first.submission.id),r=>r.submission?.turn_id==='accepted-0',15000).catch(async e=>{throw Error(e.message+JSON.stringify(await fetch(s.base+'/api/state').then(r=>r.json()))+s.logs())});
  assert.equal((await get(second.submission.id)).submission.state,'queued');
  const resolved=(await get(first.submission.id)).submission;
  assert.equal(resolved.turn_id,'accepted-0','the accepted turn is found in resumed history by its input');
  assert.equal(resolved.state,'running','the provider still reports that turn running');
  assert.equal((await fetch(s.base+'/api/state').then(r=>r.json())).runtime.recovery.restarts,1);
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['unknown accepted input'],'never replayed');
});

for(const mode of ['omitted','native','summary'])test('history order keeps receipt steering between observed activity and its response, including restart '+mode,{timeout:20000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/history-order-provider.mjs'),ORDER_HISTORY_FILE:'order-history.json',ORDER_NATIVE_ECHO:mode==='native'?'1':'0',ORDER_SUMMARY:mode==='summary'?'1':'0'});
  const read=()=>fetch(s.base+'/api/state').then(r=>r.json());
  const first=await s.post({text:'first accepted input',operation_id:'order-first'}).then(r=>r.json());
  await until(read,x=>x.thread.turns.at(-1)?.items.some(i=>i.kind==='commandExecution'));
  const steer=await s.post({text:'later steering input',delivery:'steer',operation_id:'order-steer'}).then(r=>r.json());
  const expected=['first accepted input','assistant before steering','synthetic read only','later steering input','assistant after steering'];
  const check=state=>{
    const turn=state.thread.turns.find(t=>t.id===state.thread.turns.find(t=>t.items.some(i=>i.submissionId===first.submission.id))?.id);
    assert.deepEqual(turn.items.map(i=>i.text),expected);
    assert.deepEqual(turn.items.filter(i=>i.kind==='userMessage').map(i=>i.submissionId),[first.submission.id,steer.submission.id]);
    for(let n=0;n<12;n++)assert.deepEqual(state.thread.turns[n].items.map(i=>i.text),['past question '+n,'past answer '+n]);
    assert.doesNotMatch(state.thread.title,/Rust experiment/);
  };
  check(await until(read,x=>x.thread.turns.at(-1)?.status==='completed'));
  const surface=await fetch(s.base+'/.treer/agent').then(r=>r.json());assert.equal(surface.title,'Codex');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
  const restarted=await s.launch({AGENT_UI_SESSION_ID:'order-thread'});
  check(await fetch(restarted.base+'/api/state').then(r=>r.json()));
  const response=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'ordinary follow-up',operation_id:'order-followup'})});
  assert.equal(response.status,202);
  const final=await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.thread.turns.length===14&&x.thread.turns.at(-1).status==='completed');
  check(final);assert.equal(final.thread.turns.at(-1).items[0].text,'ordinary follow-up');
  const duplicate=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'later steering input',delivery:'steer',operation_id:'order-steer'})}).then(r=>r.json());
  assert.equal(duplicate.duplicate,true);assert.equal(duplicate.submission.id,steer.submission.id);
  const calls=(await readFile(join(s.root,'provider.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,2);assert.equal(calls.filter(c=>c.method==='turn/steer').length,1);
});

for(const scenario of ['distinct','identical','contradiction'])test('legacy unanchored steering yields to ordered native echoes instead of pinning users at top '+scenario,{timeout:20000},async t=>{
  const identical=scenario==='identical';
  const s=await fixture(t,{CODEX_BIN:resolve('tests/fixtures/legacy-steer-order-provider.mjs')});
  const first=await s.post({text:'initial question',operation_id:'legacy-first'}).then(r=>r.json());
  const steers=[];
  for(let n=0;n<2;n++){
    steers.push(await s.post({text:'steering question '+(identical?0:n),operation_id:'legacy-steer-'+n,delivery:'steer'}).then(r=>r.json()));
    await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.thread.turns[0]?.items.some(i=>i.text==='after steering '+n));
  }
  const unbound=await s.post({text:'cancelled input has no provider turn',operation_id:'legacy-unbound'}).then(r=>r.json());
  const unboundRecord=await fetch(s.base+'/v1/submissions/'+unbound.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(unboundRecord.submission.turn_id,null);
  assert.equal((await fetch(s.base+'/api/queue/'+unbound.submission.id,{method:'DELETE'})).status,200);
  const cancelled=await fetch(s.base+'/v1/submissions/'+unbound.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(cancelled.submission.state,'interrupted');
  assert.equal(cancelled.submission.turn_id,null);
  const exited=once(s.child,'exit');s.child.kill('SIGTERM');await exited;
  const path=join(s.state,'rust-spike.json'),journal=JSON.parse(await readFile(path,'utf8'));
  // Exact legacy shape: receipt turn identity/state exists but display anchor
  // was never persisted by the old runtime. Fixture-owned state only.
  for(const record of journal.records)delete record.projection_after;
  await writeFile(path,JSON.stringify(journal),{mode:0o600});
  const resumed=await s.launch({LEGACY_ORDER_ECHO:'1',LEGACY_ORDER_CONTRADICT:scenario==='contradiction'?'1':'0'});
  const pending=await fetch(resumed.base+'/api/state').then(r=>r.json());
  assert.deepEqual(pending.thread.turns[0].items.filter(i=>i.positionUnavailable).map(i=>i.submissionId),steers.map(x=>x.submission.id));
  assert.deepEqual(pending.thread.turns[0].items.slice(0,3).map(i=>i.text),['initial question','before steering','read-only tool']);
  assert.ok(pending.thread.turns.every(turn=>turn.items.every(i=>i.submissionId!==unbound.submission.id)),'receipt without turn_id must never borrow a turn');
  const final=await until(()=>fetch(resumed.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='blocked'||x.thread.turns[0]?.status==='completed');
  assert.notEqual(final.runtime.state,'blocked',JSON.stringify(final.runtime));
  assert.deepEqual(final.thread.turns[0].items.map(i=>i.text),['initial question','before steering','read-only tool','steering question 0','after steering 0','steering question '+(identical?0:1),'after steering 1']);
  assert.ok(final.thread.turns[0].items.every(i=>!i.positionUnavailable));
  assert.deepEqual(final.thread.turns[0].items.filter(i=>i.kind==='userMessage').map(i=>i.submissionId),[first.submission.id,...steers.map(x=>x.submission.id)]);
  const calls=await providerCalls(s);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,1);
  assert.equal(calls.filter(c=>c.method==='turn/steer').length,2);
  if(scenario==='contradiction'){
    // A contradicting later snapshot is logged and applied; the Agent stays live.
    await new Promise(r=>setTimeout(r,500));
    assert.notEqual((await fetch(resumed.base+'/api/state').then(r=>r.json())).runtime.state,'blocked');
    return;
  }
  const stop=once(resumed.child,'exit');resumed.child.kill('SIGTERM');await stop;
  const again=await s.launch({});
  const restored=await fetch(again.base+'/api/state').then(r=>r.json());
  assert.deepEqual(restored.thread.turns[0].items.map(i=>[i.text,i.submissionId]),final.thread.turns[0].items.map(i=>[i.text,i.submissionId]));
  const cancelledAgain=await fetch(again.base+'/v1/submissions/'+unbound.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(cancelledAgain.submission.state,'interrupted');
  assert.equal(cancelledAgain.submission.turn_id,null);
  // Optional evidence is synthetic fixture state only, for the browser owner.
  if(process.env.HISTORY239_EVIDENCE_DIR&&!identical){
    await writeFile(join(process.env.HISTORY239_EVIDENCE_DIR,'pending.json'),JSON.stringify(pending));
    await writeFile(join(process.env.HISTORY239_EVIDENCE_DIR,'final.json'),JSON.stringify(final));
  }
});

test('Rust serves its manifest while Controller registration probes it',async t=>{
  const s=await fixture(t,{RUST_SPIKE_REGISTER:'1',PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH});
  const manifest=await until(()=>readFile(join(s.root,'registration.json'),'utf8').then(JSON.parse),x=>x.instance_id==='contract-instance');
  assert.equal(manifest.protocol,'treer.agent-interface/v1');
});
test('Rust retries an accept error without ending the provider or HTTP server',async t=>{
  const s=await fixture(t,{RUST_SPIKE_HTTP_ACCEPT_ERROR_ONCE:'1'});
  assert.match(s.logs(),/http_accept_retry/);
  assert.equal((await fetch(s.base+'/api/state').then(r=>r.json())).ready,true);
  assert.equal((await s.post({text:'still serves after EMFILE',operation_id:'accept-retry'})).status,202);
  assert.equal(s.child.exitCode,null);
});
test('Rust bounds slow headers, header count and upgraded connection occupancy',{timeout:15000},async t=>{
  const s=await fixture(t,{RUST_SPIKE_HTTP_HEADER_MS:'300',RUST_SPIKE_HTTP_CONNECTION_LIMIT:'8'});
  const socket=connect({host:'127.0.0.1',port:Number(new URL(s.base).port)});socket.on('error',()=>{});
  t.after(()=>socket.destroy());await once(socket,'connect');socket.resume();
  const started=Date.now();socket.write('GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Trickle: ');
  const trickle=setInterval(()=>{if(!socket.destroyed)socket.write('a')},25);
  try {await until(async()=>socket.destroyed,Boolean);}finally{clearInterval(trickle);}
  assert.ok(Date.now()-started<3000,'header deadline is absolute, not renewed per byte');
  const tooMany=Object.fromEntries(Array.from({length:70},(_,n)=>['x-extra-'+n,'x']));
  assert.equal((await fetch(s.base+'/api/state',{headers:tooMany})).status,431);
  const sockets=[];t.after(()=>sockets.forEach(ws=>ws.terminate()));
  let refused=false;
  for(let n=0;n<10;n++) {
    const ws=new WebSocket(s.base.replace('http','ws')+'/ws');ws.on('error',()=>{});
    try {await once(ws,'open');sockets.push(ws);}catch{ws.terminate();refused=true;break;}
  }
  assert.equal(refused,true);assert.ok(sockets.length>=5&&sockets.length<=8);
  const closed=once(sockets[0],'close');sockets[0].close();await closed;
  const recovered=new WebSocket(s.base.replace('http','ws')+'/ws');recovered.on('error',()=>{});t.after(()=>recovered.terminate());await once(recovered,'open');
  const exit=once(s.child,'exit');const stop=Date.now();s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
  assert.ok(Date.now()-stop<8000,'open upgraded sockets cannot pin shutdown');
});
test('Rust bounds directory enumeration without blocking concurrent state reads',{timeout:20000},async t=>{
  const s=await fixture(t);
  const directory=join(s.root,'cwd/many');await mkdir(directory);
  for(let first=0;first<4097;first+=64)await Promise.all(Array.from({length:Math.min(64,4097-first)},(_,n)=>writeFile(join(directory,'entry-'+(first+n)),'')));
  const listing=fetch(s.base+'/v1/files/tree?path=many');
  const state=await fetch(s.base+'/api/state').then(r=>r.json());assert.equal(state.ready,true);
  const response=await listing;assert.equal(response.status,413);
  assert.match((await response.json()).error.message,/4096 entry/);
  assert.equal((await fetch(s.base+'/v1/files/tree?path=')).status,200);
});

test('Rust cancelled directory requests retain worker slots until the workers exit',{timeout:15000},async t=>{
  const s=await fixture(t,{RUST_SPIKE_FILES_WORKER_GATE:'filesystem-worker-gate'});
  const gate=join(s.root,'cwd/filesystem-worker-gate');await mkdir(gate);
  const controllers=[new AbortController(),new AbortController()];
  const requests=controllers.map(controller=>fetch(s.base+'/v1/files/tree?path=',{signal:controller.signal}).catch(()=>null));
  try {
    await until(()=>readdir(gate),names=>names.length===2);
    controllers.forEach(controller=>controller.abort());await Promise.all(requests);
    assert.equal((await fetch(s.base+'/api/state').then(r=>r.json())).ready,true);
    const started=Date.now();
    assert.equal((await fetch(s.base+'/v1/files/tree?path=')).status,429);
    assert.ok(Date.now()-started>=800,'busy workers allow a bounded wait for normal navigation bursts');
  } finally {await rm(gate,{recursive:true,force:true});}
  await until(()=>fetch(s.base+'/v1/files/tree?path='),response=>response.status===200);
});

test('Rust canonicalizes a configured cwd alias and reports root failures without paths',{timeout:15000},async t=>{
  const s=await fixture(t);
  await writeFile(join(s.root,'cwd/alias-proof.txt'),'accepted through canonical cwd');
  const alias=join(s.root,'cwd-alias');
  await symlink(join(s.root,'cwd'),alias);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({AGENT_UI_CWD:alias});
  const preview=await fetch(restarted.base+'/v1/files?path=alias-proof.txt').then(r=>r.json());
  assert.equal(preview.content,'accepted through canonical cwd');
  const missing=join(s.root,'synthetic-root-do-not-echo');
  const regular=join(s.root,'synthetic-regular-do-not-echo');
  await writeFile(regular,'not a directory');
  for(const target of [missing,regular]) {
    const child=spawn(binary,[],{cwd:join(s.root,'cwd'),env:hermeticEnv({HOME:s.root,AGENT_UI_CWD:target}),stdio:['ignore','ignore','pipe']});
    s.trackChild(child);
    let stderr='';child.stderr.on('data',chunk=>stderr+=chunk);
    const [code]=await once(child,'close');
    assert.equal(code,1);
    assert.match(stderr,/workspace_file_root_unavailable/);
    assert.doesNotMatch(stderr,/synthetic-(root|regular)-do-not-echo/);
    assert.ok(!stderr.includes(s.root));
  }
});

test('many-turn recent window evicts finished history without losing receipts or blocking dispatch',{timeout:60000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid'});
  const baseline=await rssKiB(s.child.pid);let peak=baseline;const admitted=[];
  for(let n=0;n<60;n++) {
    const response=await s.post({text:`bounded history ${n}`,operation_id:`history-${n}`});assert.equal(response.status,202);
    const receipt=await response.json();admitted.push(receipt.submission.id);
    await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done').catch(async e=>{throw Error(e.message+' n='+n+' '+JSON.stringify((await fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json())).submission)+JSON.stringify((await fetch(s.base+'/api/state').then(r=>r.json())).runtime)+s.logs().slice(-1500));});
    if(n===0) {
      const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
      const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
      saved.records[0].source_messages=[{messageId:'fixture_unacked_message',deliveryId:'fixture_unacked_delivery',ackPending:true}];
      await writeFile(join(s.state,'rust-spike.json'),JSON.stringify(saved),{mode:0o600});
      const next=await s.launch({});s.child=next.child;s.base=next.base;
      s.post=body=>fetch(s.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify(body)});
    }
    peak=Math.max(peak,await rssKiB(s.child.pid));
  }
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.ready,true);assert.equal(state.runtime.state,'idle');assert.equal(state.thread.hasOlderItems,true);
  assert.ok(state.thread.turns.length<=50);assert.ok(Buffer.byteLength(JSON.stringify(state.thread.turns))<17*1024*1024);
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(journal.dispatchBlocked,false);assert.equal(journal.records.length,admitted.length);
  assert.ok(journal.records.every(r=>r.submission.state==='done'));
  assert.equal(journal.records[0].source_messages[0].ackPending,true,'history eviction must not discard an unacknowledged Core message');
  assert.ok(!journal.turns,'older turns are not copied into the journal');
  assert.ok(peak<256*1024,`recent-window RSS ${peak} KiB exceeds256 MiB`);
  const beforeRefusedResume=await readFile(join(s.state,'rust-spike.json'),'utf8');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({HISTORY_LARGE_RESUME:'1',HISTORY_LARGE_ACTIVE:'1'},false);
  // An active thread whose only in-progress turn is old still starts: that turn is followed.
  const resumed=await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.ready===true,45000);
  assert.notEqual(resumed.runtime.state,'blocked');
  assert.equal(JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8')).records.length,JSON.parse(beforeRefusedResume).records.length,'receipts are preserved');
  t.diagnostic(JSON.stringify({baselineRssKiB:baseline,peakRssKiB:peak,retainedTurns:state.thread.turns.length,receipts:admitted.length}));
});

test('streaming deltas stop at admission limit without replacing accepted text',{timeout:30000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_DELTA:'1'});
  const receipt=await s.post({text:'bounded stream',operation_id:'stream'}).then(r=>r.json());
  // A delta past the history limit stops the provider, which is restarted.
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>state.runtime.recovery.restarts>=1||/history limit/.test(state.runtime.recovery.lastFailure??''),20000);
  assert.match(String(state.runtime.recovery.lastFailure),/history limit/);
  const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.ok(saved.records.some(r=>r.submission.id===receipt.submission.id),'the receipt is kept');
  const pid=Number(await readFile(join(s.root,'cwd/provider.pid'),'utf8'));
  await until(async()=>{try{process.kill(pid,0);return false;}catch(e){if(e.code==='ESRCH')return true;throw e;}},Boolean);
});

test('invalid provider turn identity restarts the provider without echoing provider content',{timeout:15000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_INVALID_ID:'1'});
  const receipt=await s.post({text:'malformed completion',operation_id:'invalid-id'}).then(r=>r.json());
  // A malformed completion stops the provider; it is restarted, never paused.
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>/Invalid provider turn identity/.test(state.runtime?.recovery?.lastFailure??''));
  assert.notEqual(state.runtime.state,'blocked');
  assert.doesNotMatch(JSON.stringify(state),/synthetic-do-not-echo/);
  assert.equal((await fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers})).status,200);
  const pid=Number(await readFile(join(s.root,'cwd/provider.pid'),'utf8'));
  assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

test('terminal reconciliation selects an old target by ID without a completion notification or duplicate dispatch',{timeout:15000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_RECONCILE_OLD:'1',HISTORY_FRAME_LOG:'frames.jsonl'});
  const receipt=await s.post({text:'attested local input omitted by provider',operation_id:'old-reconciliation'}).then(r=>r.json());
  const result=await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done');
  assert.equal(result.submission.turn_id,'history-1');
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.thread.status,'idle');assert.ok(state.thread.turns.length<=50);
  assert.ok(Buffer.byteLength(JSON.stringify(state.thread.turns))<16*1024*1024);
  assert.ok(state.thread.turns.flatMap(t=>t.items).some(i=>i.kind==='userMessage'&&i.text==='attested local input omitted by provider'));
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.ok(frames.some(f=>f.method==='thread/read'));
  assert.equal(frames.filter(f=>f.method==='turn/start').length,1,'no automatic redispatch');
  const retry=await s.post({text:'attested local input omitted by provider',operation_id:'old-reconciliation'});
  assert.equal(retry.status,202);
  const after=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(after.filter(f=>f.method==='turn/start').length,1);
});

for(const rich of [false,true])test('provider user echo is projected once'+(rich?' after787-turn resume and node-rich live notification':''),{timeout:60000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl',HISTORY_ECHO_USER:'1',...(rich?{HISTORY_LIVE_SHAPE:'8000',AGENT_UI_SESSION_ID:'synthetic-history'}:{})});
  const initial=await fetch(s.base+'/api/state').then(r=>r.json());
  if(rich){assert.equal(initial.history.totalTurns,787);assert.equal(initial.thread.turns.length,50);assert.ok(initial.runtime.provider.historyScan.frameBytes>45*1024*1024);}
  const r=await s.post({text:'read-only synthetic question',operation_id:'native-echo'}).then(r=>r.json());
  let maxUsers=0;
  const final=await until(async()=>{const state=await fetch(s.base+'/api/state').then(r=>r.json());const turn=state.thread.turns.find(t=>t.id.startsWith('history-'));maxUsers=Math.max(maxUsers,turn?.items.filter(i=>i.kind==='userMessage').length??0);return state;},x=>x.runtime.state==='blocked'||x.thread.turns.some(t=>t.id.startsWith('history-')&&t.status==='completed'));
  assert.notEqual(final.runtime.state,'blocked',JSON.stringify(final.runtime.provider));
  const turn=final.thread.turns.find(t=>t.items.some(i=>i.submissionId===r.submission.id));
  assert.equal(turn.items.filter(i=>i.kind==='userMessage').length,1);
  assert.equal(maxUsers,1,'native user item lifecycle never duplicates the local projection');
  assert.equal(turn.items.find(i=>i.kind==='userMessage').submissionId,r.submission.id);
  const retry=await s.post({text:'read-only synthetic question',operation_id:'native-echo'}).then(r=>r.json());assert.equal(retry.duplicate,true);
  assert.ok(final.thread.turns.length<=50);assert.ok(Buffer.byteLength(JSON.stringify(final.thread.turns))<=16*1024*1024);
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(frames.filter(f=>f.method==='turn/start').length,1,'the echo and retry never dispatch a second turn');
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));assert.equal(journal.records.length,1);
  assert.equal((await fetch(s.base+'/v1/submissions/'+r.submission.id,{headers:s.headers}).then(r=>r.json())).submission.state,'done');
});

test('identical accepted steer text retains both receipt projections across native echoes',async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl',HISTORY_IDENTICAL_ECHO:'1'});
  const sender1={kind:'user',agent_id:'ag_first',path:'/fixture/first',name:'First'};
  const sender2={kind:'user',agent_id:'ag_second',path:'/fixture/second',name:'Second'};
  const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
  const first=await s.post({text:'same accepted text',operation_id:'identical-first',sender:sender1,attachment_ids:[attachment.id]}).then(r=>r.json());
  await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.thread.turns.some(t=>t.items.some(i=>i.submissionId===first.submission.id))&&x.thread.activeTurnId);
  const second=await s.post({text:'same accepted text',operation_id:'identical-second',delivery:'steer',sender:sender2}).then(r=>r.json());
  assert.notEqual(first.submission.id,second.submission.id);
  let maxUsers=0;
  const state=await until(async()=>{const state=await fetch(s.base+'/api/state').then(r=>r.json());maxUsers=Math.max(maxUsers,state.thread.turns.flatMap(t=>t.items).filter(i=>i.kind==='userMessage').length);return state;},x=>x.thread.turns.some(t=>t.status==='completed'));
  const users=state.thread.turns.flatMap(t=>t.items).filter(i=>i.kind==='userMessage');
  assert.equal(users.length,2);assert.equal(maxUsers,2);
  assert.deepEqual(users.map(i=>i.submissionId).sort(),[first.submission.id,second.submission.id].sort());
  const firstItem=users.find(i=>i.submissionId===first.submission.id),secondItem=users.find(i=>i.submissionId===second.submission.id);
  assert.deepEqual(firstItem.sender,sender1);assert.deepEqual(secondItem.sender,sender2);
  assert.equal(firstItem.attachments[0].id,attachment.id);assert.deepEqual(secondItem.attachments,[]);
  const duplicate=await s.post({text:'same accepted text',operation_id:'identical-second',delivery:'steer',sender:sender2}).then(r=>r.json());assert.equal(duplicate.duplicate,true);
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(frames.filter(f=>f.method==='turn/start').length,1);assert.equal(frames.filter(f=>f.method==='turn/steer').length,1);
});

test('live overflow with a read-only journal reports checkpoint failure and exits nonzero for restart',{timeout:30000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_LIVE_SHAPE:'80000',HISTORY_LIVE_DELAY:'1500'});
  const journal=join(s.state,'rust-spike.json');
  const receipt=await s.post({text:'accepted before read-only fence',operation_id:'readonly-overflow'}).then(r=>r.json());
  await until(()=>readFile(journal,'utf8').then(JSON.parse),x=>x.records.some(r=>r.submission.id===receipt.submission.id&&r.submission.state==='running'));
  const before=await readFile(journal,'utf8');
  try {
    await chmod(journal,0o400);await chmod(s.state,0o500);
    // The provider overflow triggers a restart whose checkpoint cannot be
    // written: the launcher exits non-zero on its own for the Host to restart.
    const [code]=await once(s.child,'exit');assert.notEqual(code,0);
    assert.equal(await readFile(journal,'utf8'),before,'failed checkpoint never replaces accepted durable intent');
    assert.match(s.logs(),/checkpoint_failure=1/);
  } finally {await chmod(s.state,0o700);await chmod(journal,0o600);}
});

test('live node overflow restarts the provider and never resends accepted work',{timeout:60000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl',HISTORY_LIVE_SHAPE:'80000',AGENT_UI_SESSION_ID:'synthetic-history'});
  const receipt=await s.post({text:'synthetic accepted input',operation_id:'shape-overflow'}).then(r=>r.json());
  // An oversized frame stops the provider; it is restarted and resumed.
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.recovery.restarts>=1,30000);
  assert.match(String(state.runtime.recovery.lastFailure),/provider_json_limit/);
  assert.ok(!JSON.stringify(state.runtime).includes('synthetic accepted input'));
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(journal.records.length,1);assert.equal(journal.records[0].submission.id,receipt.submission.id);
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(frames.filter(f=>f.method==='turn/start').length,1,'restart never resends uncertain work');
});

test('streamed 50 MiB resume retains a recent window, fetches older pages on demand and completes the next turn',{timeout:60000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl'});
  const originalReceipt=await s.post({text:'local text belongs only to this requested old turn',operation_id:'old-page-projection'}).then(r=>r.json());
  await until(()=>fetch(s.base+'/v1/submissions/'+originalReceipt.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  // Assert no preload by this resumed process, not observer calls made by the
  // previous process while finishing the setup receipt.
  await writeFile(join(s.root,'cwd/frames.jsonl'),'');
  let peak=0,pid=null,busy=false;
  const sample=setInterval(async()=>{if(!pid||busy)return;busy=true;try{peak=Math.max(peak,await rssKiB(pid));}catch{}finally{busy=false;}},25);
  t.after(()=>clearInterval(sample));
  // s.launch records children before readiness; watch only our sealed child.
  const launch=s.launch({HISTORY_LARGE_RESUME:'1',HISTORY_PROJECTION_OLD:'1'},false);
  const restarted=await launch;pid=restarted.child.pid;
  await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.ready===true,45000);
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.ok(frames.some(x=>x.method==='thread/resume'&&x.bytes>30*1024*1024&&x.bytes<60*1024*1024));
  assert.equal(frames.filter(x=>x.method==='thread/read').length,0,'no preload');
  const state=await fetch(restarted.base+'/api/state').then(r=>r.json());
  assert.equal(state.thread.id,'synthetic-history');
  assert.equal(state.runtime.provider.transport,'open');
  const history=await fetch(restarted.base+'/v1/transcript?page=0&limit=1').then(r=>r.json());
  assert.equal(history.page_count,50);
  assert.equal(state.thread.turns[0].id,'old-480');
  const cursor=state.history.cursor;assert.equal(typeof cursor,'string');
  assert.equal((await fetch(restarted.base+'/api/history/older',{method:'POST',body:JSON.stringify({cursor:'bad-cursor'})})).status,400);
  const older=await new Promise((resolve,reject)=>{
    const request=httpRequest(restarted.base+'/api/history/older',{method:'POST',headers:{'content-type':'application/json'}},response=>{response.pause();resolve(response);});
    request.on('error',reject);request.end(JSON.stringify({cursor}));
  });
  t.after(()=>older.destroy());assert.equal(older.statusCode,200);
  // Let the rate window expire while the first large response is paused.
  // A transfer slot, not just the request timer, must prevent a second scan.
  await new Promise(resolve=>setTimeout(resolve,2300));
  const concurrent=await fetch(restarted.base+'/api/history/older',{method:'POST',body:JSON.stringify({cursor})});
  assert.equal(concurrent.status,429);
  const chunks=[];for await(const chunk of older)chunks.push(chunk);
  const page=JSON.parse(Buffer.concat(chunks).toString());
  assert.equal(page.thread.turns[0].id,'history-1');assert.equal(page.thread.turns.length,50);
  assert.ok(page.thread.turns[0].items.some(i=>i.kind==='userMessage'&&i.text==='local text belongs only to this requested old turn'&&i.submissionId===originalReceipt.submission.id));
  assert.equal(page.history.page,true);
  assert.ok(page.history.cursor!==cursor);
  assert.ok(peak<256*1024,`launcher peak ${peak} KiB must stay below256 MiB`);
  t.diagnostic(JSON.stringify({peakLauncherRssKiB:peak,frameBytes:frames.find(x=>x.method==='thread/resume').bytes,historyScan:state.runtime.provider.historyScan,settledRssKiB:await rssKiB(pid)}));
  const receipt=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'after large resume',operation_id:'after-large-resume'})});
  assert.equal(receipt.status,202);
  const accepted=await receipt.json();
  await until(()=>fetch(restarted.base+'/v1/submissions/'+accepted.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done');
  assert.equal(restarted.child.exitCode,null);
});

for(const extra of [0,1])test('streaming frame256 MiB boundary +'+extra,{timeout:120000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid'});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const original=await readFile(join(s.state,'rust-spike.json'),'utf8');
  const restarted=await s.launch({HISTORY_FRAME_BYTES:String(256*1024*1024+extra),HISTORY_FRAME_LOG:'over-limit-frames.jsonl'},false);
  if(!extra){
    const state=await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.ready,90000);
    assert.equal(state.runtime.provider.transport,'open');t.diagnostic(JSON.stringify({historyScan:state.runtime.provider.historyScan,settledRssKiB:await rssKiB(restarted.child.pid)}));return;
  }
  // Startup failure exits non-zero so the Host restarts the launcher.
  assert.equal((await once(restarted.child,'exit'))[0],1);
  const frames=(await readFile(join(s.root,'cwd/over-limit-frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.ok(frames.some(x=>x.method==='thread/resume'&&x.bytes===256*1024*1024+1));
  assert.ok(!frames.some(x=>x.method==='thread/start'),'no fresh-thread fallback');
  assert.equal(await readFile(join(s.state,'rust-spike.json'),'utf8'),original);
});

for(const malformed of [false,true])test((malformed?'malformed discarded old turn':'oversized resumed history')+' exits for restart without rewriting the durable journal',{timeout:10000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid'});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const original=await readFile(join(s.state,'rust-spike.json'),'utf8');
  const portFile=join(s.root,'oversized-port');
  const env=hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(launcherRoot,'public'),CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'oversized.pid',...(malformed?{HISTORY_MALFORMED_OLD:'1'}:{HISTORY_OVERSIZED:'1'}),AGENT_UI_PORT_FILE:portFile});
  const child=spawn(binary,[],{cwd:join(s.root,'cwd'),env,stdio:'ignore'});
  s.trackChild(child);
  // Startup failure exits non-zero so the Host restarts the launcher.
  assert.equal((await once(child,'exit'))[0],1);
  assert.equal(await readFile(join(s.state,'rust-spike.json'),'utf8'),original);
  const pid=Number(await readFile(join(s.root,'cwd/oversized.pid'),'utf8'));
  assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

test('same manifest and initial transcript contract',async t=>{
  const s=await fixture(t);
  const manifest=await fetch(s.base+'/v1/manifest').then(r=>r.json());
  const contract=JSON.parse(await readFile(join(launcherRoot,'interface-contract.json'),'utf8'));
  assert.equal(manifest.protocol,contract.protocol);assert.deepEqual(manifest.capabilities,contract.capabilities);
  const transcript=await fetch(s.base+'/v1/transcript?page=0&limit=1').then(r=>r.json());
  assert.equal(transcript.page_count,2);assert.equal(transcript.next_page,1);assert.equal(transcript.entries[0].content,'remember 0');
  assert.equal((await fetch(s.base+'/')).status,200);
});
test('browser mutations return full state with the requested history window',async t=>{
  const s=await fixture(t);
  for(const [path,body] of [
    ['/api/settings',{model:'synthetic-model',history_limit:150}],
    ['/api/prompt',{prompt:'browser contract',history_limit:150}],
    ['/api/interrupt',{history_limit:150}],
  ]) {
    const response=await fetch(s.base+path,{method:'POST',body:JSON.stringify(body)});
    assert.equal(response.status,200,path);
    const state=await response.json();
    assert.equal(state.ready,true,path);
    assert.equal(state.history.limit,150,path);
    assert.ok(state.thread.id,path);
    assert.ok(Array.isArray(state.thread.turns),path);
    assert.ok(Array.isArray(state.queue),path);
    assert.ok(state.runtime.state,path);
    if(path==='/api/settings')assert.equal(state.thread.model,'synthetic-model');
    if(path==='/api/prompt')assert.ok(state.submission.id);
  }
  const missing=await fetch(s.base+'/api/queue/missing',{method:'DELETE'});
  assert.equal(missing.status,404);
  assert.equal(typeof (await missing.json()).error,'string');
});
test('HTML permissions and script media headers match browser expectations',async t=>{
  const s=await fixture(t);
  const html=await fetch(s.base+'/');
  assert.match(html.headers.get('permissions-policy'),/camera=\(\)/);
  assert.ok(html.headers.get('content-security-policy'));
  const script=await fetch(s.base+'/ui.js');
  assert.equal(script.status,200);
  assert.match(script.headers.get('content-type'),/charset=utf-8/);
  assert.equal(script.headers.get('content-security-policy'),null);
});
test('mutating-route native versus browser authorization matrix matches Node',async t=>{
  const s=await fixture(t);
  const routes=[
    ['POST','/v1/prompts',{text:'blocked',operation_id:'native'},false],
    ['DELETE','/v1/queue/missing',null,false],
    ['POST','/api/prompt',{prompt:'native trusted-loopback prompt'},true],
    ['POST','/api/interrupt',{},true],
    ['POST','/api/settings',{model:'synthetic-model'},true],
    ['POST','/v1/abort',{},true],
    ['DELETE','/api/queue/missing',null,true],
    ['PUT','/api/goal',{objective:'native goal',operationId:'auth-matrix-goal'},true],
    ['DELETE','/api/goal',null,true],
    ['POST','/api/goal/reconcile',{},true],
  ];
  for(const [method,path,body,nativeAllowed] of routes){
    for(const headers of [{origin:'null'},{'sec-fetch-mode':'cors'},{...s.headers,'x-treer-interface-instance':'stale'}])
      assert.equal((await fetch(s.base+path,{method,headers,body:body===null?undefined:JSON.stringify(body)})).status,403,path);
    const native=await fetch(s.base+path,{method,body:body===null?undefined:JSON.stringify(body)});
    if(nativeAllowed)assert.notEqual(native.status,403,path);else assert.equal(native.status,403,path);
  }
  for(const path of ['/v1/queue','/v1/submissions/missing'])assert.equal((await fetch(s.base+path)).status,403,path);
  const legacy=await fetch(s.base+'/api/prompt',{method:'POST',headers:s.headers,body:JSON.stringify({prompt:'legacy metadata ignored',sender:{agent_id:'ag_forged',path:'/forged',name:'forged'},notify:{on:['done'],to:'ag_forged'}})}).then(r=>r.json());
  const submission=await fetch(s.base+'/v1/submissions/'+legacy.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(submission.submission.sender,null);
});
test('Host, browser provenance and attested sender negative matrix (HTTP + WebSocket)',async t=>{
  const s=await fixture(t);
  for(const path of ['/','/api/state','/v1/transcript','/v1/prompts','/api/interrupt'])for(const headers of [{host:'evil.invalid'},{origin:'null'},{'sec-fetch-mode':'navigate'},{...s.headers,'x-treer-interface-instance':'stale'},{...s.headers,origin:'https://evil.invalid'}]) {
    assert.equal((await fetch(s.base+path,{headers})).status,403,path);
  }
  const ui={'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'contract-instance',origin:'https://proxy.invalid'};
  assert.equal((await fetch(s.base+'/api/state',{headers:ui})).status,200);
  assert.equal((await fetch(s.base+'/v1/prompts',{method:'POST',headers:ui,body:JSON.stringify({text:'hello',operation_id:'x',sender:{agent_id:'a',path:'/a',name:'a'}})})).status,403);
  for(const headers of [{origin:'null'},{host:'evil.invalid'},{...ui,'x-treer-interface-instance':'stale'}]) {
    const status=await new Promise((done,reject)=>{const ws=new WebSocket(s.base.replace('http','ws')+'/ws',{headers});ws.on('error',()=>{});ws.once('unexpected-response',(_,r)=>{r.resume();ws.terminate();done(r.statusCode)});ws.once('open',()=>{ws.close();reject(Error('hostile WS accepted'))});});
    assert.equal(status,403);
  }
  const ws=new WebSocket(s.base.replace('http','ws')+'/ws',{headers:ui});
  const [data]=await once(ws,'message');assert.equal(JSON.parse(String(data)).type,'state');ws.close();
});
test('bounded input and fixed parser errors do not echo synthetic secrets',async t=>{
  const s=await fixture(t);const secret='SYNTHETIC_SECRET_158';
  for(const body of [`{"x":"${secret}"broken`,'null','[]','5']){
    const r=await fetch(s.base+'/api/prompt',{method:'POST',body});assert.equal(r.status,400);assert.ok(!(await r.text()).includes(secret));
  }
  assert.equal((await fetch(s.base+'/api/prompt',{method:'POST',body:'x'.repeat(128*1024+1)})).status,413);
  assert.ok(!s.logs().includes(secret));
  const ws=new WebSocket(s.base.replace('http','ws')+'/ws');ws.on('error',()=>{});await once(ws,'open');
  const close=once(ws,'close');ws.send('x'.repeat(16385));assert.equal((await close)[0],1009);
  assert.equal((await fetch(s.base+'/api/state')).status,200);
});
test('durable operation deduplication, conflicting retry and correlated terminal result',async t=>{
  const s=await fixture(t);
  const body={text:'hello',operation_id:'once',delivery:'queue'};
  const first=await s.post(body).then(r=>r.json());
  assert.ok(first.submission.id);
  const duplicate=await s.post(body).then(r=>r.json());assert.equal(duplicate.submission.id,first.submission.id);assert.equal(duplicate.duplicate,true);
  assert.ok((await s.post({...body,text:'different'})).status>=400);
  const result=await until(()=>fetch(s.base+'/v1/submissions/'+first.submission.id,{headers:s.headers}).then(r=>r.json()),x=>['done','failed'].includes(x.submission?.state));
  assert.ok(result.submission.turn_id);
  const encoded=await fetch(s.base+'/v1/submissions/'+controllerSegment(first.submission.id),{headers:s.headers});
  assert.equal(encoded.status,200);
  assert.deepEqual(await encoded.json(),result);
  for(const invalid of ['%FF','%2Fbad','%252D','bad%'])
    assert.equal((await fetch(s.base+'/v1/submissions/'+invalid,{headers:s.headers})).status,400);
});
test('Rust keeps journaled user text and attested sender after completion and restart',async t=>{
  const s=await fixture(t);
  const sender={kind:'user',agent_id:'ag_sender',path:'/fixture/sender',name:'Fixture sender'};
  const accepted=await s.post({text:'durable user text',operation_id:'durable-user',sender}).then(r=>r.json());
  await until(()=>fetch(s.base+'/v1/submissions/'+accepted.submission.id,{headers:s.headers}).then(r=>r.json()),x=>['done','failed'].includes(x.submission?.state));
  const check=async base=>{
    const state=await fetch(base+'/api/state').then(r=>r.json());
    const items=state.thread.turns.flatMap(t=>t.items).filter(i=>i.kind==='userMessage'&&i.text==='durable user text');
    assert.equal(items.length,1);
    assert.deepEqual(items[0].sender,sender);
    assert.equal(items[0].submissionId,accepted.submission.id);
    return items[0].id;
  };
  const id=await check(s.base);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});
  assert.equal(await check(restarted.base),id);
});
test('Rust corrupt journal refuses startup before provider dispatch and preserves evidence',async t=>{
  const s=await fixture(t);
  const accepted=await s.post({text:'private text must not enter error logs',operation_id:'corruption-check'}).then(r=>r.json());
  await until(()=>fetch(s.base+'/v1/submissions/'+accepted.submission.id,{headers:s.headers}).then(r=>r.json()),x=>['done','failed'].includes(x.submission?.state));
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const path=join(s.state,'rust-spike.json');
  const journal=JSON.parse(await readFile(path,'utf8'));
  delete journal.records[0].submission.id;
  const bytes=JSON.stringify(journal);
  await writeFile(path,bytes,{mode:0o600});
  const providerBefore=await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null);
  const child=spawn(binary,[],{cwd:join(s.root,'cwd'),env:hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(launcherRoot,'public'),CODEX_BIN:join(launcherRoot,'tests/fake-codex.mjs'),RUST_SPIKE_PROVIDER_LOG:join(s.root,'provider.jsonl')}),stdio:['ignore','pipe','pipe']});
  let logs='';child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
  const [code]=await once(child,'exit');
  assert.notEqual(code,0);
  assert.match(logs,/private_state_invalid/);
  assert.doesNotMatch(logs,/private text|corruption-check/);
  assert.equal(await readFile(path,'utf8'),bytes);
  assert.equal(await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null),providerBefore);
});
test('Rust refuses concurrent writers and mismatched resume; existing Rust journal is not re-imported',{timeout:15000},async t=>{
  const s=await fixture(t);
  const path=join(s.state,'rust-spike.json');
  const env=hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:s.controller.url,TREER_AGENT_PRIVATE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(launcherRoot,'public'),CODEX_BIN:join(launcherRoot,'tests/fake-codex.mjs'),RUST_SPIKE_PROVIDER_LOG:join(s.root,'provider.jsonl')});
  const refuses=async (code,extra={})=>{
    const bytes=await readFile(path);
    const providerBefore=await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null);
    await assert.rejects(execFileAsync(binary,[],{cwd:join(s.root,'cwd'),env:{...env,...extra},timeout:3000,killSignal:'SIGKILL'}),error=>{
      assert.equal(error.killed,false);assert.equal(error.code,1);assert.ok(error.stderr.includes(code));
      assert.ok(!error.stderr.includes(s.root));return true;
    });
    assert.deepEqual(await readFile(path),bytes);
    assert.equal(await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null),providerBefore);
  };
  await refuses('private_state_in_use');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  await refuses('private_state_session_mismatch',{AGENT_UI_SESSION_ID:'different-synthetic-session'});
  const legacy=join(s.root,'cwd/.treer/agents/ag_contract');await mkdir(legacy,{recursive:true,mode:0o700});
  const evidence='private legacy evidence; do not parse or discard';await writeFile(join(legacy,'queue.json'),evidence,{mode:0o600});
  assert.equal(await readFile(join(legacy,'queue.json'),'utf8'),evidence);
  const restarted=await s.launch({});
  assert.equal((await fetch(restarted.base+'/api/state').then(r=>r.json())).ready,true,'lease must be released on exit');
  assert.equal(await readFile(join(legacy,'queue.json'),'utf8'),evidence,'existing Rust state never reimports stale Node files');
});
test('Rust history failure is visible but live receipts still publish',{timeout:15000},async t=>{
  const s=await fixture(t);const stopped=once(s.child,'exit');s.child.kill('SIGTERM');await stopped;
  const history=join(s.state,'rust-journal-history');await mkdir(history,{recursive:true,mode:0o700});
  const extra=join(history,'unexpected-private-entry');await writeFile(extra,'keep untouched',{mode:0o600});
  const restarted=await s.launch({});
  const snapshot=await fetch(restarted.base+'/api/state').then(r=>r.json());
  assert.equal(snapshot.runtime.journalHistory.state,'journal_history_unavailable');assert.equal(snapshot.ready,true);
  const response=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'history failure must not block live work',operation_id:'history-live-op',delivery:'queue'})});
  assert.equal(response.status,202);
  const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.ok(saved.records.some(r=>r.operation==='history-live-op'));
  assert.equal(await readFile(extra,'utf8'),'keep untouched');
});
for(const scenario of ['normal','concurrent','mismatch','reopen','partialObserver','readAnchors'])test('producer-shaped stale history resume then real new-turn path '+scenario,{timeout:150000},async t=>{
  // Concurrent, mismatched, reopened or partial history no longer stops the
  // Agent: the newest in-progress turn wins and every receipt is settled.
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl',HISTORY_STALE_SNAPSHOT:'1',HISTORY_BACKWARD_ENVELOPE:scenario==='partialObserver'?'observerPartial':scenario==='readAnchors'?'read':'both',...({concurrent:{HISTORY_CONCURRENT_ACTIVE:'1'},mismatch:{HISTORY_MISMATCH_START:'1'},reopen:{HISTORY_REOPEN_TERMINAL:'1'}}[scenario]||{})});
  const stopped=once(s.child,'exit');s.child.kill('SIGTERM');await stopped;
  await unlink(join(s.state,'rust-spike.json'));
  const imported=await s.launch({AGENT_UI_SESSION_ID:'synthetic-history'});
  let state=await fetch(imported.base+'/api/state').then(r=>r.json());
  assert.equal(state.thread.id,'synthetic-history');assert.equal(state.thread.status,'idle');
  assert.equal(state.history.totalTurns,778);assert.equal(state.thread.turns.length,50);
  assert.equal(state.thread.turns.at(-1).status,'interrupted');
  assert.equal(state.runtime.provider.historyScan.staleInProgressCount,4);
  assert.ok(state.runtime.provider.historyScan.frameBytes>45*1024*1024);
  const post=()=>fetch(imported.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'synthetic acceptance only',operation_id:'stale-after-import'})});
  const accepted=await post();
  assert.ok((scenario==='mismatch'?[202,400]:[202]).includes(accepted.status),'a mismatch discovered during the RPC may reject the HTTP call after its durable intent');
  // concurrent: the provider keeps our turn running; the Agent stays live (working), not blocked.
  state=await until(()=>fetch(imported.base+'/api/state').then(r=>r.json()),x=>scenario==='concurrent'?x.thread.turns.some(t=>t.id==='genuinely-other-live-turn')&&x.runtime.state==='working':x.thread.turns.some(t=>t.id==='history-779'&&t.status==='completed')&&JSON.stringify(x.runtime.recovery.unresolved)==='[]',100000);
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  {
    assert.equal(state.ready,true);assert.equal(state.runtime.provider.transport,'open');
    const turn=state.thread.turns.find(t=>t.id==='history-779');
    assert.equal(turn.items.filter(i=>i.kind==='userMessage').length,1);
    if(scenario!=='concurrent')assert.equal(turn.items.filter(i=>i.kind==='agentMessage').length,1);
    assert.equal((await (await post()).json()).duplicate,true);
    const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
    if(scenario!=='concurrent')assert.ok(frames.some(f=>f.method==='thread/read'&&f.bytes>45*1024*1024),'observer must parse full stale history while the new turn is active');
    assert.equal(journal.records.find(r=>r.operation==='stale-after-import').submission.state,{concurrent:'running',mismatch:'failed'}[scenario]??'done');
    const older=await fetch(imported.base+'/api/history/older',{method:'POST',body:JSON.stringify({cursor:state.history.cursor})}).then(r=>r.json());
    assert.equal(older.thread.turns.find(t=>t.id==='old-700').status,'interrupted');
    assert.ok(older.thread.turns.every(t=>t.status!=='inProgress'),'old page uses the same stale normalization');
  }
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(frames.filter(f=>f.method==='turn/start').length,1);
  t.diagnostic(JSON.stringify({historyScan:state.runtime.provider.historyScan,rssKiB:await rssKiB(imported.child.pid),scenario}));
});

for(const [name,env] of [['paged:one',{HISTORY_AUTHORITY_CASE:'paged:one'}],['duplicateId:one',{HISTORY_AUTHORITY_CASE:'duplicateId:one'}],['nonInteger:one',{HISTORY_AUTHORITY_CASE:'nonInteger:one'}],['anchors object:one',{HISTORY_AUTHORITY_CASE:'valid:one',HISTORY_BACKWARD_ENVELOPE:'object'}],['stale missing',{HISTORY_STALE_SNAPSHOT:'1',HISTORY_STALE_INVALID:'missing'}],['stale unknown',{HISTORY_STALE_SNAPSHOT:'1',HISTORY_STALE_INVALID:'unknown'}],['stale paged',{HISTORY_STALE_SNAPSHOT:'1',HISTORY_STALE_INVALID:'paged'}]])test('ambiguous provider history still starts and follows the newest in-progress turn: '+name,{timeout:20000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_FRAME_LOG:'frames.jsonl'});
  const stopped=once(s.child,'exit');s.child.kill('SIGTERM');await stopped;
  const restarted=await s.launch(env,false);
  const state=await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.ready===true,15000);
  assert.notEqual(state.runtime.state,'blocked');assert.equal(state.runtime.provider.transport,'open');
  assert.ok(state.thread.turns.filter(t=>t.status==='inProgress').length<=1);
  const frames=(await readFile(join(s.root,'cwd/frames.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(frames.filter(f=>f.method==='turn/start').length,0);
});

test('Rust refuses a FIFO journal promptly before provider startup',{timeout:10000},async t=>{
  const s=await fixture(t);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const path=join(s.state,'rust-spike.json');
  await unlink(path);
  await execFileAsync('/usr/bin/mkfifo',[path]);
  const providerBefore=await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null);
  const env=hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(launcherRoot,'public'),CODEX_BIN:join(launcherRoot,'tests/fake-codex.mjs'),RUST_SPIKE_PROVIDER_LOG:join(s.root,'provider.jsonl')});
  await assert.rejects(execFileAsync(binary,[],{cwd:join(s.root,'cwd'),env,timeout:2000,killSignal:'SIGKILL'}),error=>{
    assert.equal(error.killed,false,'FIFO must be refused without the watchdog killing startup');
    assert.match(error.stderr,/private_state_invalid/);
    return true;
  });
  assert.equal((await lstat(path)).isFIFO(),true);
  assert.equal(await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null),providerBefore);
});
test('Rust goal edits persist native state and deduplicate model-context delivery across restart',async t=>{
  // Use the existing persisted-turn fixture so new turn identities survive
  // provider restart. The relay-recovery fixture resets relay-1 on each process
  // and is intentionally unsuitable for terminal identity reconciliation.
  const s=await fixture(t,{FAKE_CODEX_GOAL_ACTIVE:'1',FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_IMAGES:'1'});
  const update=async (base,body,method='PUT')=>{
    const response=await fetch(base+'/api/goal',{method,body:JSON.stringify(body)});
    assert.equal(response.status,200,await response.clone().text());
    return response.json();
  };
  const input={objective:'Ship Rust parity',tokenBudget:5000,operationId:'goal-one'};
  const first=await update(s.base,input);
  assert.equal(first.goal.objective,input.objective);
  assert.equal(first.goal.tokenBudget,5000);
  assert.equal(first.goalDelivery.status,'sent');
  await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
  const id=first.goalDelivery.promptId;
  assert.equal((await update(s.base,input)).goalDelivery.promptId,id);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});
  assert.equal((await update(restarted.base,input)).goalDelivery.promptId,id);
  assert.equal((await update(restarted.base,{status:'paused',operationId:'goal-pause'})).goal.status,'paused');
  await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
  const unlimited=await update(restarted.base,{status:'active',tokenBudget:null,operationId:'goal-resume'});
  assert.equal(unlimited.goal.tokenBudget,null);
  await until(()=>fetch(restarted.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
  const cleared=await update(restarted.base,{operationId:'goal-clear'},'DELETE');
  assert.equal(cleared.goal.status,'cleared');
  for(const body of [{objective:' '},{tokenBudget:-1},{roundBudget:10001},{status:'done'}])
    assert.equal((await fetch(restarted.base+'/api/goal',{method:'PUT',body:JSON.stringify(body)})).status,400);
  const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(saved.records.filter(r=>r.operation==='goal-edit/goal-one/0').length,1);
});
test('Rust uncertain native goal update is observed on restart without repeating the mutation',async t=>{
  const s=await fixture(t,faultEnv('goal-ack-lost'));
  const response=await fetch(s.base+'/api/goal',{method:'PUT',body:JSON.stringify({objective:'Keep the exact goal',tokenBudget:5000,operationId:'goal-uncertain'})});
  assert.equal(response.status,400);
  let saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(saved.goals.edits[0].applying,true);
  assert.equal(saved.goals.edits[0].applied,false);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});
  assert.equal((await fetch(restarted.base+'/api/goal/reconcile',{method:'POST',body:'{}'})).status,200);
  saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(saved.goals.edits[0].sent,true);
  const calls=(await readFile(join(s.root,'provider.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.filter(c=>c.method==='thread/goal/set').length,1);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,1);
});
test('Rust mismatching goal reconciliation refuses a stale native mutation',async t=>{
  const s=await fixture(t,faultEnv('goal-ack-lost'));
  assert.equal((await fetch(s.base+'/api/goal',{method:'PUT',body:JSON.stringify({objective:'Original objective',operationId:'goal-stale'})})).status,400);
  await writeFile(join(s.root,'cwd','.fault-goal.json'),JSON.stringify({objective:'Changed elsewhere',status:'active'}));
  assert.equal((await fetch(s.base+'/api/goal/reconcile',{method:'POST',body:'{}'})).status,400);
  const calls=await providerCalls(s);
  assert.equal(calls.filter(c=>c.method==='thread/goal/set').length,1);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,0);
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.goal.objective,'Changed elsewhere');
  assert.equal(state.goalDelivery.status,'failed');
});
const imageGif=Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==','base64');
test('Rust image transfers retain two slots under backpressure and expire at the total duration cap',{timeout:30000},async t=>{
  const s=await fixture(t,{AGENT_UI_DOWNLOAD_MAX_MS:'5000',AGENT_UI_DOWNLOAD_IDLE_MS:'15000'});
  const block=Buffer.alloc(256,120);block[0]=255;
  const image=Buffer.concat([imageGif.subarray(0,-1),Buffer.from([0x21,0xfe]),...Array(32768).fill(block),Buffer.from([0,0x3b])]);
  const upload=await fetch(s.base+'/api/attachments',{method:'POST',body:image});assert.equal(upload.status,201,await upload.clone().text());
  const {attachment}=await upload.json();const url=s.base+'/api/attachments/'+attachment.id;
  const transfers=[];t.after(()=>transfers.forEach(({request,response})=>{response.destroy();request.destroy()}));
  const stalled=()=>new Promise((resolve,reject)=>{
    const request=httpGet(url,response=>{response.pause();response.on('error',()=>{});resolve({request,response});});request.on('error',reject);
  });
  transfers.push(await stalled(),await stalled());
  assert.ok(transfers.every(({response})=>response.statusCode===200));
  assert.equal((await fetch(url)).status,429,'two backpressured image buffers must retain their slots');
  await new Promise(r=>setTimeout(r,5300));
  for(const {response} of transfers)response.resume();
  await until(()=>fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}),response=>response.status===201);
  assert.ok(transfers.some(({response})=>!response.complete),'expired transfer must not claim complete bytes');
});
test('Rust images are validated, quota admitted, retained in history and durable across restart',async t=>{
  const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_IMAGES:'1'});
  for(const bad of [Buffer.from('MZfake'),imageGif.subarray(0,-1),Buffer.concat([imageGif,Buffer.from('MZ')])]) {
    const r=await fetch(s.base+'/api/attachments',{method:'POST',body:bad});
    assert.equal(r.status,400);assert.equal((await r.json()).error,'attachment_invalid_image');
  }
  const upload=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});
  assert.equal(upload.status,201,await upload.clone().text());
  const {attachment}=await upload.json();
  assert.deepEqual(Object.keys(attachment).sort(),['id','mime','size']);
  assert.match(attachment.id,/^img_[a-f0-9]{32}$/);
  const read=await fetch(s.base+'/api/attachments/'+attachment.id);
  assert.equal(read.headers.get('content-type'),'image/gif');assert.equal(read.headers.get('x-content-type-options'),'nosniff');
  assert.deepEqual(Buffer.from(await read.arrayBuffer()),imageGif);
  const accepted=await s.post({text:'',attachment_ids:[attachment.id],operation_id:'image-only'}).then(r=>r.json());
  assert.equal(accepted.accepted,true,JSON.stringify(accepted));
  const check=async base=>{
    const state=await until(()=>fetch(base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
    const item=state.thread.turns.flatMap(t=>t.items).find(i=>i.submissionId===accepted.submission.id);
    assert.equal(item.text,'');assert.deepEqual(item.attachments,[attachment]);
    assert.equal(JSON.stringify(state).includes(s.state),false);
  };
  await check(s.base);
  assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id,{method:'DELETE'})).status,409);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});await check(restarted.base);
  assert.deepEqual(Buffer.from(await (await fetch(restarted.base+'/api/attachments/'+attachment.id)).arrayBuffer()),imageGif);
  assert.ok(s.controller.events.some(e=>e.path.endsWith('/commit')));assert.equal(s.controller.reservations.size,0);
});
test('Rust failed image quota commit rolls back exact data and releases reservation',async t=>{
  const s=await fixture(t,{}, {commitError:'private_state_reservation_expired'});
  const r=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});
  assert.equal(r.status,503);
  assert.equal(s.controller.reservations.size,0);
  assert.ok(s.controller.events.some(e=>e.method==='DELETE'));
  assert.equal((await readdir(join(s.state,'rust-images'))).some(n=>n.startsWith('img_')),false);
});
test('Rust image reads refuse replaced symlinks and preserve the outside sentinel',async t=>{
  const s=await fixture(t);
  const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
  const sentinel=join(s.root,'outside-image');await writeFile(sentinel,'sentinel',{mode:0o600});
  const path=join(s.state,'rust-images',attachment.id+'.gif');await unlink(path);await symlink(sentinel,path);
  const r=await fetch(s.base+'/api/attachments/'+attachment.id);assert.equal(r.status,503);
  assert.equal(await readFile(sentinel,'utf8'),'sentinel');
  assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id,{method:'DELETE'})).status,503);
  assert.equal(await readFile(sentinel,'utf8'),'sentinel');
});
test('Rust retained images stop admission at byte and entry bounds',async t=>{
  for(const extra of [{RUST_SPIKE_IMAGE_QUOTA_BYTES:String(imageGif.length)},{RUST_SPIKE_IMAGE_ENTRY_LIMIT:'1'}]) {
    const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_IMAGES:'1',...extra});
    const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
    await s.post({text:'retain',attachment_ids:[attachment.id],operation_id:'retained-bound'});
    const r=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});assert.equal(r.status,413);
    assert.equal((await r.json()).error,'attachment_quota_exceeded');
    assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id)).status,200);
  }
});
test('Rust quota timeout releases the image store for reads and deletes',{timeout:12000},async t=>{
  const s=await fixture(t);
  const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
  s.controller.options.hangCommit=true;
  const stalled=fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});
  await until(async()=>s.controller.events.filter(e=>e.path.endsWith('/commit')).length,n=>n===2);
  assert.equal((await fetch(s.base+'/api/health')).status,200);
  const read=fetch(s.base+'/api/attachments/'+attachment.id);
  assert.equal((await stalled).status,503);
  assert.equal((await read).status,200);
  assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id,{method:'DELETE'})).status,200);
  assert.equal(s.controller.reservations.size,0);
});
test('Rust missing expired drafts do not block a new upload',async t=>{
  const s=await fixture(t);
  const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const path=join(s.state,'rust-images','rust-spike.json');
  const saved=JSON.parse(await readFile(path,'utf8'));saved.entries[0].created_ms=Date.now()-3_600_001;
  await writeFile(path,JSON.stringify(saved),{mode:0o600});
  await unlink(join(s.state,'rust-images',attachment.id+'.gif'));
  const restarted=await s.launch({});
  assert.equal((await fetch(restarted.base+'/api/attachments',{method:'POST',body:imageGif})).status,201);
  assert.match(s.logs(),/missing expired image entry removed/);
});
test('concurrent queued submissions are FIFO, queue cancellation differs from abort',async t=>{
  const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_LIVE:'1',FAKE_CODEX_STATUS:'active'});
  const a=await s.post({text:'a',operation_id:'a'}).then(r=>r.json());
  const b=await s.post({text:'b',operation_id:'b'}).then(r=>r.json());
  assert.equal(a.submission.disposition,'queued');assert.equal(b.submission.position,2);
  const canceled=await fetch(s.base+'/v1/queue/'+controllerSegment(a.submission.id),{method:'DELETE',headers:s.headers}).then(r=>r.json());
  assert.equal(canceled.submission.state,'interrupted');
  const running=await fetch(s.base+'/v1/status').then(r=>r.json());assert.equal(running.busy,true);
  assert.equal((await fetch(s.base+'/v1/abort',{method:'POST',headers:s.headers,body:'{}'})).status,202);
});

test('Rust journal restart never replays an unresolved turn, preserves queued identity and keeps dispatching',async t=>{
  const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:''});
  const first=await s.post({text:'hold',operation_id:'hold'}).then(r=>r.json());
  const queued=await s.post({text:'later',operation_id:'later'}).then(r=>r.json());
  assert.equal(queued.submission.disposition,'queued');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const second=await s.launch({FAKE_CODEX_RELAY_RECOVERY:'1'});
  const replay=await fetch(second.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'later',operation_id:'later'})}).then(r=>r.json());
  assert.equal(replay.duplicate,true);assert.equal(replay.submission.id,queued.submission.id);
  const old=await fetch(second.base+'/v1/submissions/'+first.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(old.submission.state,'failed');assert.match(old.submission.error,/not replayed/);
  // The queue is not paused: the next prompt dispatches.
  await until(()=>fetch(second.base+'/v1/submissions/'+queued.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission.state!=='queued');
  const starts=(await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text);
  assert.deepEqual(starts,['hold','later'],'the uncertain turn is never resent');
});

const faultEnv=mode=>({CODEX_BIN:process.execPath,RUST_SPIKE_CODEX_ARGS:JSON.stringify([resolve(import.meta.dirname,'fixtures/fault-codex.mjs')]),RUST_SPIKE_FAULT:mode});
const providerCalls=async s=>(await readFile(join(s.root,'provider.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
for(const mode of ['model-list-timeout','model-list-late','model-list-burst','model-list-refused'])test('Rust optional model discovery startup and resume remain usable '+mode,{timeout:35000},async t=>{
  const s=await fixture(t,faultEnv(mode));
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.ready,true);
  assert.equal(state.runtime.provider.transport,'open');
  assert.equal(state.runtime.state,'idle');
  assert.deepEqual(state.modelOptions,mode==='model-list-burst'?[{id:'synthetic-model',model:'synthetic-model',displayName:'Synthetic model'}]:[]);
  const calls=await providerCalls(s);
  assert.ok(calls.some(call=>call.method==='model/list'));
  assert.ok(calls.some(call=>call.method==='thread/start'));
  if(mode==='model-list-burst')assert.doesNotMatch(s.logs(),/optional model\/list unavailable/,'event backpressure must be pumped before response deadline');
  else assert.match(s.logs(),/optional model\/list unavailable; continuing startup/);
  const first=await s.post({text:'after discovery startup',operation_id:'discovery-startup'}).then(r=>r.json());
  await until(()=>fetch(s.base+'/v1/submissions/'+first.submission.id,{headers:s.headers}).then(r=>r.json()),r=>r.submission?.state==='done');
  if(mode==='model-list-late') {
    await new Promise(r=>setTimeout(r,1800));
    const late=await fetch(s.base+'/api/state').then(r=>r.json());
    assert.deepEqual(late.modelOptions,[]);assert.equal(late.runtime.provider.transport,'open');
  }
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
  const resumed=await s.launch({AGENT_UI_SESSION_ID:'fault-thread'});
  const resumeState=await fetch(resumed.base+'/api/state').then(r=>r.json());
  assert.equal(resumeState.thread.id,'fault-thread');assert.equal(resumeState.runtime.state,'idle');
  assert.equal(resumeState.runtime.provider.transport,'open');
  const second=await fetch(resumed.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'after discovery resume',operation_id:'discovery-resume'})}).then(r=>r.json());
  await until(()=>fetch(resumed.base+'/v1/submissions/'+second.submission.id,{headers:s.headers}).then(r=>r.json()),r=>r.submission?.state==='done');
  assert.ok((await providerCalls(s)).some(c=>c.method==='thread/resume'));
  assert.deepEqual((await providerCalls(s)).filter(c=>c.method==='turn/start').map(c=>c.params.input[0].text),['after discovery startup','after discovery resume']);
});
const incomingNotice=(n,delivery='dlv_'+n)=>({delivery_id:delivery,recipient:{kind:'agent',id:'ag_contract'},message:{message_id:'msg_'+n,sender:{kind:'agent',id:'sender_'+n,name:'Sender '+n,path:'/team/sender-'+n},body:'Synthetic notice '+n,created_at:'2026-10-21T00:00:00Z'}});
const messageFixtureEnv={AGENT_UI_MESSAGE_WATCH:'1',RUST_SPIKE_MESSAGE_INPUT:'incoming.json',PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH,CODEX_BIN:resolve(import.meta.dirname,'./fake-codex.mjs'),FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_COMPLETE_TURNS:'1'};
const receiptFixtureEnv={CODEX_BIN:resolve(import.meta.dirname,'./fake-codex.mjs'),FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_COMPLETE_TURNS:'1'};
async function seededReceipts(t,count,edit=()=>{}) {
  const s=await fixture(t,receiptFixtureEnv);
  await s.post({text:'synthetic seed',operation_id:'seed'});
  await until(()=>messageJournal(s),j=>j.records[0]?.submission.state==='done');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const saved=await messageJournal(s),template=saved.records[0];
  saved.records=Array.from({length:count},(_,n)=>{
    const r=structuredClone(template),id=randomUUID();r.operation='retained-'+n;
    r.submission.id=id;r.receipt.id=id;r.submission.turn_id='synthetic-old-'+n;
    edit(r,n);return r;
  });
  await writeFile(join(s.state,'rust-spike.json'),JSON.stringify(saved));
  return {s,saved};
}
test('Rust receipt pruning recovers full4096 journal, preserves recent retries and admits a new real turn',{timeout:15000},async t=>{
  const {s,saved}=await seededReceipts(t,4096);
  const restarted=await s.launch({});
  const journal=await messageJournal(s);assert.equal(journal.records.length,1024);
  assert.equal(journal.records[0].operation,'retained-3072');assert.equal(journal.records.at(-1).operation,'retained-4095');
  const post=body=>fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify(body)});
  const duplicate=await post({text:'synthetic seed',operation_id:'retained-4095'}).then(r=>r.json());
  assert.equal(duplicate.duplicate,true);assert.equal(duplicate.submission.id,saved.records[4095].submission.id);
  assert.equal((await fetch(restarted.base+'/v1/submissions/'+saved.records[0].submission.id,{headers:s.headers})).status,404,'evicted receipt is outside the declared dedup window');
  assert.equal((await post({text:'new after lifetime4096',operation_id:'after4096'})).status,202);
  await until(()=>messageJournal(s),j=>j.records.some(r=>r.operation==='after4096'&&r.submission.state==='done'));
  assert.equal((await messageJournal(s)).records.length,1024);
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,2);
});
test('Rust receipt pressure protects256 terminal pending ACKs and refuses new admission before dispatch',{timeout:15000},async t=>{
  const {s}=await seededReceipts(t,4096,(r,n)=>{if(n<256)r.source_messages=[{messageId:'msg_'+n,deliveryId:'dlv_'+n,ackPending:true}];});
  const restarted=await s.launch({}),j=await messageJournal(s);
  assert.equal(j.records.length,1280);assert.equal(j.records.filter(r=>r.source_messages?.some(m=>m.ackPending)).length,256);
  const response=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'must not dispatch',operation_id:'pressure-refused'})});
  assert.notEqual(response.status,202);assert.equal((await response.json()).error.code,'operation_refused');
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,1);
  assert.equal((await messageJournal(s)).records.some(r=>r.operation==='pressure-refused'),false);
});
test('Rust receipt pruning failed completion publication restores the evicted receipt and durable running intent',{timeout:15000},async t=>{
  const {s,saved}=await seededReceipts(t,1024);
  const restarted=await s.launch({RUST_SPIKE_CHECKPOINT_FAIL:'completion-prune'});
  await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'completion fails',operation_id:'prune-fault'})});
  assert.notEqual((await once(restarted.child,'exit'))[0],0,'a failed checkpoint exits for restart');
  const j=await messageJournal(s);assert.equal(j.records.length,1025);
  assert.equal(j.records[0].submission.id,saved.records[0].submission.id);
  assert.equal(j.records.find(r=>r.operation==='prune-fault').submission.state,'running');
});
test('Rust explicit prompt limit uses raw UTF8 bytes while retained oversized operations stay retryable',{timeout:15000},async t=>{
  const {s,saved}=await seededReceipts(t,1,r=>{
    r.text='x'.repeat(70_000);r.fingerprint=JSON.stringify([r.text,'queue',null,null]);
  });
  const restarted=await s.launch({});
  const post=body=>fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify(body)});
  const retry=await post({text:'x'.repeat(70_000),operation_id:'retained-0'}).then(r=>r.json());
  assert.equal(retry.duplicate,true);assert.equal(retry.submission.id,saved.records[0].submission.id);
  for(const text of ['界'.repeat(21846),'x'.repeat(65537),' '.repeat(65536)+'x']) {
    const response=await post({text,operation_id:'oversized-'+Buffer.byteLength(text)});
    assert.notEqual(response.status,202);assert.equal((await response.json()).error.code,'operation_refused');
  }
  assert.equal((await post({text:'界'.repeat(21845)+'x',operation_id:'exact65536'})).status,202);
});
const messageJournal=s=>readFile(join(s.state,'rust-spike.json'),'utf8').then(JSON.parse);
const messageAcks=async s=>(await readFile(join(s.root,'registration.json'),'utf8')).trim().split('\n').map(JSON.parse).filter(x=>x.ack);
const messageAttempts=async s=>(await readFile(join(s.root,'registration.json'),'utf8')).trim().split('\n').map(JSON.parse).filter(x=>x.messageAttempt);
test('Rust message watcher survives 403 auth failures, reports them and recovers without content disclosure',{timeout:20000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,RUST_SPIKE_MESSAGE_FAILURE:'failure.json'});
  await writeFile(join(s.root,'cwd/failure.json'),JSON.stringify({operation:'receive',kind:'auth',remaining:2,stdout:true}));
  const retrying=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.messages?.state==='retrying'&&x.runtime.messages.failures>=1);
  assert.match(retrying.runtime.messages.lastFailure,/message_auth/);
  assert.equal(retrying.ready,true);
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(301)]));
  await until(()=>messageAcks(s),a=>a.some(m=>m.ack==='dlv_301'),15000);
  assert.ok((await messageAttempts(s)).length>=2,'auth failures are retried');
  assert.equal((await fetch(s.base+'/api/state').then(r=>r.json())).runtime.messages.state,'receiving');
  assert.ok(!s.logs().includes('private-diagnostic-canary'));
});
test('Rust message retry auth ACK keeps durable pending IDs and retries them without turn replay',{timeout:25000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,RUST_SPIKE_MESSAGE_FAILURE:'failure.json'});
  await writeFile(join(s.root,'cwd/failure.json'),JSON.stringify({operation:'ack',kind:'auth',remaining:2,flood:true}));
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(301),incomingNotice(302)]));
  const before=await until(()=>messageJournal(s),j=>j.records[0]?.source_messages.some(m=>m.ackPending));
  const id=before.records[0].submission.id;
  await writeFile(join(s.root,'cwd/incoming.json'),'[]');
  const after=await until(()=>messageJournal(s),j=>j.records[0].source_messages.every(m=>!m.ackPending),20000);
  assert.equal(after.records[0].submission.id,id);
  assert.ok(!s.logs().includes('private-diagnostic-canary'));
  await until(()=>messageJournal(s),j=>j.records[0].submission.state==='done');
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,1);
});
test('Rust JSON-RPC refusal fails only that prompt and the queue keeps going',{timeout:15000},async t=>{
  const s=await fixture(t,{...receiptFixtureEnv,FAKE_CODEX_REFUSE_TEXT:'refuse me'});
  const refused=await s.post({text:'refuse me',operation_id:'rpc-refused'}).then(r=>r.json());
  const next=await s.post({text:'run after the refusal',operation_id:'rpc-next'}).then(r=>r.json());
  const get=id=>fetch(s.base+'/v1/submissions/'+id,{headers:s.headers}).then(r=>r.json());
  const failed=await until(()=>get(refused.submission.id),x=>x.submission?.state==='failed');
  assert.match(failed.submission.error,/refused/);
  await until(()=>get(next.submission.id),x=>x.submission?.state==='done');
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.notEqual(state.runtime.state,'blocked');assert.equal(state.runtime.provider.transport,'open');
  assert.equal(state.runtime.recovery.restarts,0);
});
test('Rust message retry network backoff grows and resets after a successful poll',{timeout:15000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,RUST_SPIKE_MESSAGE_FAILURE:'failure.json'});
  const path=join(s.root,'cwd/failure.json');
  await writeFile(path,JSON.stringify({operation:'receive',kind:'network',remaining:2,flood:true}));
  await until(async()=>s.logs(),log=>log.includes('failures=1; delay_ms='));
  let matches=[...s.logs().matchAll(/message_network.*failures=(\d+); delay_ms=(\d+)/g)];
  assert.deepEqual(matches.map(m=>Number(m[1])),[0,1]);
  assert.ok(Number(matches[0][2])>=1000&&Number(matches[0][2])<=1200);
  assert.ok(Number(matches[1][2])>=2000&&Number(matches[1][2])<=2400);
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(303)]));
  await until(()=>messageAcks(s),a=>a.some(m=>m.ack==='dlv_303'));
  await writeFile(path,JSON.stringify({operation:'receive',kind:'network',remaining:1}));
  await until(async()=>s.logs(),log=>[...log.matchAll(/message_network.*failures=(\d+); delay_ms=(\d+)/g)].length>=3);
  matches=[...s.logs().matchAll(/message_network.*failures=(\d+); delay_ms=(\d+)/g)];
  assert.equal(matches[2][1],'0');
  assert.ok(!s.logs().includes('private-diagnostic-canary'));
});
test('Rust message retry oversized stdout kills the owned command without waiting for stderr EOF',{timeout:15000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,RUST_SPIKE_MESSAGE_FAILURE:'failure.json'});
  await writeFile(join(s.root,'cwd/failure.json'),JSON.stringify({operation:'receive',kind:'other',remaining:1,stdout:true,oversized:true}));
  await until(async()=>s.logs(),log=>log.includes('message_other: operation failed; failures=0'));
  assert.equal((await messageJournal(s)).records.length,0);
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(304)]));
  await until(()=>messageAcks(s),a=>a.some(m=>m.ack==='dlv_304'));
});
test('Rust message batching same poll is one real turn, durable before ack and redelivery deduplicated',{timeout:20000},async t=>{
  const s=await fixture(t,messageFixtureEnv);
  const notices=[incomingNotice(1),incomingNotice(2),incomingNotice(3)];
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify(notices));
  const journal=await until(()=>messageJournal(s),j=>j.records.some(r=>r.source_messages?.length===3));
  const batch=journal.records.find(r=>r.source_messages?.length===3);
  assert.equal(batch.operation,'treer-message/msg_1');
  assert.deepEqual(batch.source_messages.map(m=>[m.messageId,m.deliveryId,m.sender.id]),notices.map(d=>[d.message.message_id,d.delivery_id,d.message.sender.id]));
  await until(()=>messageAcks(s),a=>a.length>=3);
  for(const a of await messageAcks(s)){assert.equal(a.operation,'launcher-message-ack/'+a.ack);assert.equal(a.durable,true);}
  await until(()=>messageJournal(s),j=>j.records.some(r=>r.operation===batch.operation&&r.submission.state==='done'));
  const calls=await providerCalls(s),starts=calls.filter(x=>x.method==='turn/start');
  assert.equal(starts.length,1);const text=starts[0].params.input[0].text;
  for(const d of notices){assert.ok(text.includes(d.message.message_id));assert.ok(text.includes(d.message.sender.path));}
  assert.ok(text.includes('[3/3]'));
  // The same immutable message gets another delivery ID, not another prompt.
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(1,'dlv_new')]));
  await until(()=>messageAcks(s),a=>a.some(x=>x.ack==='dlv_new'));
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,1);
  const final=await messageJournal(s);assert.equal(final.records.length,1);assert.equal(final.records[0].source_messages.length,4);
});
test('Rust message batching busy bursts merge queued tail but never merge an explicit user prompt',{timeout:20000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,FAKE_CODEX_COMPLETE_TURNS:''});
  await s.post({text:'explicit running prompt',operation_id:'busy-user'});
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(1),incomingNotice(2)]));
  await until(()=>messageAcks(s),a=>a.some(x=>x.ack==='dlv_2'));
  const initial=(await messageJournal(s)).records[1];
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(3),incomingNotice(4)]));
  await until(()=>messageAcks(s),a=>a.some(x=>x.ack==='dlv_4'));
  let j=await messageJournal(s);assert.equal(j.records.length,2);assert.equal(j.records[1].source_messages.length,4);
  const retry=await s.post({text:initial.text,operation_id:initial.operation}).then(r=>r.json());
  assert.equal(retry.duplicate,true);assert.equal(retry.submission.id,initial.submission.id);
  const receipt=await s.post({text:'explicit queued prompt',operation_id:'user-barrier'}).then(r=>r.json());
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(5)]));
  await until(()=>messageAcks(s),a=>a.some(x=>x.ack==='dlv_5'));
  j=await messageJournal(s);assert.equal(j.records.length,4);assert.equal(j.records[2].submission.id,receipt.submission.id);
  assert.equal(j.records[2].message_batch,undefined);assert.equal(j.records[3].source_messages.length,1);
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,1);
});
test('Rust message batching restart retries remote-success ack without replaying uncertain dispatch',{timeout:20000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,RUST_SPIKE_MESSAGE_ACK_GATE:'ack-release',FAKE_CODEX_COMPLETE_TURNS:''});
  await s.post({text:'explicit running prompt',operation_id:'busy-user'});
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(1),incomingNotice(2)]));
  await until(()=>messageAcks(s),a=>a.some(x=>x.ack==='dlv_1'));
  let j=await messageJournal(s);assert.equal(j.records[1].source_messages.every(m=>m.ackPending),true);
  const id=j.records[1].submission.id, text=j.records[1].text;
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  await writeFile(join(s.root,'cwd/ack-release'),'');
  await writeFile(join(s.root,'cwd/incoming.json'),'[]');
  const restarted=await s.launch({});
  await until(()=>messageJournal(s),j=>j.records[1].source_messages.every(m=>!m.ackPending));
  const acks=await messageAcks(s);assert.equal(acks.filter(x=>x.ack==='dlv_1').length,2);assert.equal(acks.filter(x=>x.ack==='dlv_2').length,1);
  j=await messageJournal(s);assert.equal(j.records[1].submission.id,id);assert.equal(j.records[1].text,text);
  assert.notEqual((await fetch(restarted.base+'/api/state').then(r=>r.json())).runtime.state,'blocked');
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start'&&x.params.input[0].text==='explicit running prompt').length,1,'the running prompt is never resent');
});
test('Rust message batching failed durable publication does not acknowledge or dispatch the poll',{timeout:15000},async t=>{
  const s=await fixture(t,messageFixtureEnv);
  const journal=join(s.state,'rust-spike.json'),backup=join(s.state,'saved-journal');
  const before=await readFile(journal,'utf8');await rename(journal,backup);await mkdir(journal);
  await writeFile(join(s.root,'cwd/incoming.json'),JSON.stringify([incomingNotice(1),incomingNotice(2)]));
  assert.notEqual((await once(s.child,'exit'))[0],0,'unusable storage exits for restart');
  let acks=[];try{acks=await messageAcks(s);}catch{}assert.equal(acks.length,0);
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,0);
  assert.equal(await readFile(backup,'utf8'),before);
});
test('Rust message batching admits a bounded multi-megabyte poll and splits previews without lost IDs',{timeout:15000},async t=>{
  const s=await fixture(t,{...messageFixtureEnv,FAKE_CODEX_COMPLETE_TURNS:''});
  await s.post({text:'explicit running prompt',operation_id:'busy-user'});
  const notices=Array.from({length:10},(_,n)=>({...incomingNotice(n),message:{...incomingNotice(n).message,body:'x'.repeat(210_000)}}));
  const bytes=JSON.stringify(notices);assert.ok(Buffer.byteLength(bytes)>2_000_000);
  await writeFile(join(s.root,'cwd/incoming.json'),bytes);
  await until(()=>messageAcks(s),a=>a.filter(x=>x.ack).length===10);
  const journal=await messageJournal(s),batches=journal.records.filter(r=>r.message_batch);
  assert.equal(batches.length,10);assert.equal(batches.flatMap(r=>r.source_messages).length,10);
  for(const r of batches){assert.ok(Buffer.byteLength(r.text)<=65536);assert.ok(r.text.includes('Long message: read the original'));}
  assert.deepEqual(batches.flatMap(r=>r.source_messages).map(m=>m.messageId).sort(),notices.map(n=>n.message.message_id).sort());
  assert.equal((await providerCalls(s)).filter(x=>x.method==='turn/start').length,1);
});
for(const kind of ['file','tool'])for(const change of ['empty','extend','shrink','conflict',...(kind==='tool'?['name','argument','duration']:['path'])])for(const observer of [false,true])test(`Rust terminal ${kind} detail survives summary or yields to the final snapshot on ${change} (${observer?'observer':'event'})`,{timeout:20000},async t=>{
  const s=await fixture(t,faultEnv(`final-${observer?'observer-':''}${kind}-${change}`));
  const receipt=await s.post({text:'synthetic tool detail',operation_id:'typed-final'}).then(r=>r.json());
  const read=()=>fetch(s.base+'/api/state').then(r=>r.json());
  const live=await until(read,x=>x.thread.turns.some(t=>t.items.some(i=>i.id==='synthetic-command'&&i.status==='completed')));
  const old=live.thread.turns.flatMap(t=>t.items).find(i=>i.id==='synthetic-command');
  // A final snapshot that contradicts streamed detail no longer stops the
  // Agent: the provider's final snapshot wins and the receipt settles.
  const conflict=['shrink','conflict','name','argument','duration','path'].includes(change);
  const state=await until(read,x=>x.thread.turns.some(t=>t.status==='completed')&&x.runtime.state==='idle');
  const tools=state.thread.turns.flatMap(t=>t.items).filter(i=>i.id==='synthetic-command');
  assert.equal(tools.length,1);const item=tools[0];assert.equal(item.status,'completed');
  const result=await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),r=>r.submission.state==='done');
  assert.equal(result.submission.state,'done');assert.equal(state.runtime.provider.transport,'open');
  if(!conflict) {
    const detail=kind==='file'?item.changes:JSON.parse(item.detailText).contentItems;
    const prior=kind==='file'?old.changes:JSON.parse(old.detailText).contentItems;
    assert.deepEqual(detail.slice(0,2),prior);assert.equal(detail.length,change==='extend'?3:2);
  }
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
});
for(const mode of ['event','observer','other-turn','terminal','terminal-error',...['full','empty','forward','late-user','kind','order','item-status','output','duplicate','empty-id','missing-id','id-type','id-mismatch','bound'].flatMap(m=>[m,'observer-'+m])])test('Rust final snapshot retains observed tool or is replaced without stopping: '+mode,{timeout:25000},async t=>{
  const scenario=mode.replace(/^observer-/,''),observer=mode==='observer'||mode.startsWith('observer-');
  const s=await fixture(t,faultEnv('final-'+mode));
  const receipt=await s.post({text:'synthetic read-only observation',operation_id:'final-retention'}).then(r=>r.json());
  const read=()=>fetch(s.base+'/api/state').then(r=>r.json());
  // Conflicting or malformed final snapshots no longer stop the Agent.
  const conflict=!['event','observer','full','empty','forward','late-user'].includes(scenario);
  const live=conflict?null:await until(read,x=>x.thread.turns.some(t=>t.items.some(i=>i.id==='synthetic-command'&&i.status==='completed')));
  const turnId=live?.thread.turns.find(t=>t.items.some(i=>i.id==='synthetic-command')).id;
  const submission=await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),r=>r.submission.state!=='running'||['bound','other-turn'].includes(scenario),20000);
  let state=await read();
  assert.notEqual(state.runtime.state,'blocked');
  if(!conflict) {
    state=await until(read,x=>x.thread.turns.some(t=>t.id===turnId&&t.status==='completed'));
    await new Promise(r=>setTimeout(r,350)); // observe duplicate final event too
    state=await read();
    const turn=state.thread.turns.find(t=>t.id===turnId);
    const tools=turn.items.filter(i=>i.id==='synthetic-command');
    assert.equal(tools.length,1);assert.equal(tools[0].kind,'commandExecution');assert.equal(tools[0].status,'completed');
    assert.equal(tools[0].detailText,'synthetic bytes=42 sha256=fixture-only'+(scenario==='forward'?' appended':''));
    assert.equal(state.runtime.state,'idle');assert.equal(state.runtime.provider.transport,'open');
    assert.equal(submission.submission.state,'done');
    assert.deepEqual(turn.items.map(i=>i.kind),scenario==='empty'?['userMessage','commandExecution']:['userMessage','commandExecution','agentMessage']);
    assert.deepEqual(turn.items.map(i=>i.transcriptOrder),scenario==='empty'?[0,1]:[0,1,2]);
    assert.equal(turn.items[0].submissionId,receipt.submission.id);
    const transcript=await fetch(s.base+'/v1/transcript',{headers:s.headers}).then(r=>r.json());
    const entries=transcript.entries.filter(i=>i.id==='synthetic-command');
    assert.equal(entries.length,1);assert.equal(entries[0].kind,'commandExecution');
    assert.equal(entries[0].content,'synthetic public-file checksum'); // compact AIS omits detailText by contract
    if(observer)assert.ok((await providerCalls(s)).some(c=>c.method==='thread/read'));
    assert.equal(state.runtime.provider.finalProjection.refusals,0);
  }
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
});
test('Rust observes an explicit terminal accepted turn without a completion event',async t=>{
  const s=await fixture(t,faultEnv('observe'));
  const receipt=await s.post({text:'observe only',operation_id:'observe'}).then(r=>r.json());
  const result=await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done');
  assert.equal(result.submission.turn_id,'fault-1');
  const calls=await providerCalls(s);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,1);
  assert.ok(calls.some(c=>c.method==='thread/read'));
});
test('Rust sparse completion keeps accepted user input alongside the assistant',async t=>{
  const s=await fixture(t,faultEnv('sparse'));
  await s.post({text:'keep this input',operation_id:'sparse-user'});
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
  const items=state.thread.turns.flatMap(t=>t.items);
  assert.equal(items.filter(i=>i.kind==='userMessage'&&i.text==='keep this input').length,1);
  assert.ok(items.some(i=>i.kind==='agentMessage'&&i.text==='sparse answer'));
});
test('Rust shutdown kills the provider-owned process group including a descendant',async t=>{
  const s=await fixture(t,faultEnv('descendant'));
  const pid=Number(await readFile(join(s.root,'provider.jsonl.child'),'utf8'));
  const alive=()=>{try{process.kill(pid,0);return true}catch{return false}};
  assert.equal(alive(),true);
  t.after(()=>{if(alive())try{process.kill(pid,'SIGKILL')}catch{}});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);
  await until(async()=>alive(),x=>x===false);
  assert.match(s.logs(),/shutdown provider_exit=confirmed; process_group=termination_requested_unverified; detached_descendants=not_guaranteed/);
});
test('Rust shutdown cancels startup and an in-flight turn without waiting for the RPC timeout',{timeout:25000},async t=>{
  const s=await fixture(t,faultEnv('timeout'));
  const provider=Number(await readFile(join(s.root,'provider.jsonl.pid'),'utf8'));
  const pending=s.post({text:'uncertain accepted input',operation_id:'shutdown-inflight'}).catch(()=>null);
  await until(()=>providerCalls(s),calls=>calls.some(c=>c.method==='turn/start'));
  const started=Date.now();
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');assert.equal((await exit)[0],0);await pending;
  assert.ok(Date.now()-started<8000,'shutdown must cancel the pending 60 s RPC');
  assert.throws(()=>process.kill(provider,0),{code:'ESRCH'});
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(journal.dispatchBlocked,true);
  assert.equal(journal.records[0].submission.state,'running','stop must not invent completion');
  const restarted=await s.launch(faultEnv('shutdown-startup'),false);
  const nextProvider=await until(()=>readFile(join(s.root,'provider.jsonl.pid'),'utf8').then(Number),pid=>pid!==provider);
  const stop=Date.now();const stopped=once(restarted.child,'exit');restarted.child.kill('SIGTERM');assert.equal((await stopped)[0],0);
  assert.ok(Date.now()-stop<8000);
  assert.throws(()=>process.kill(nextProvider,0),{code:'ESRCH'});
  assert.match(s.logs(),/shutdown provider_exit=confirmed/);
});
test('Rust refuses unknown provider RPC requests with method-not-found',async t=>{
  const s=await fixture(t,faultEnv('unknown-request'));
  const calls=await until(()=>providerCalls(s),calls=>calls.some(c=>c.id==='provider-unknown'));
  const response=calls.find(c=>c.id==='provider-unknown');assert.equal(response.error.code,-32601);assert.equal(response.result,undefined);
});
test('Rust checks private-state ancestor modes once, at startup',async t=>{
  const s=await fixture(t);
  await chmod(s.root,0o777);
  try {
    // Already open: no per-command recheck.
    assert.equal((await s.post({text:'dispatches normally',operation_id:'open-once'})).status,202);
    const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
    const env={...s.env};
    await assert.rejects(execFileAsync(binary,[],{cwd:join(s.root,'cwd'),env,timeout:5000}),error=>error.code===1&&/private_state_unavailable/.test(error.stderr));
  } finally {await chmod(s.root,0o700);}
});

test('Rust immutable queued receipt survives dispatch/restart while dynamic queue positions change',async t=>{
  const s=await fixture(t,faultEnv('receipt'));
  const a=await s.post({text:'a',operation_id:'a'}).then(r=>r.json());
  const body={text:'b',operation_id:'b'};
  const b=await s.post(body).then(r=>r.json());assert.equal(b.submission.position,2);
  const queue=()=>fetch(s.base+'/v1/queue',{headers:s.headers}).then(r=>r.json());
  assert.equal((await queue()).submissions.find(x=>x.id===b.submission.id).position,2);
  await fetch(s.base+'/v1/queue/'+a.submission.id,{method:'DELETE',headers:s.headers});
  assert.equal((await queue()).submissions.find(x=>x.id===b.submission.id).position,1);
  await fetch(s.base+'/v1/abort',{method:'POST',headers:s.headers,body:'{}'});
  await until(queue,x=>x.submissions.find(y=>y.id===b.submission.id)?.state==='running');
  assert.equal((await fetch(s.base+'/v1/queue/'+b.submission.id,{method:'DELETE',headers:s.headers})).status,409);
  assert.deepEqual((await s.post(body).then(r=>r.json())).submission,b.submission);
  assert.equal((await queue()).submissions.find(x=>x.id===b.submission.id).position,null);
  await fetch(s.base+'/v1/abort',{method:'POST',headers:s.headers,body:'{}'});
  await until(queue,x=>x.submissions.find(y=>y.id===b.submission.id)?.state==='interrupted');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});
  const retry=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify(body)}).then(r=>r.json());
  assert.equal(retry.duplicate,true);assert.deepEqual(retry.submission,b.submission);
});

test('Rust backpressure consumes 64 startup notifications before the first RPC response',async t=>{
  const s=await fixture(t,faultEnv('burst-startup'));
  const snapshot=await fetch(s.base+'/v1/state').then(r=>r.json());
  assert.equal(snapshot.ready,true);
  assert.equal(snapshot.runtime.provider.transport,'open');
  assert.equal(snapshot.runtime.provider.beforeCleanup,null);
});
test('Rust backpressure preserves 200 mid-turn deltas and early completion before RPC response',async t=>{
  const s=await fixture(t,faultEnv('burst-turn'));
  const accepted=await s.post({text:'synthetic burst',operation_id:'burst'}).then(r=>r.json());
  const queue=await until(()=>fetch(s.base+'/v1/queue',{headers:s.headers}).then(r=>r.json()),x=>x.submissions?.find(r=>r.id===accepted.submission.id)?.state==='done');
  const receipt=queue.submissions.find(r=>r.id===accepted.submission.id);
  assert.equal(receipt.assistant_preview,'x'.repeat(200));
  const snapshot=await fetch(s.base+'/v1/state').then(r=>r.json());
  assert.equal(snapshot.thread.turns.find(t=>t.id===receipt.turn_id).items.find(i=>i.id==='burst-item').text,'x'.repeat(200));
  assert.equal(snapshot.runtime.provider.transport,'open');
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
});
for(const mode of ['diagnostic-eof','diagnostic-invalid'])test('Rust private provider diagnostics '+mode,async t=>{
  const s=await fixture(t,{...faultEnv(mode),RUST_SPIKE_CAPTURE_STDERR:'1'});
  // The stopped provider's stderr tail is saved privately before its restart.
  const snapshot=await until(()=>fetch(s.base+'/v1/state').then(r=>r.json()),x=>x.runtime?.recovery?.restarts>=1);
  assert.match(String(snapshot.runtime.recovery.lastFailure),mode==='diagnostic-eof'?/stdout_eof/:/invalid_json/);
  const file=join(s.state,'rust-provider-stderr.log');const stat=await until(()=>lstat(file),Boolean);
  assert.equal(stat.mode&0o777,0o600);assert.ok(stat.size<=8192);
  const captured=await readFile(file,'utf8');
  if(mode==='diagnostic-invalid'){assert.ok(captured.endsWith('synthetic-private-tail'));assert.ok(!captured.includes('sensitive-fixture-prefix'));}
  assert.ok(!JSON.stringify(snapshot.runtime).includes('synthetic-secret'));
  assert.ok(!s.logs().includes('synthetic-private-tail'));assert.ok(!s.logs().includes('sensitive-fixture-prefix'));
});
test('Rust stderr retention is opt-in and a private write failure does not stop the restart',async t=>{
  const off=await fixture(t,faultEnv('diagnostic-invalid'));
  const stopped=await until(()=>fetch(off.base+'/v1/state').then(r=>r.json()),x=>x.runtime?.recovery?.restarts>=1);
  assert.equal(stopped.runtime.provider.stderrCapture,'disabled');
  await assert.rejects(lstat(join(off.state,'rust-provider-stderr.log')),{code:'ENOENT'});
  const on=await fixture(t,{...faultEnv('diagnostic-invalid'),RUST_SPIKE_CAPTURE_STDERR:'1'});
  // Fail only optional stderr publication, not the authoritative journal.
  const stderr=join(on.state,'rust-provider-stderr.log');await mkdir(stderr);
  try {
    const restarted=await until(()=>fetch(on.base+'/v1/state').then(r=>r.json()),x=>x.runtime?.recovery?.restarts>=1);
    assert.ok(!JSON.stringify(restarted.runtime).includes('synthetic-private-tail'));
    assert.equal((await lstat(stderr)).isDirectory(),true);
    const exit=once(on.child,'exit');on.child.kill('SIGTERM');assert.deepEqual(await exit,[0,null]);
  } finally { await rm(stderr,{recursive:true}); }
});
test('Rust stalled provider stdin restarts the provider while state reads remain responsive',{timeout:20000},async t=>{
  const s=await fixture(t,faultEnv('stdin-stall'));
  const pending=s.post({text:'accepted but write stalls',operation_id:'stalled'}).then(async r=>({status:r.status,body:await r.json()}));
  await until(()=>providerCalls(s),calls=>calls.some(c=>c.method==='turn/start'));
  const readStarted=Date.now();
  assert.equal((await fetch(s.base+'/api/state').then(r=>r.json())).ready,true);
  assert.ok(Date.now()-readStarted<2000,'reads do not await stdin writes');
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime?.recovery?.restarts>=1,15000);
  await pending;
  assert.match(String(state.runtime.recovery.lastFailure),/stdin_write_timeout/);
  assert.notEqual(state.runtime.state,'blocked');
});
test('Rust shutdown cancels a stalled provider reply write without duplicate dispatch',{timeout:15000},async t=>{
  const s=await fixture(t,faultEnv('stdin-stall'));
  const pending=s.post({text:'stop while replying',operation_id:'stopped'}).then(()=>null,()=>null);
  await until(()=>providerCalls(s),calls=>calls.some(c=>c.method==='turn/start'));
  await new Promise(r=>setTimeout(r,200));
  const before=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  const started=Date.now(),exit=once(s.child,'exit');s.child.kill('SIGTERM');
  assert.deepEqual(await exit,[0,null]);assert.ok(Date.now()-started<7000);
  await pending;
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
  const after=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(after.dispatchBlocked,true);
  assert.deepEqual(after.records,before.records);assert.deepEqual(after.goals,before.goals);
});
test('Rust stalled notification worker permits continuous provider deltas and prompt abort',async t=>{
  const s=await fixture(t,{...faultEnv('stream'),PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH,RUST_SPIKE_NOTIFY_BLOCK:'1'});
  await s.post({text:'notify on completion',operation_id:'notify',notify:{on:['done'],to:'ag_synthetic_recipient'}});
  const notification=await until(()=>readFile(join(s.root,'registration.json'),'utf8').then(x=>JSON.parse(x.trim())),x=>x.payload?.type==='treer.prompt.completed');
  assert.equal(notification.payload.status,'done');
  await s.post({text:'keep streaming while CLI is stalled',operation_id:'stream'});
  const read=()=>fetch(s.base+'/api/state').then(r=>r.json());
  const text=snapshot=>snapshot.thread.turns.flatMap(t=>t.items).find(i=>i.id==='stream-item')?.text||'';
  const before=await until(read,x=>text(x).length>60);
  await new Promise(resolve=>setTimeout(resolve,800));
  const after=await read();assert.equal(after.ready,true);assert.ok(text(after).length>text(before).length+60);
  const start=Date.now();
  assert.equal((await fetch(s.base+'/v1/abort',{method:'POST',headers:s.headers,body:'{}'})).status,202);
  assert.ok(Date.now()-start<2000,'abort must not await notification IO');
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/interrupt').length,1);
});

test('Rust keeps dispatching after a VCS marker appears above private state',async t=>{
  const s=await fixture(t,faultEnv('receipt'));
  await mkdir(join(s.root,'.git'));
  assert.equal((await s.post({text:'still dispatches',operation_id:'vcs-ok'})).status,202);
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.ready,true);assert.notEqual(state.runtime.state,'blocked');
});


for(const command of ['submit','steer','cancel','goal','goal-clear','goal-reconcile','goal-retry'])test('Rust failed '+command+' checkpoint exits for restart and keeps only durable receipts and goals',async t=>{
  const goalCommand=command.startsWith('goal');
  const s=await fixture(t,faultEnv(goalCommand?'goal-ack-lost':'receipt'));
  let queued;
  if(command==='cancel')queued=await s.post({text:'keep queued after failed cancel',operation_id:'cancel-before-failure'}).then(r=>r.json());
  if(['goal-clear','goal-reconcile','goal-retry'].includes(command)) {
    assert.equal((await fetch(s.base+'/api/goal',{method:'PUT',body:JSON.stringify({objective:'Durable objective',operationId:'durable-goal'})})).status,400);
  }
  const journal=join(s.state,'rust-spike.json'),backup=join(s.state,'fixture-journal-backup');
  const before=await readFile(journal,'utf8'),saved=JSON.parse(before);
  await rename(journal,backup);await rm(journal,{force:true,recursive:true});await mkdir(journal);
  let response;
  if(command==='cancel')response=await fetch(s.base+'/v1/queue/'+controllerSegment(queued.submission.id),{method:'DELETE',headers:s.headers});
  else if(goalCommand)response=await fetch(s.base+(['goal-reconcile','goal-retry'].includes(command)?'/api/goal/'+command.slice(5):'/api/goal'),{method:command==='goal-clear'?'DELETE':command==='goal'?'PUT':'POST',body:JSON.stringify({id:'durable-goal',objective:'Unpublished objective',operationId:'unpublished-goal'})});
  else response=await s.post({text:'must not publish',operation_id:'unpublished',delivery:command==='steer'?'steer':'queue'});
  assert.ok(response.status>=400);
  // Unusable storage: the launcher exits non-zero for the Host to restart it.
  assert.notEqual((await once(s.child,'exit'))[0],0);
  assert.equal(await readFile(backup,'utf8'),before);
  assert.deepEqual((await readdir(s.state)).filter(name=>name.startsWith('.rust-spike-')),[]);
  await rm(journal,{recursive:true});await rename(backup,journal);
  const restarted=await s.launch({});
  const receipts=await fetch(restarted.base+'/v1/queue',{headers:s.headers}).then(r=>r.json());
  assert.deepEqual(receipts.submissions.map(r=>r.id),saved.records.map(r=>r.submission.id),'only durable receipts survive');

});

for(const phase of ['dispatch-intent','dispatch-result','completion','notification-ack','goal-applying','goal-applied','goal-mode','goal-sent','directory-sync'])test('Rust failed '+phase+' checkpoint exits for restart and keeps only published receipts and goals',async t=>{
  const s=await fixture(t,{RUST_SPIKE_CHECKPOINT_FAIL:phase,FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_COMPLETE_TURNS:'1',PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH});
  if(phase.startsWith('goal-'))await fetch(s.base+'/api/goal',{method:'PUT',body:JSON.stringify({objective:'Checkpoint objective',operationId:'checkpoint-goal'})});
  else await s.post({text:'checkpoint prompt',operation_id:'checkpoint-prompt',notify:{to:'ag_target',on:['done']}});
  // A failed checkpoint exits the launcher non-zero; the journal keeps only published state.
  assert.notEqual((await once(s.child,'exit'))[0],0);
  const path=join(s.state,'rust-spike.json'),saved=JSON.parse(await readFile(path,'utf8'));
  assert.ok(Array.isArray(saved.records));
  if(['dispatch-intent','directory-sync'].includes(phase))assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,0);
});
