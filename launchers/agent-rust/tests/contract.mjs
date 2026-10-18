import assert from 'node:assert/strict';
import test from 'node:test';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, mkdir, readFile, realpath, rm, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';
import { createRequire } from 'node:module';
import { hermeticEnv, localFetch as fetch } from '../../agent/tests/hermetic.mjs';
import { fakePrivateState } from '../../agent/tests/fake-private-state.mjs';

const nodeRoot = resolve(process.env.NODE_LAUNCHER_ROOT || '../agent');
const { WebSocket } = createRequire(join(nodeRoot, 'package.json'))('ws');
const binary = process.env.RUST_SPIKE_BIN;
const useNode = process.env.COMPARE_NODE === '1';
// Match Controller's NON_ALPHANUMERIC encoding, which also escapes '-' / '_'.
const controllerSegment = id => id.replace(/[^a-z0-9]/gi, c => '%'+c.charCodeAt(0).toString(16).toUpperCase());
assert.ok(binary || useNode, 'RUST_SPIKE_BIN or COMPARE_NODE required');

async function until(read, predicate) {
  const end = Date.now() + 12000;
  while (Date.now() < end) { try {const value=await read();if(predicate(value))return value;}catch{} await new Promise(r=>setTimeout(r,20)); }
  throw Error('contract fixture deadline');
}
async function fixture(t, extra={}) {
  const root=await realpath(await mkdtemp(join(tmpdir(),'rust158-')));
  const cwd=join(root,'cwd'), state=join(root,'private');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const controller=await fakePrivateState('ag_contract');
  const portFile=join(root,'port');
  const env=hermeticEnv({ HOME:root, CODEX_HOME:join(root,'codex'), AGENT_UI_CWD:cwd, AGENT_UI_PUBLIC_DIR:join(nodeRoot,'public'), AGENT_UI_PORT_FILE:portFile, AGENT_UI_PORT:'0', TREER_AGENT_ID:'ag_contract', TREER_WORKLOAD_CREDENTIAL:'fixture-only', TREER_AGENT_SERVER_URL:controller.url, TREER_AGENT_PRIVATE_STATE_DIR:state, AGENT_UI_INSTANCE_ID:'contract-instance', AGENT_UI_MESSAGE_WATCH:'0', CODEX_BIN:join(nodeRoot,'tests/fake-codex.mjs'), FAKE_CODEX_RELAY_RECOVERY:'1', ...extra });
  env.RUST_SPIKE_CLI_LOG=join(root,'registration.json');
  env.RUST_SPIKE_PROVIDER_LOG=join(root,'provider.jsonl');
  if(useNode&&process.env.NODE_LAUNCHER_RUNTIME){env.AGENT_UI_APP_ROOT=dirname(process.env.NODE_LAUNCHER_RUNTIME);env.AGENT_UI_PUBLIC_DIR=join(env.AGENT_UI_APP_ROOT,'public');}
  const children=[];let logs='';
  const launch=async overrides=>{
    await rm(portFile,{force:true});
    const child=spawn(useNode?process.execPath:binary,useNode?(process.env.NODE_LAUNCHER_RUNTIME?[process.env.NODE_LAUNCHER_RUNTIME]:['--import',join(nodeRoot,'node_modules/tsx/dist/loader.mjs'),join(nodeRoot,'src/index.ts')]):[],{cwd,env:{...env,...overrides},stdio:['ignore','pipe','pipe']});
    children.push(child);child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
    const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(portFile,'utf8'))}`,x=>!x.endsWith('NaN'));
    try {await until(()=>fetch(base+'/api/state').then(r=>r.json()),x=>x.ready===true);}catch(e){throw Error(e.message+' '+logs);}
    return {child,base};
  };
  t.after(async()=>{for(const child of children){if(child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await once(child,'exit');}}await controller.close();await rm(root,{recursive:true,force:true});});
  const {child,base}=await launch({});
  const headers={'x-treer-interface-source':'controller-api','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'contract-instance',host:'agent-interface'};
  const post=body=>fetch(base+'/v1/prompts',{method:'POST',headers,body:JSON.stringify(body)});
  return {base,post,headers,child,root,state,launch,logs:()=>logs};
}

test('Rust serves its manifest while Controller registration probes it',{skip:useNode},async t=>{
  const s=await fixture(t,{RUST_SPIKE_REGISTER:'1',PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH});
  const manifest=await until(()=>readFile(join(s.root,'registration.json'),'utf8').then(JSON.parse),x=>x.instance_id==='contract-instance');
  assert.equal(manifest.protocol,'treer.agent-interface/v1');
});

test('same manifest and initial transcript contract',async t=>{
  const s=await fixture(t);
  const manifest=await fetch(s.base+'/v1/manifest').then(r=>r.json());
  const contract=JSON.parse(await readFile(join(nodeRoot,'interface-contract.json'),'utf8'));
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
  if(!useNode){
    for(const invalid of ['%FF','%2Fbad','%252D','bad%'])
      assert.equal((await fetch(s.base+'/v1/submissions/'+invalid,{headers:s.headers})).status,400);
  }
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

test('Rust journal restart never replays an uncertain turn and preserves queued operation identity',{skip:useNode},async t=>{
  const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:''});
  const first=await s.post({text:'hold',operation_id:'hold'}).then(r=>r.json());
  const queued=await s.post({text:'later',operation_id:'later'}).then(r=>r.json());
  assert.equal(queued.submission.disposition,'queued');
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const second=await s.launch({FAKE_CODEX_RELAY_RECOVERY:'1'});
  const replay=await fetch(second.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'later',operation_id:'later'})}).then(r=>r.json());
  assert.equal(replay.duplicate,true);assert.equal(replay.submission.id,queued.submission.id);
  const old=await fetch(second.base+'/v1/submissions/'+first.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(old.submission.state,'failed');assert.match(old.submission.error,/uncertain/);
  const pending=await fetch(second.base+'/v1/submissions/'+queued.submission.id,{headers:s.headers}).then(r=>r.json());
  assert.equal(pending.submission.state,'queued');
  const snapshot=await fetch(second.base+'/api/state').then(r=>r.json());
  assert.equal(snapshot.runtime.state,'blocked');
});

const faultEnv=mode=>({CODEX_BIN:process.execPath,RUST_SPIKE_CODEX_ARGS:JSON.stringify([resolve(import.meta.dirname,'fixtures/fault-codex.mjs')]),RUST_SPIKE_FAULT:mode});
const providerCalls=async s=>(await readFile(join(s.root,'provider.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
test('Rust shutdown kills the provider-owned process group including a descendant',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv('descendant'));
  const pid=Number(await readFile(join(s.root,'provider.jsonl.child'),'utf8'));
  const alive=()=>{try{process.kill(pid,0);return true}catch{return false}};
  assert.equal(alive(),true);
  t.after(()=>{if(alive())try{process.kill(pid,'SIGKILL')}catch{}});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  await until(async()=>alive(),x=>x===false);
});
test('Rust refuses unknown provider RPC requests with method-not-found',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv('unknown-request'));
  const calls=await until(()=>providerCalls(s),calls=>calls.some(c=>c.id==='provider-unknown'));
  const response=calls.find(c=>c.id==='provider-unknown');assert.equal(response.error.code,-32601);assert.equal(response.result,undefined);
});
test('Rust rejects a writable non-sticky private-state ancestor at runtime',{skip:useNode},async t=>{
  const s=await fixture(t);
  const before=await readFile(join(s.state,'rust-spike.json'),'utf8');
  await chmod(s.root,0o777);
  assert.ok((await s.post({text:'must not dispatch',operation_id:'unsafe-parent'})).status>=400);
  assert.equal(await readFile(join(s.state,'rust-spike.json'),'utf8'),before);
  await chmod(s.root,0o700);
});
for(const mode of ['omit','timeout'])test('Rust uncertain '+mode+' blocks all later dispatch including after restart',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv(mode));
  const first=s.post({text:'accepted but ambiguous',operation_id:'uncertain'});
  await until(()=>providerCalls(s),calls=>calls.some(c=>c.method==='turn/start'));
  const second=s.post({text:'must remain queued',operation_id:'later'});
  assert.equal((await first).status,202);
  const admission=await (await second).json();assert.equal(admission.submission.disposition,'queued');
  await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='blocked');
  await new Promise(resolve=>setTimeout(resolve,2200));
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const restarted=await s.launch({});
  const retry=await fetch(restarted.base+'/v1/prompts',{method:'POST',headers:s.headers,body:JSON.stringify({text:'must remain queued',operation_id:'later'})}).then(r=>r.json());
  assert.deepEqual(retry.submission,admission.submission);
  await new Promise(resolve=>setTimeout(resolve,2200));
  assert.equal((await providerCalls(s)).filter(c=>c.method==='turn/start').length,1);
});

test('Rust immutable queued receipt survives dispatch/restart while dynamic queue positions change',{skip:useNode},async t=>{
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

test('Rust stalled notification worker permits continuous provider deltas and prompt abort',{skip:useNode},async t=>{
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

test('Rust runtime VCS placement invalidation refuses a new prompt without journaling',{skip:useNode},async t=>{
  const s=await fixture(t);
  const before=await readFile(join(s.state,'rust-spike.json'),'utf8');
  await mkdir(join(s.root,'.git'));
  assert.ok((await s.post({text:'must not dispatch',operation_id:'invalidated'})).status>=400);
  assert.equal(await readFile(join(s.state,'rust-spike.json'),'utf8'),before);
  const queue=await fetch(s.base+'/v1/queue',{headers:s.headers}).then(r=>r.json());
  assert.equal(queue.submissions.length,0);
});
