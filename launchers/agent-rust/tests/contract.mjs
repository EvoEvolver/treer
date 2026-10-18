import assert from 'node:assert/strict';
import test from 'node:test';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { mkdtemp, mkdir, readFile, writeFile, realpath, rm, chmod, readdir, unlink, symlink, lstat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, dirname } from 'node:path';
import { createRequire } from 'node:module';
import { hermeticEnv, localFetch as fetch } from '../../agent/tests/hermetic.mjs';
import { fakePrivateState } from '../../agent/tests/fake-private-state.mjs';

const nodeRoot = resolve(process.env.NODE_LAUNCHER_ROOT || '../agent');
const { WebSocket } = createRequire(join(nodeRoot, 'package.json'))('ws');
const binary = process.env.RUST_SPIKE_BIN;
const useNode = process.env.COMPARE_NODE === '1';
const execFileAsync = promisify(execFile);
async function rssKiB(pid) {
  const {stdout}=await execFileAsync('/bin/ps',['-o','rss=','-p',String(pid)]);
  const rss=Number(stdout.trim());assert.ok(Number.isFinite(rss)&&rss>0);return rss;
}
// Match Controller's NON_ALPHANUMERIC encoding, which also escapes '-' / '_'.
const controllerSegment = id => id.replace(/[^a-z0-9]/gi, c => '%'+c.charCodeAt(0).toString(16).toUpperCase());
assert.ok(binary || useNode, 'RUST_SPIKE_BIN or COMPARE_NODE required');

async function until(read, predicate) {
  const end = Date.now() + 12000;
  while (Date.now() < end) { try {const value=await read();if(predicate(value))return value;}catch{} await new Promise(r=>setTimeout(r,20)); }
  throw Error('contract fixture deadline');
}
async function fixture(t, extra={}, controllerOptions={}) {
  const root=await realpath(await mkdtemp(join(tmpdir(),'rust158-')));
  const cwd=join(root,'cwd'), state=join(root,'private');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const controller=await fakePrivateState('ag_contract','fixture-only',controllerOptions);
  const portFile=join(root,'port');
  const env=hermeticEnv({ HOME:root, CODEX_HOME:join(root,'codex'), AGENT_UI_CWD:cwd, AGENT_UI_PUBLIC_DIR:join(nodeRoot,'public'), AGENT_UI_PORT_FILE:portFile, AGENT_UI_PORT:'0', TREER_AGENT_ID:'ag_contract', TREER_WORKLOAD_CREDENTIAL:'fixture-only', TREER_AGENT_SERVER_URL:controller.url, TREER_AGENT_PRIVATE_STATE_DIR:state, AGENT_UI_INSTANCE_ID:'contract-instance', AGENT_UI_MESSAGE_WATCH:'0', CODEX_BIN:join(nodeRoot,'tests/fake-codex.mjs'), FAKE_CODEX_RELAY_RECOVERY:'1', ...extra });
  env.RUST_SPIKE_CLI_LOG=join(root,'registration.json');
  env.RUST_SPIKE_PROVIDER_LOG=join(root,'provider.jsonl');
  if(useNode&&process.env.NODE_LAUNCHER_RUNTIME){env.AGENT_UI_APP_ROOT=dirname(process.env.NODE_LAUNCHER_RUNTIME);env.AGENT_UI_PUBLIC_DIR=join(env.AGENT_UI_APP_ROOT,'public');}
  const children=[];let logs='';
  const launch=async (overrides,waitReady=true)=>{
    await rm(portFile,{force:true});
    const child=spawn(useNode?process.execPath:binary,useNode?(process.env.NODE_LAUNCHER_RUNTIME?[process.env.NODE_LAUNCHER_RUNTIME]:['--import',join(nodeRoot,'node_modules/tsx/dist/loader.mjs'),join(nodeRoot,'src/index.ts')]):[],{cwd,env:{...env,...overrides},stdio:['ignore','pipe','pipe']});
    children.push(child);child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
    const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(portFile,'utf8'))}`,x=>!x.endsWith('NaN'));
    if(waitReady)try {await until(()=>fetch(base+'/api/state').then(r=>r.json()),x=>x.ready===true);}catch(e){throw Error(e.message+' '+logs);}
    return {child,base};
  };
  t.after(async()=>{for(const child of children){if(child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await once(child,'exit');}}await controller.close();await rm(root,{recursive:true,force:true});});
  const {child,base}=await launch({});
  const headers={'x-treer-interface-source':'controller-api','x-treer-agent-id':'ag_contract','x-treer-interface-instance':'contract-instance',host:'agent-interface'};
  const post=body=>fetch(base+'/v1/prompts',{method:'POST',headers,body:JSON.stringify(body)});
  return {base,post,headers,child,root,state,launch,controller,logs:()=>logs,trackChild:child=>children.push(child)};
}

test('Rust serves its manifest while Controller registration probes it',{skip:useNode},async t=>{
  const s=await fixture(t,{RUST_SPIKE_REGISTER:'1',PATH:resolve(import.meta.dirname,'fixtures')+':'+process.env.PATH});
  const manifest=await until(()=>readFile(join(s.root,'registration.json'),'utf8').then(JSON.parse),x=>x.instance_id==='contract-instance');
  assert.equal(manifest.protocol,'treer.agent-interface/v1');
});

test('Rust canonicalizes a configured cwd alias and reports root failures without paths',{skip:useNode,timeout:15000},async t=>{
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

test('many-turn history limit stops provider, preserves receipts and remains readable',{skip:useNode,timeout:60000},async t=>{
  const s=await fixture(t,{
    CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),
    // The fixture root is unknown until launch; the private root is explicit.
    HISTORY_PID_FILE:'provider.pid',
  });
  const providerPid=Number(await readFile(join(s.root,'cwd/provider.pid'),'utf8'));
  const baseline=await rssKiB(s.child.pid);let peak=baseline;
  let rejected;
  const admitted=[];
  for(let n=0;n<40;n++) {
    const response=await s.post({text:`bounded history ${n}`,operation_id:`history-${n}`});
    assert.equal(response.status,202);
    const receipt=await response.json();admitted.push(receipt.submission.id);
    const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>!state.ready||state.thread.status==='idle');
    peak=Math.max(peak,await rssKiB(s.child.pid));
    if(!state.ready){rejected=receipt.submission.id;break;}
  }
  assert.ok(rejected,'default history budget was reached');
  const state=await fetch(s.base+'/api/state').then(r=>r.json());
  assert.equal(state.runtime.state,'blocked');
  assert.match(state.runtime.error,/history limit/);
  assert.ok(Buffer.byteLength(JSON.stringify(state.thread.turns))<17*1024*1024);
  await until(async()=>{try{process.kill(providerPid,0);return false;}catch(e){if(e.code==='ESRCH')return true;throw e;}},Boolean);
  const cleanup=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>state.runtime.cleanup.providerExit==='confirmed');
  assert.equal(cleanup.runtime.cleanup.processGroup,'termination_requested_unverified');
  assert.equal(cleanup.runtime.cleanup.detachedDescendants,'not_guaranteed');
  assert.match(cleanup.runtime.error,/detached-descendant cleanup not guaranteed/);
  const receipt=await fetch(s.base+'/v1/submissions/'+rejected,{headers:s.headers}).then(r=>r.json());
  assert.equal(receipt.submission.state,'running','oversized completion must not invent success');
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(journal.dispatchBlocked,true);
  assert.equal(journal.records.length,admitted.length);
  assert.ok((await s.post({text:'no automatic replay',operation_id:'after-limit'})).status>=400);
  const before=JSON.stringify(state.thread.turns);
  await new Promise(r=>setTimeout(r,250));
  assert.equal(JSON.stringify((await fetch(s.base+'/api/state').then(r=>r.json())).thread.turns),before);
  const stopped=await rssKiB(s.child.pid);
  // RSS is allocator/platform dependent; this is a regression ceiling for the
  // synthetic 16 MiB history, not a general process quota or a heap measurement.
  assert.ok(peak<384*1024,`synthetic history RSS ${peak} KiB exceeds ceiling`);
  assert.ok(stopped<384*1024);
  t.diagnostic(JSON.stringify({turns:admitted.length,baselineRssKiB:baseline,peakSampledRssKiB:peak,stoppedRssKiB:stopped}));
  const exited=once(s.child,'exit');s.child.kill('SIGTERM');await exited;
  const restarted=await s.launch({});
  const recovered=await fetch(restarted.base+'/api/state').then(r=>r.json());
  assert.equal(recovered.runtime.state,'blocked');
  const saved=await fetch(restarted.base+'/v1/submissions/'+rejected,{headers:s.headers}).then(r=>r.json());
  assert.equal(saved.submission.state,'failed');
  assert.match(saved.submission.error,/uncertain/);
});

test('streaming deltas stop at admission limit without replacing accepted text',{skip:useNode,timeout:30000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_DELTA:'1'});
  const receipt=await s.post({text:'bounded stream',operation_id:'stream'}).then(r=>r.json());
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>!state.ready);
  assert.match(state.runtime.error,/history limit/);
  const answer=state.thread.turns.flatMap(t=>t.items).find(i=>i.id==='stream');
  assert.equal(answer.text.length,14*1024*1024,'oversized delta is refused atomically');
  const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(saved.dispatchBlocked,true);
  assert.equal(saved.records.find(r=>r.submission.id===receipt.submission.id).submission.state,'running');
  const pid=Number(await readFile(join(s.root,'cwd/provider.pid'),'utf8'));
  await until(async()=>{try{process.kill(pid,0);return false;}catch(e){if(e.code==='ESRCH')return true;throw e;}},Boolean);
});

test('invalid provider turn identity blocks dispatch without echoing provider content',{skip:useNode,timeout:15000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid',HISTORY_INVALID_ID:'1'});
  const receipt=await s.post({text:'malformed completion',operation_id:'invalid-id'}).then(r=>r.json());
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),state=>state.runtime?.cleanup?.providerExit==='confirmed');
  assert.equal(state.ready,false);
  assert.equal(state.runtime.state,'blocked');
  assert.match(state.runtime.error,/Invalid provider turn identity; queue paused/);
  assert.doesNotMatch(JSON.stringify(state),/synthetic-do-not-echo/);
  assert.equal(state.thread.turns.length,1,'malformed completion cannot append a turn');
  assert.equal(state.thread.turns[0].id,'history-1','accepted turn identity is preserved');
  assert.ok(state.thread.turns[0].items.some(item=>item.kind==='userMessage'&&item.text==='malformed completion'));
  assert.ok(state.thread.turns[0].items.every(item=>item.kind!=='agentMessage'),'malformed completion cannot replace accepted content');
  const saved=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(saved.dispatchBlocked,true);
  assert.equal(saved.records.find(r=>r.submission.id===receipt.submission.id).submission.state,'running');
  assert.equal((await fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers})).status,200);
  assert.ok((await s.post({text:'must not dispatch',operation_id:'after-invalid'})).status>=400);
  const pid=Number(await readFile(join(s.root,'cwd/provider.pid'),'utf8'));
  assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

test('oversized resumed history fails visibly without rewriting the durable journal',{skip:useNode,timeout:10000},async t=>{
  const s=await fixture(t,{CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'provider.pid'});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const original=await readFile(join(s.state,'rust-spike.json'),'utf8');
  const portFile=join(s.root,'oversized-port');
  const env=hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(nodeRoot,'public'),CODEX_BIN:resolve(import.meta.dirname,'fixtures/history-provider.mjs'),HISTORY_PID_FILE:'oversized.pid',HISTORY_OVERSIZED:'1',AGENT_UI_PORT_FILE:portFile});
  const child=spawn(binary,[],{cwd:join(s.root,'cwd'),env,stdio:'ignore'});
  s.trackChild(child);
  const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(portFile,'utf8'))}`,x=>!x.endsWith('NaN'));
  const state=await until(()=>fetch(base+'/api/state').then(r=>r.json()),state=>state.runtime?.cleanup?.providerExit==='confirmed');
  assert.equal(state.ready,false);
  assert.equal(state.runtime.state,'blocked');
  assert.match(state.runtime.error,/thread exceeds launcher history limit/);
  assert.match(state.runtime.error,/no fresh-thread fallback/);
  assert.match(state.runtime.error,/detached-descendant cleanup not guaranteed/);
  assert.equal(await readFile(join(s.state,'rust-spike.json'),'utf8'),original);
  const pid=Number(await readFile(join(s.root,'cwd/oversized.pid'),'utf8'));
  assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
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
  if(!useNode){
    for(const invalid of ['%FF','%2Fbad','%252D','bad%'])
      assert.equal((await fetch(s.base+'/v1/submissions/'+invalid,{headers:s.headers})).status,400);
  }
});
test('Rust keeps journaled user text and attested sender after completion and restart',{skip:useNode},async t=>{
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
test('Rust corrupt journal refuses startup before provider dispatch and preserves evidence',{skip:useNode},async t=>{
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
  const child=spawn(binary,[],{cwd:join(s.root,'cwd'),env:hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(nodeRoot,'public'),CODEX_BIN:join(nodeRoot,'tests/fake-codex.mjs'),RUST_SPIKE_PROVIDER_LOG:join(s.root,'provider.jsonl')}),stdio:['ignore','pipe','pipe']});
  let logs='';child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
  const [code]=await once(child,'exit');
  assert.notEqual(code,0);
  assert.match(logs,/private_state_invalid/);
  assert.doesNotMatch(logs,/private text|corruption-check/);
  assert.equal(await readFile(path,'utf8'),bytes);
  assert.equal(await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null),providerBefore);
});
test('Rust refuses a FIFO journal promptly before provider startup',{skip:useNode,timeout:10000},async t=>{
  const s=await fixture(t);
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  const path=join(s.state,'rust-spike.json');
  await unlink(path);
  await execFileAsync('/usr/bin/mkfifo',[path]);
  const providerBefore=await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null);
  const env=hermeticEnv({HOME:s.root,CODEX_HOME:join(s.root,'codex'),AGENT_UI_CWD:join(s.root,'cwd'),TREER_RUST_SPIKE_STATE_DIR:s.state,AGENT_UI_PUBLIC_DIR:join(nodeRoot,'public'),CODEX_BIN:join(nodeRoot,'tests/fake-codex.mjs'),RUST_SPIKE_PROVIDER_LOG:join(s.root,'provider.jsonl')});
  await assert.rejects(execFileAsync(binary,[],{cwd:join(s.root,'cwd'),env,timeout:2000,killSignal:'SIGKILL'}),error=>{
    assert.equal(error.killed,false,'FIFO must be refused without the watchdog killing startup');
    assert.match(error.stderr,/private_state_invalid/);
    return true;
  });
  assert.equal((await lstat(path)).isFIFO(),true);
  assert.equal(await readFile(join(s.root,'provider.jsonl'),'utf8').catch(()=>null),providerBefore);
});
test('Rust goal edits persist native state and deduplicate model-context delivery across restart',{skip:useNode},async t=>{
  const s=await fixture(t,{FAKE_CODEX_GOAL_ACTIVE:'1'});
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
test('Rust uncertain native goal update is observed on restart without repeating the mutation',{skip:useNode},async t=>{
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
test('Rust mismatching goal reconciliation refuses a stale native mutation',{skip:useNode},async t=>{
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
test('Rust images are validated, quota admitted, retained in history and durable across restart',{skip:useNode},async t=>{
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
test('Rust failed image quota commit rolls back exact data and releases reservation',{skip:useNode},async t=>{
  const s=await fixture(t,{}, {commitError:'private_state_reservation_expired'});
  const r=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});
  assert.equal(r.status,503);
  assert.equal(s.controller.reservations.size,0);
  assert.ok(s.controller.events.some(e=>e.method==='DELETE'));
  assert.equal((await readdir(join(s.state,'rust-images'))).some(n=>n.startsWith('img_')),false);
});
test('Rust image reads refuse replaced symlinks and preserve the outside sentinel',{skip:useNode},async t=>{
  const s=await fixture(t);
  const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
  const sentinel=join(s.root,'outside-image');await writeFile(sentinel,'sentinel',{mode:0o600});
  const path=join(s.state,'rust-images',attachment.id+'.gif');await unlink(path);await symlink(sentinel,path);
  const r=await fetch(s.base+'/api/attachments/'+attachment.id);assert.equal(r.status,503);
  assert.equal(await readFile(sentinel,'utf8'),'sentinel');
  assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id,{method:'DELETE'})).status,503);
  assert.equal(await readFile(sentinel,'utf8'),'sentinel');
});
test('Rust retained images stop admission at byte and entry bounds',{skip:useNode},async t=>{
  for(const extra of [{RUST_SPIKE_IMAGE_QUOTA_BYTES:String(imageGif.length)},{RUST_SPIKE_IMAGE_ENTRY_LIMIT:'1'}]) {
    const s=await fixture(t,{FAKE_CODEX_RELAY_RECOVERY:'',FAKE_CODEX_IMAGES:'1',...extra});
    const {attachment}=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif}).then(r=>r.json());
    await s.post({text:'retain',attachment_ids:[attachment.id],operation_id:'retained-bound'});
    const r=await fetch(s.base+'/api/attachments',{method:'POST',body:imageGif});assert.equal(r.status,413);
    assert.equal((await r.json()).error,'attachment_quota_exceeded');
    assert.equal((await fetch(s.base+'/api/attachments/'+attachment.id)).status,200);
  }
});
test('Rust quota timeout releases the image store for reads and deletes',{skip:useNode,timeout:12000},async t=>{
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
test('Rust missing expired drafts do not block a new upload',{skip:useNode},async t=>{
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
test('Rust observes an explicit terminal accepted turn without a completion event',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv('observe'));
  const receipt=await s.post({text:'observe only',operation_id:'observe'}).then(r=>r.json());
  const result=await until(()=>fetch(s.base+'/v1/submissions/'+receipt.submission.id,{headers:s.headers}).then(r=>r.json()),x=>x.submission?.state==='done');
  assert.equal(result.submission.turn_id,'fault-1');
  const calls=await providerCalls(s);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,1);
  assert.ok(calls.some(c=>c.method==='thread/read'));
});
test('Rust sparse completion keeps accepted user input alongside the assistant',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv('sparse'));
  await s.post({text:'keep this input',operation_id:'sparse-user'});
  const state=await until(()=>fetch(s.base+'/api/state').then(r=>r.json()),x=>x.runtime.state==='idle');
  const items=state.thread.turns.flatMap(t=>t.items);
  assert.equal(items.filter(i=>i.kind==='userMessage'&&i.text==='keep this input').length,1);
  assert.ok(items.some(i=>i.kind==='agentMessage'&&i.text==='sparse answer'));
});
test('Rust shutdown kills the provider-owned process group including a descendant',{skip:useNode},async t=>{
  const s=await fixture(t,faultEnv('descendant'));
  const pid=Number(await readFile(join(s.root,'provider.jsonl.child'),'utf8'));
  const alive=()=>{try{process.kill(pid,0);return true}catch{return false}};
  assert.equal(alive(),true);
  t.after(()=>{if(alive())try{process.kill(pid,'SIGKILL')}catch{}});
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;
  await until(async()=>alive(),x=>x===false);
  assert.match(s.logs(),/shutdown provider_exit=confirmed; process_group=termination_requested_unverified; detached_descendants=not_guaranteed/);
});
test('Rust shutdown cancels startup and an in-flight turn without waiting for the RPC timeout',{skip:useNode,timeout:25000},async t=>{
  const s=await fixture(t,faultEnv('timeout'));
  const provider=Number(await readFile(join(s.root,'provider.jsonl.pid'),'utf8'));
  const pending=s.post({text:'uncertain accepted input',operation_id:'shutdown-inflight'}).catch(()=>null);
  await until(()=>providerCalls(s),calls=>calls.some(c=>c.method==='turn/start'));
  const started=Date.now();
  const exit=once(s.child,'exit');s.child.kill('SIGTERM');await exit;await pending;
  assert.ok(Date.now()-started<8000,'shutdown must cancel the pending 60 s RPC');
  assert.throws(()=>process.kill(provider,0),{code:'ESRCH'});
  const journal=JSON.parse(await readFile(join(s.state,'rust-spike.json'),'utf8'));
  assert.equal(journal.dispatchBlocked,true);
  assert.equal(journal.records[0].submission.state,'running','stop must not invent completion');
  const restarted=await s.launch(faultEnv('shutdown-startup'),false);
  const nextProvider=await until(()=>readFile(join(s.root,'provider.jsonl.pid'),'utf8').then(Number),pid=>pid!==provider);
  const stop=Date.now();const stopped=once(restarted.child,'exit');restarted.child.kill('SIGTERM');await stopped;
  assert.ok(Date.now()-stop<8000);
  assert.throws(()=>process.kill(nextProvider,0),{code:'ESRCH'});
  assert.match(s.logs(),/shutdown provider_exit=confirmed/);
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
