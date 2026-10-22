import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,mkdir,realpath,writeFile,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {hermeticEnv,localFetch as fetch} from '../../agent/tests/hermetic.mjs';
import {fakePrivateState} from '../../agent/tests/fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
async function until(fn,p){const end=Date.now()+12000;while(Date.now()<end){try{const v=await fn();if(p(v))return v;}catch{}await new Promise(r=>setTimeout(r,20));}throw Error('home-cwd entry deadline');}
async function setup(t,kind='hidden'){
  const home=await realpath(await mkdtemp(join(tmpdir(),'rust158-home-')));
  const state=kind==='equal'?home:join(home,kind==='visible'?'state':'.local/state/agent-private/ag_contract');
  await mkdir(state,{recursive:true,mode:0o700});
  if(kind==='vcs')await writeFile(join(home,'.git'),'synthetic marker');
  const source=join(home,'.treer/agents/ag_contract');await mkdir(source,{recursive:true,mode:0o700});
  const thread='thread-home-fixture',queue=JSON.stringify({version:1,records:[]})+'\n';
  await writeFile(join(source,'agent-ui.json'),JSON.stringify({threadId:thread})+'\n',{mode:0o600});
  await writeFile(join(source,'queue.json'),queue,{mode:0o600});
  const controller=await fakePrivateState('ag_contract','fixture-only'),calls=join(home,'calls'),port=join(home,'port'),children=[];let logs='';
  const launch=()=>{
    const env=hermeticEnv({HOME:home,CODEX_HOME:join(home,'synthetic-unused-codex'),AGENT_UI_CWD:home,AGENT_UI_PUBLIC_DIR:resolve('../agent/public'),AGENT_UI_PORT_FILE:port,AGENT_UI_PORT:'0',AGENT_UI_SESSION_ID:thread,TREER_AGENT_ID:'ag_contract',TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_SERVER_URL:controller.url,TREER_AGENT_PRIVATE_STATE_DIR:state,AGENT_UI_MESSAGE_WATCH:'0',CODEX_BIN:resolve('../agent/tests/fake-codex.mjs'),FAKE_CODEX_COMPLETE_TURNS:'1',FAKE_CODEX_RICH:'1',FAKE_CODEX_CALLS:calls,RUST_SPIKE_PROVIDER_LOG:calls});env.RUST_SPIKE_REGISTER='0';
    const child=spawn(binary,['--import-node-state'],{cwd:home,env,stdio:['ignore','pipe','pipe']});children.push(child);child.stdout.on('data',b=>logs+=b);child.stderr.on('data',b=>logs+=b);return child;
  };
  t.after(async()=>{for(const c of children)if(c.exitCode===null&&c.signalCode===null){const done=once(c,'exit');c.kill('SIGTERM');await done;}await controller.close();await rm(home,{recursive:true,force:true});});
  return {home,state,source,thread,queue,calls,port,launch,logs:()=>logs};
}
test('HOME hidden non-VCS private import and same-session receipt restart, no input replay',async t=>{
  const f=await setup(t),child=f.launch();
  const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(f.port,'utf8'))}`,s=>!s.endsWith('NaN'));
  const snapshot=()=>fetch(base+'/api/state').then(r=>r.json());
  let state=await until(snapshot,s=>s.ready);assert.equal(state.thread.id,f.thread);
  assert.equal((await fetch(base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'synthetic home cwd retained tool',operationId:'home-once'})})).status,200);
  state=await until(snapshot,s=>s.thread.status==='idle'&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='commandExecution')));
  const turn=state.thread.turns.at(-1);assert.equal(turn.status,'completed');
  assert.equal(await readFile(join(f.source,'queue.json'),'utf8'),f.queue);
  assert.equal(await readFile(join(f.state,'node-backup-v1/queue.json'),'utf8'),f.queue);
  const journal=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));assert.equal(journal.threadId,f.thread);assert.equal(journal.records.length,1);
  const done=once(child,'exit');child.kill('SIGTERM');assert.equal((await done)[0],0);
  await rm(f.port,{force:true});f.launch();
  const next=await until(async()=>`http://127.0.0.1:${Number(await readFile(f.port,'utf8'))}`,s=>!s.endsWith('NaN'));
  state=await until(()=>fetch(next+'/api/state').then(r=>r.json()),s=>s.ready);
  assert.equal(state.thread.id,f.thread);
  const after=JSON.parse(await readFile(join(f.state,'rust-spike.json'),'utf8'));assert.deepEqual(after.records.map(r=>r.submission.id),journal.records.map(r=>r.submission.id));
  const calls=(await readFile(f.calls,'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.filter(c=>c.method==='turn/start').length,1,'restart must not resend accepted prompt');
});
for(const kind of ['equal','visible','vcs'])test('actual entry refuses '+kind+' private placement before provider/import',async t=>{
  const f=await setup(t,kind),child=f.launch();assert.equal((await once(child,'exit'))[0],1);assert.match(f.logs(),/private_state_unavailable/);
  await assert.rejects(readFile(f.calls));await assert.rejects(readFile(join(f.state,'.rust-launcher.lock')));await assert.rejects(readFile(join(f.state,'rust-spike.json')));
  assert.equal(await readFile(join(f.source,'queue.json'),'utf8'),f.queue);
});
