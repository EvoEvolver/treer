import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, realpath, mkdir, readFile, writeFile, rm, symlink, rename, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import { hermeticEnv, localFetch as fetch } from '../../agent/tests/hermetic.mjs';
import { fakePrivateState } from '../../agent/tests/fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
async function until(read,accept,ms=10000){const end=Date.now()+ms;while(Date.now()<end){const value=await read().catch(()=>null);if(value&&accept(value))return value;await new Promise(r=>setTimeout(r,15));}throw Error('owned snapshot fixture deadline');}
async function fixture(t,{gate=false,events=false,config=true,managed=true}={}){
  const root=await realpath(await mkdtemp(join(tmpdir(),'snapshot247-'))),cwd=join(root,'project'),privateRoot=join(root,'private'),home=join(root,'synthetic-claude'),port=join(root,'port'),gatePath=join(root,'release');
  await mkdir(cwd,{mode:0o700});await mkdir(privateRoot,{mode:0o700});await mkdir(home,{mode:0o700});
  const agent='ag_snapshot_fixture',instance='snapshot-fixture-instance';
  const controller=await fakePrivateState(agent);
  const env=hermeticEnv({HOME:root,AGENT_UI_CWD:cwd,TREER_RUST_SPIKE_STATE_DIR:privateRoot,AGENT_UI_HARNESS:'claude',AGENT_UI_PUBLIC_DIR:resolve('../agent/public'),AGENT_UI_PORT:'0',AGENT_UI_PORT_FILE:port,AGENT_UI_INSTANCE_ID:instance,RUST_SPIKE_REGISTER:'0',CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-snapshot.mjs')]),SNAPSHOT_NATIVE_HOME:home,SNAPSHOT_CALLS:join(root,'calls')});
  delete env.CLAUDE_CONFIG_DIR;
  if(config)env.CLAUDE_CONFIG_DIR=home;
  if(managed)Object.assign(env,{TREER_AGENT_ID:agent,TREER_AGENT_SERVER_URL:controller.url,TREER_WORKLOAD_CREDENTIAL:'fixture-only',TREER_AGENT_PRIVATE_STATE_DIR:privateRoot,AGENT_UI_MESSAGE_WATCH:'0'});
  if(gate)env.RUST_SESSION_SNAPSHOT_GATE=gatePath;
  if(events)env.SNAPSHOT_AFTER_GATE=gatePath+'.entered';
  const child=spawn(binary,[],{cwd,env,stdio:['ignore','pipe','pipe']});let log='';child.stderr.on('data',b=>log+=b);child.stdout.on('data',b=>log+=b);
  t.after(async()=>{await writeFile(gatePath,'release').catch(()=>{});if(child.exitCode===null&&child.signalCode===null){const exited=once(child,'exit');child.kill('SIGTERM');await exited;}await controller.close();await rm(root,{recursive:true,force:true});});
  const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(port,'utf8'))}`,v=>!v.endsWith('NaN')).catch(error=>{throw Error(error.message+log);});
  const state=()=>fetch(base+'/api/state').then(r=>r.json());
  try{await until(state,s=>s.ready);}catch(error){throw Error(error.message+log);}
  const headers={'content-type':'application/json','x-treer-interface-source':'controller-api','x-treer-agent-id':agent,'x-treer-interface-instance':instance};
  const snap=(body,options={})=>fetch(base+'/api/session/export',{method:'POST',headers,body:JSON.stringify(body),...options});
  return {root,cwd,home,child,base,state,headers,snap,gatePath,env,log:()=>log,prompt:text=>fetch(base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:text,mode:'queue'})}),calls:async()=> (await readFile(join(root,'calls'),'utf8')).trim().split('\n').map(JSON.parse)};
}
test('managed idle snapshot: exact provenance/thread, allowlist bundle and offline atomic import',async t=>{
  const f=await fixture(t);const before=await f.state(),threadId=before.thread.id;
  assert.equal(before.harness.capabilities.sessionTransfer,null);
  for(const headers of [{},{...f.headers,'x-treer-agent-id':'wrong'},{...f.headers,'x-treer-interface-instance':'stale'},{...f.headers,origin:'https://untrusted.invalid'}])assert.equal((await f.snap({threadId},{headers})).status,403);
  assert.equal((await f.snap({threadId},{method:'GET',body:undefined})).status,405);
  for(const body of [{threadId,configDir:f.home},{threadId,cwd:f.cwd},{threadId,sessionId:threadId},{},null])assert.equal((await f.snap(body)).status,400);
  assert.equal((await f.snap({threadId:'stale'})).status,409);
  assert.equal((await f.snap({threadId},{body:'{"threadId":"a","threadId":"b"}'})).status,400);
  const response=await f.snap({threadId});assert.equal(response.status,200,await response.clone().text());assert.equal(response.headers.get('cache-control'),'no-store');
  const bundle=await response.json();assert.equal(bundle.schema,'treer.agent-session/v1');assert.equal(bundle.sessionId,threadId);assert.equal(bundle.runtimeSnapshot.agentId,'ag_snapshot_fixture');assert.equal(bundle.runtimeSnapshot.threadId,threadId);assert.equal(bundle.runtimeSnapshot.providerCheckpoint,false);
  for(const file of bundle.files)assert.equal(createHash('sha256').update(Buffer.from(file.content,'base64')).digest('hex'),file.sha256);
  const input=join(f.root,'snapshot.json'),destination=join(f.root,'new-project');await writeFile(input,JSON.stringify(bundle),{mode:0o600});
  const imported=spawnSync(binary,['session','import','--input',input,'--directory',destination],{env:hermeticEnv({HOME:f.root}),encoding:'utf8',timeout:10000});assert.equal(imported.status,0,imported.stderr);assert.notEqual(JSON.parse(imported.stdout).sessionId,threadId);
  assert.equal((await f.state()).thread.id,threadId);assert.equal((await f.calls()).filter(c=>c.text).length,0);
});
test('snapshot custody serializes concurrent prompt exactly once and bounds duplicate export',async t=>{
  const f=await fixture(t,{gate:true});const threadId=(await f.state()).thread.id;
  const exporting=f.snap({threadId});await until(()=>stat(f.gatePath+'.entered'),Boolean);
  assert.equal((await f.snap({threadId})).status,429);
  const prompt=f.prompt('queued while snapshot reads');
  await new Promise(r=>setTimeout(r,100));assert.equal((await f.calls()).filter(c=>c.text).length,0);
  await writeFile(f.gatePath,'release');const exported=await exporting;assert.equal(exported.status,200,await exported.clone().text());
  const bundle=await exported.json();assert.equal(bundle.files.some(e=>Buffer.from(e.content,'base64').includes(Buffer.from('queued while'))),false);
  assert.equal((await prompt).status,200);await until(f.state,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  assert.deepEqual((await f.calls()).filter(c=>c.text).map(c=>c.text),['queued while snapshot reads']);
  assert.equal((await f.state()).thread.id,threadId);
});
test('snapshot pumps provider burst and refuses changed window without killing/replaying',async t=>{
  const f=await fixture(t,{gate:true,events:true});const threadId=(await f.state()).thread.id;
  const exporting=f.snap({threadId});await until(f.state,s=>s.backgroundTasks.some(t=>t.id==='own-terminal-63'));
  assert.equal(f.child.exitCode,null);await writeFile(f.gatePath,'release');
  const response=await exporting;assert.equal(response.status,409);assert.equal((await response.json()).error.code,'snapshot_provider_changed');
  assert.equal((await f.prompt('after refused snapshot')).status,200);await until(f.state,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  assert.deepEqual((await f.calls()).filter(c=>c.text).map(c=>c.text),['after refused snapshot']);
  assert.equal((await f.state()).runtime.state,'idle');
});
test('busy/queued/task/config/unmanaged/source refusals disclose no bundle',async t=>{
  const f=await fixture(t);const threadId=(await f.state()).thread.id;
  await f.prompt('busy');await until(f.state,s=>s.thread.status==='running');
  assert.equal((await f.snap({threadId})).status,409);await f.prompt('background');await until(f.state,s=>s.thread.status==='idle'&&s.thread.turns.length===2);
  const activeTask=await f.snap({threadId});assert.equal(activeTask.status,409);assert.equal((await activeTask.json()).error.code,'snapshot_requires_idle_without_obligations');
  const missing=await fixture(t,{config:false});const noRoot=await missing.snap({threadId:(await missing.state()).thread.id});assert.equal(noRoot.status,409);assert.equal((await noRoot.json()).error.code,'snapshot_explicit_config_required');
  const unmanaged=await fixture(t,{managed:false});assert.equal((await unmanaged.snap({threadId:(await unmanaged.state()).thread.id})).status,403);
  const bad=await fixture(t);const id=(await bad.state()).thread.id,key=bad.cwd.replace(/[^a-zA-Z0-9]/g,'-'),file=join(bad.home,'projects',key,id+'.jsonl');
  await rename(file,file+'.original');await symlink(file+'.original',file);
  const refused=await bad.snap({threadId:id});assert.equal(refused.status,409);assert.equal((await refused.json()).error.code,'snapshot_source_refused');
  assert.equal((await bad.state()).runtime.state,'idle');assert.equal((await bad.calls()).filter(c=>c.text).length,0);
});
