// `--no-register`: a parent process (the Agent Hub) owns the launcher. It
// inherits a real Agent identity for its tools but must never register an
// interface, watch Core Messages or call the Controller; the journal lives in
// --state-dir and a restart on that directory resumes the same session.
import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,realpath,readFile,rm,access} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {hermeticEnv,localFetch as fetch} from './hermetic.mjs';
import {fakePrivateState} from './fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary,'RUST_SPIKE_BIN required');
async function until(fn,p){const end=Date.now()+12000;while(Date.now()<end){try{const v=await fn();if(p(v))return v;}catch{}await new Promise(r=>setTimeout(r,20));}throw Error('unregistered entry deadline');}

test('--no-register serves the AIS on loopback without registration, message watch or Controller calls; restart resumes',async t=>{
  const root=await realpath(await mkdtemp(join(tmpdir(),'launch-unreg-')));
  const cwd=join(root,'cwd'),state=join(root,'state'),portFile=join(root,'port'),cli=join(root,'cli.log'),calls=join(root,'calls');
  await mkdir(cwd,{mode:0o700});await mkdir(state,{mode:0o700});
  const controller=await fakePrivateState('ag_hub','fixture-only');
  const children=[];let logs='';
  t.after(async()=>{for(const c of children)if(c.exitCode===null&&c.signalCode===null){const done=once(c,'exit');c.kill('SIGTERM');await done;}await controller.close();await rm(root,{recursive:true,force:true});});
  // The inherited environment of a registered Agent, including the opt-ins
  // that would otherwise register and watch messages.
  const env=hermeticEnv({HOME:root,CODEX_HOME:join(root,'codex'),TREER_AGENT_ID:'ag_hub',TREER_WORKLOAD_CREDENTIAL:'fixture-only',
    TREER_AGENT_SERVER_URL:controller.url,TREER_AGENT_PRIVATE_STATE_DIR:join(root,'hub-private'),TREER_BIN:resolve('tests/fixtures/treer'),
    RUST_SPIKE_REGISTER:'1',AGENT_UI_MESSAGE_WATCH:'1',RUST_SPIKE_CLI_LOG:cli,RUST_SPIKE_MESSAGE_INPUT:join(root,'messages.json'),
    AGENT_UI_PUBLIC_DIR:resolve('public'),AGENT_UI_INSTANCE_ID:'hub-j-0001-a',CODEX_BIN:resolve('tests/fake-codex.mjs'),
    FAKE_CODEX_COMPLETE_TURNS:'1',FAKE_CODEX_CALLS:calls,RUST_SPIKE_PROVIDER_LOG:calls});
  const args=['--harness','codex','--no-register','--state-dir',state,'--listen','127.0.0.1:0','--port-file',portFile];
  const launch=async extra=>{
    await rm(portFile,{force:true});
    const child=spawn(binary,[...args,...extra],{cwd,env,stdio:['ignore','pipe','pipe']});children.push(child);
    child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);
    const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(portFile,'utf8'))}`,s=>!s.endsWith('NaN'));
    const ready=await until(()=>fetch(base+'/api/state').then(r=>r.json()),s=>s.ready===true).catch(e=>{throw Error(e.message+' '+logs)});
    return {child,base,ready};
  };
  const first=await launch([]);
  const thread=first.ready.thread.id;assert.ok(thread);
  const manifest=await fetch(first.base+'/v1/manifest').then(r=>r.json());
  assert.equal(manifest.instance_id,'hub-j-0001-a');
  // A loopback native client (the hub) may submit without Controller provenance.
  const submitted=await fetch(first.base+'/v1/prompts',{method:'POST',body:JSON.stringify({text:'hub first prompt',operation_id:'hub-j-0001-first'})});
  assert.equal(submitted.status,202);
  await until(()=>fetch(first.base+'/v1/status').then(r=>r.json()),s=>s.status==='idle'&&s.queue_length===0);
  const status=await fetch(first.base+'/v1/status').then(r=>r.json());
  assert.equal(status.agent_id,'','the runtime has no Agent identity of its own');
  const transcript=await fetch(first.base+'/v1/transcript?page=0&limit=1000').then(r=>r.json());
  assert.ok(transcript.entries.some(e=>e.role==='user'&&String(e.content).includes('hub first prompt')));
  // Browser requests still need the protected markers, which nothing can supply here.
  assert.equal((await fetch(first.base+'/api/state',{headers:{origin:'https://evil.invalid'}})).status,403);
  assert.equal((await fetch(first.base+'/api/state',{headers:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_hub','x-treer-interface-instance':'hub-j-0001-a'}})).status,403);
  // Same session after a restart on the same state directory, without replay.
  const done=once(first.child,'exit');first.child.kill('SIGTERM');assert.equal((await done)[0],0);
  const journal=JSON.parse(await readFile(join(state,'rust-spike.json'),'utf8'));assert.equal(journal.threadId,thread);
  const second=await launch(['--session-id',thread]);
  assert.equal(second.ready.thread.id,thread);
  const duplicate=await fetch(second.base+'/v1/prompts',{method:'POST',body:JSON.stringify({text:'hub first prompt',operation_id:'hub-j-0001-first'})}).then(r=>r.json());
  assert.equal(duplicate.duplicate,true);
  const providerCalls=(await readFile(calls,'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(providerCalls.filter(c=>c.method==='turn/start').length,1);
  // No registration, no message receive, no Controller private-state calls.
  await assert.rejects(access(cli),'the treer CLI was never invoked');
  assert.deepEqual(controller.events,[]);
});

test('--no-register refuses without --state-dir and owned flags without --no-register',async()=>{
  for(const args of [['--no-register'],['--state-dir','/tmp'],['--listen','127.0.0.1:0'],['--no-register','--state-dir','/tmp','--listen','0.0.0.0:1']]){
    const child=spawn(binary,args,{env:hermeticEnv({HOME:tmpdir()}),stdio:['ignore','pipe','pipe']});
    let err='';child.stderr.on('data',b=>err+=b);
    const [code]=await once(child,'exit');assert.notEqual(code,0,args.join(' '));assert.match(err,/--no-register|--state-dir|--listen/);
  }
});
