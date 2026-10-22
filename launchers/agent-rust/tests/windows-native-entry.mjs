// Real Windows PE launcher + synthetic provider protocol. No installed agents,
// provider credentials/history, Controller, or production lifecycle is used.
import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn, execFileSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp, mkdir, readFile, realpath, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {localFetch as fetch} from '../../agent/tests/hermetic.mjs';
const binary=process.env.RUST_SPIKE_BIN;
assert.equal(process.platform,'win32','this suite must execute on Windows');
assert.ok(binary,'RUST_SPIKE_BIN must name the Windows fixture-feature PE');
const sid=execFileSync('powershell.exe',['-NoProfile','-NonInteractive','-Command','[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'],{encoding:'utf8'}).trim();
assert.match(sid,/^S-1-/);
async function until(read, accept){const end=Date.now()+15000;while(Date.now()<end){try{const value=await read();if(accept(value))return value;}catch{}await new Promise(r=>setTimeout(r,30));}throw Error('windows_entry_deadline');}
for(const harness of ['codex','claude'])test(`Windows native ${harness}: text queue, durable restart and single writer`,async t=>{
  const root=await realpath(await mkdtemp(join(tmpdir(),'treer-rust-win-'))),cwd=join(root,'cwd'),state=join(root,'private'),port=join(root,'port');
  await mkdir(cwd);await mkdir(state);
  execFileSync('icacls.exe',[state,'/inheritance:r','/grant:r',`*${sid}:(OI)(CI)F`],{stdio:'pipe'});
  const children=[];
  t.after(async()=>{for(const child of children)if(child.exitCode===null&&child.signalCode===null){const exit=once(child,'exit');child.kill();await exit;}await rm(root,{recursive:true,force:true,maxRetries:30,retryDelay:100});});
  const env={SystemRoot:process.env.SystemRoot,TEMP:root,TMP:root,USERPROFILE:root,PATH:process.env.PATH,
    AGENT_UI_CWD:cwd,TREER_RUST_SPIKE_STATE_DIR:state,AGENT_UI_PUBLIC_DIR:resolve('../agent/public'),AGENT_UI_PORT_FILE:port,AGENT_UI_PORT:'0',AGENT_UI_HARNESS:harness,
    CODEX_BIN:process.execPath,CLAUDE_BIN:process.execPath,RUST_SPIKE_CODEX_ARGS:JSON.stringify([resolve('../agent/tests/fake-codex.mjs')]),RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-stream.mjs')]),CLAUDE_FIXTURE_LOG:join(root,'calls'),FAKE_CODEX_COMPLETE_TURNS:'1',FAKE_CODEX_STATUS:'idle'};
  const launch=()=>{const child=spawn(binary,[],{cwd,env,stdio:['ignore','pipe','pipe'],windowsHide:true});children.push(child);child.stdout.resume();let error='';child.stderr.on('data',b=>{error+=b.toString();});return {child,error:()=>error};};
  const first=launch();
  const address=async()=>{const value=Number(await readFile(port,'utf8'));assert.ok(value>0&&value<=65535);return `http://127.0.0.1:${value}`;};
  const base=await until(address,()=>true);
  const snapshot=()=>fetch(base+'/api/state').then(r=>r.json());
  const initial=await until(snapshot,s=>s.ready);
  assert.equal(initial.harness.id,harness);assert.equal(initial.attachmentsAvailable,false);
  const contender=launch();await once(contender.child,'exit');assert.notEqual(contender.child.exitCode,0);assert.match(contender.error(),/private_state_in_use/);
  for(const prompt of ['first Windows text','second Windows text']){
    const response=await fetch(base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt})});assert.equal(response.status,200);
    await until(snapshot,s=>s.runtime.state==='idle'&&s.thread.turns.some(t=>t.items.some(i=>i.kind==='userMessage'&&i.text===prompt)));
  }
  const before=await snapshot();const saved=JSON.parse(await readFile(join(state,'rust-spike.json'),'utf8'));assert.ok(saved.records.length>=2);
  const exit=once(first.child,'exit');first.child.kill();await exit;
  await rm(port);const second=launch();
  const next=await until(address,()=>true);
  const resumed=await until(()=>fetch(next+'/api/state').then(r=>r.json()),s=>s.ready);
  assert.equal(resumed.thread.id,before.thread.id);assert.equal(resumed.runtime.state,'idle');
  for(const record of saved.records)assert.ok(JSON.parse(await readFile(join(state,'rust-spike.json'),'utf8')).records.some(r=>r.submission.id===record.submission.id));
  assert.equal(second.child.exitCode,null);
});
