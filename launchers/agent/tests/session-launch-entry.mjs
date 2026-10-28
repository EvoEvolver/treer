import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,realpath,mkdir,readFile,writeFile,rm,stat,copyFile,chmod} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {hermeticEnv,localFetch as fetch} from './hermetic.mjs';
import {fakePrivateState} from './fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
const sha=b=>createHash('sha256').update(b).digest('hex');
async function until(read,accept,ms=10000){const end=Date.now()+ms;while(Date.now()<end){const value=await read().catch(()=>null);if(value&&accept(value))return value;await new Promise(r=>setTimeout(r,15));}throw Error('owned launch fixture deadline');}
function clean(extra){const env=hermeticEnv();for(const key of Object.keys(env))if(/^(CLAUDE_|ANTHROPIC_|LAUNCH_|SNAPSHOT_|RUST_SPIKE_|RUST_SESSION_)/.test(key))delete env[key];return {...env,...extra};}
async function source(t){
  const root=await realpath(await mkdtemp(join(tmpdir(),'launch247-'))),cwd=join(root,'source'),home=join(root,'synthetic-config');await mkdir(cwd,{mode:0o700});await mkdir(home,{mode:0o700});
  const controller=await fakePrivateState('ag_launch_fixture');const children=[];
  t.after(async()=>{for(const child of children){if(child.exitCode===null&&child.signalCode===null){const done=once(child,'exit');child.kill('SIGTERM');await done;}}await controller.close();await rm(root,{recursive:true,force:true});});
  function environment(privateRoot,port,calls){return clean({HOME:root,AGENT_UI_CWD:cwd,AGENT_UI_HARNESS:'claude',CLAUDE_CONFIG_DIR:home,AGENT_UI_PUBLIC_DIR:resolve('public'),AGENT_UI_PORT:'0',AGENT_UI_PORT_FILE:port,TREER_AGENT_ID:'ag_launch_fixture',TREER_AGENT_PRIVATE_STATE_DIR:privateRoot,TREER_AGENT_SERVER_URL:controller.url,TREER_WORKLOAD_CREDENTIAL:'fixture-only',AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'launch-fixture',RUST_SPIKE_REGISTER:'0',CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-launch.mjs')]),LAUNCH_CALLS:calls,ANTHROPIC_API_KEY:'synthetic-launch-token'});}
  async function start(args,env,entry=binary,options={}){const i=args.indexOf('--directory');const child=spawn(entry,args,{env,...(args.includes('run')&&i>=0?{cwd:args[i+1]}:{}),...options,stdio:['ignore','pipe','pipe']});children.push(child);let log='';child.stdout.on('data',b=>log+=b);child.stderr.on('data',b=>log+=b);return {child,log:()=>log};}
  async function running(args,env,entry=binary){const p=await start(args,env,entry);const base=await until(async()=>`http://127.0.0.1:${Number(await readFile(env.AGENT_UI_PORT_FILE,'utf8'))}`,v=>!v.endsWith('NaN')).catch(error=>{throw Error(error.message+p.log());});const state=()=>fetch(base+'/api/state').then(r=>r.json());await until(state,s=>s.ready);return {...p,base,state};}
  const privateRoot=join(root,'source-private');await mkdir(privateRoot,{mode:0o700});const calls=join(root,'source-calls');
  const live=await running([],environment(privateRoot,join(root,'source-port'),calls));
  assert.equal((await fetch(live.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'original synthetic context',mode:'queue'})})).status,200);
  await until(live.state,s=>s.thread.status==='idle'&&s.thread.turns.length===1);
  const id=(await live.state()).thread.id,headers={'content-type':'application/json','x-treer-agent-id':'ag_launch_fixture','x-treer-interface-instance':'launch-fixture','x-treer-interface-source':'controller-api'};
  const snapshot=await fetch(live.base+'/api/session/export',{method:'POST',headers,body:JSON.stringify({threadId:id})});assert.equal(snapshot.status,200,await snapshot.clone().text());
  const bundle=await snapshot.json(),input=join(root,'snapshot.json');await writeFile(input,JSON.stringify(bundle),{mode:0o600});
  const native=join(home,'projects',cwd.replace(/[^a-zA-Z0-9]/g,'-'),id+'.jsonl'),original=sha(await readFile(native));
  function prepare(name='target',file=input){const destination=join(root,name);const result=spawnSync(binary,['session','prepare','--input',file,'--directory',destination],{env:clean({HOME:root}),encoding:'utf8',timeout:10000});return {destination,result,imported:result.status===0?JSON.parse(result.stdout):null};}
  async function target(name='target'){const p=prepare(name);assert.equal(p.result.status,0,p.result.stderr);const stateRoot=join(root,name+'-private');await mkdir(stateRoot,{mode:0o700});const env=environment(stateRoot,join(root,name+'-port'),join(root,name+'-calls'));return {...p,env,stateRoot,args:['session','launch','--directory',p.destination,'--session-id',p.imported.sessionId,'--auth','environment']};}
  return {root,id,live,calls,native,original,bundle,input,prepare,target,start,running,environment,checkSource:async()=>{assert.equal(sha(await readFile(native)),original);assert.equal((await live.state()).thread.id,id);assert.equal((await readFile(calls,'utf8')).trim().split('\n').map(JSON.parse).filter(e=>e.text).length,1);}};
}
test('managed snapshot -> atomic prepare -> explicit authenticated-config launch -> prompt once and normal restart',async t=>{
  const f=await source(t),p=await f.target(),live=await f.running(p.args,p.env);
  const state=await live.state();assert.notEqual(state.thread.id,f.id);assert.equal(state.thread.id,p.imported.sessionId);assert.equal(state.harness.capabilities.sessionTransfer,null);
  assert.ok(state.thread.turns.some(turn=>turn.projectionOnly===true&&turn.items.some(item=>item.text.includes('original synthetic context'))));
  let calls=(await readFile(p.env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);assert.equal(calls.filter(e=>e.text).length,0);assert.ok(calls.find(e=>e.spawn&&e.resume&&e.context&&e.auth&&e.cwd===p.destination&&e.home===p.imported.environment.CLAUDE_CONFIG_DIR));
  const checkpoint=JSON.parse(await readFile(join(p.stateRoot,'rust-spike.json'),'utf8'));assert.deepEqual(checkpoint.records,[]);assert.equal(checkpoint.claudeNativeHistory.providerConfirmed,false);
  assert.equal((await fetch(live.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'continue imported context',mode:'queue'})})).status,200);await until(live.state,s=>s.thread.status==='idle'&&s.thread.turns.some(t=>t.items.some(i=>i.text==='continue imported context')));
  const exit=once(live.child,'exit');live.child.kill('SIGTERM');await exit;
  const env={...p.env,AGENT_UI_CWD:p.destination,AGENT_UI_SESSION_ID:p.imported.sessionId,CLAUDE_CONFIG_DIR:p.imported.environment.CLAUDE_CONFIG_DIR,AGENT_UI_PORT_FILE:join(f.root,'restart-port')};
  const restarted=await f.running([],env);assert.equal((await restarted.state()).thread.id,p.imported.sessionId);
  calls=(await readFile(p.env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);assert.deepEqual(calls.filter(e=>e.text).map(e=>e.text),['continue imported context']);await f.checkSource();
});

test('orchestrated snapshot inspect/hash prepare and persistent target ready preserve source, prompt once',async t=>{
  const f=await source(t),destination=join(f.root,'orchestrated'),sessionId=crypto.randomUUID();
  // Model the ordinary upload boundary using only task-owned bytes/directories.
  // No shell, snapshot content, authentication value or provider control in argv.
  const upload=join(f.root,'uploaded-snapshot.json'),bytes=await readFile(f.input);
  await writeFile(upload,bytes,{flag:'wx',mode:0o600});
  const options=['--input',upload,'--directory',destination,'--session-id',sessionId,'--sha256',sha(bytes)];
  function operation(op){return spawnSync(binary,['session',op,...options],{env:clean({HOME:f.root}),encoding:'utf8',timeout:10000});}
  const inspected=operation('inspect');assert.equal(inspected.status,0,inspected.stderr);
  const validation=JSON.parse(inspected.stdout);assert.equal(validation.phase,'validated');await assert.rejects(stat(destination));
  const prepared=operation('prepare');assert.equal(prepared.status,0,prepared.stderr);const receipt=JSON.parse(prepared.stdout);
  assert.deepEqual(receipt,{...validation,phase:'prepared'});
  const privateRoot=join(f.root,'orchestrated-private');await mkdir(privateRoot,{mode:0o700});
  const env=f.environment(privateRoot,join(f.root,'orchestrated-port'),join(f.root,'orchestrated-calls'));
  const startup=['session','run','--directory',receipt.cwd,'--session-id',receipt.sessionId,'--auth','environment'];
  const target=await f.running(startup,env),state=await target.state();
  assert.equal(state.ready,true);assert.equal(state.thread.id,sessionId);assert.notEqual(sessionId,f.id);
  assert.equal(state.harness.capabilities.sessionTransfer,null);
  assert.equal((await fetch(target.base+'/api/health').then(r=>r.json())).ready,true);
  assert.equal((await fetch(target.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'explicit new target input',mode:'queue'})})).status,200);
  await until(target.state,s=>s.thread.status==='idle'&&s.thread.turns.some(turn=>turn.items.some(item=>item.text==='explicit new target input')));
  const calls=(await readFile(env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.filter(c=>c.text).length,1);assert.equal(calls.find(c=>c.spawn).session,sessionId);assert.equal(calls.find(c=>c.spawn).resume,true);
  await f.checkSource();
});
test('prepare validation and launch auth/hash/private-root refusals precede provider/publication',async t=>{
  const f=await source(t);const bad=structuredClone(f.bundle);bad.files[0].sha256='0'.repeat(64);const file=join(f.root,'bad.json');await writeFile(file,JSON.stringify(bad),{mode:0o600});const refused=f.prepare('invalid',file);assert.notEqual(refused.result.status,0);await assert.rejects(stat(refused.destination));
  const p=await f.target();
  async function refuses(env=p.env,args=p.args){const process=await f.start(args,env);await once(process.child,'exit');assert.notEqual(process.child.exitCode,0,process.log());await assert.rejects(stat(env.AGENT_UI_PORT_FILE));await assert.rejects(stat(join(p.stateRoot,'rust-spike.json')));await assert.rejects(stat(p.env.LAUNCH_CALLS));}
  const noAuth={...p.env};delete noAuth.ANTHROPIC_API_KEY;await refuses(noAuth);
  await writeFile(join(p.stateRoot,'existing-state'),'preserve',{mode:0o600});await refuses();assert.equal(await readFile(join(p.stateRoot,'existing-state'),'utf8'),'preserve');await assert.rejects(stat(join(p.destination,'.harness/launch-intent.json')));
  const q=await f.target('tampered');const path=join(q.imported.environment.CLAUDE_CONFIG_DIR,'projects',q.destination.replace(/[^a-zA-Z0-9]/g,'-'),q.imported.sessionId+'.jsonl');await writeFile(path,'{}\n',{mode:0o600});const process=await f.start(q.args,q.env);await once(process.child,'exit');assert.notEqual(process.child.exitCode,0);await assert.rejects(stat(q.env.LAUNCH_CALLS));await assert.rejects(stat(q.env.AGENT_UI_PORT_FILE));await f.checkSource();
});
test('failed initialize leaves no checkpoint/port or input; once intent prevents automatic reattempt',async t=>{
  const f=await source(t),p=await f.target();const failed=await f.start(p.args,{...p.env,LAUNCH_REFUSE_INIT:'1'});await once(failed.child,'exit');assert.notEqual(failed.child.exitCode,0,failed.log());
  await assert.rejects(stat(p.env.AGENT_UI_PORT_FILE));await assert.rejects(stat(join(p.stateRoot,'rust-spike.json')));assert.ok(await stat(join(p.destination,'.harness/launch-intent.json')));
  const before=await readFile(p.env.LAUNCH_CALLS,'utf8');assert.equal(before.trim().split('\n').map(JSON.parse).filter(e=>e.text).length,0);
  const fresh=join(f.root,'retry-private');await mkdir(fresh,{mode:0o700});const retry=await f.start(p.args,{...p.env,TREER_AGENT_PRIVATE_STATE_DIR:fresh});await once(retry.child,'exit');assert.notEqual(retry.child.exitCode,0);assert.equal(await readFile(p.env.LAUNCH_CALLS,'utf8'),before);await f.checkSource();
});
test('treer-launch session launch registers the same new native session',async t=>{
  const f=await source(t),p=await f.target();
  const bin=join(f.root,'synthetic-bin'),registered=join(f.root,'registered');await mkdir(bin,{mode:0o700});const cli=join(bin,'treer');await writeFile(cli,`#!${process.execPath}\nimport {writeFileSync} from 'node:fs'; if(process.argv[2]!=='interface'||process.argv[3]!=='register')process.exit(81);writeFileSync(process.env.LAUNCH_REGISTER_LOG,JSON.stringify(process.argv.slice(2)));`,{mode:0o700});
  const live=await f.running(p.args,{...p.env,PATH:bin+':'+p.env.PATH,LAUNCH_REGISTER_LOG:registered});assert.equal((await live.state()).thread.id,p.imported.sessionId);assert.equal((await live.state()).harness.capabilities.sessionTransfer,null);await until(()=>readFile(registered,'utf8'),v=>JSON.parse(v).includes('--instance-id'));await f.checkSource();
});

test('persistent prepared startup reuses exact argv/checkpoint after native growth, with no duplicate launch/input',async t=>{
  const f=await source(t),p=await f.target('persistent'),args=[...p.args];args[1]='run';
  const first=await f.running(args,p.env);assert.equal((await first.state()).thread.id,p.imported.sessionId);
  const intentPath=join(p.destination,'.harness/launch-intent.json'),intent=await readFile(intentPath,'utf8');
  const held=await f.start(args,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'concurrent-port')});await once(held.child,'exit');assert.notEqual(held.child.exitCode,0,held.log());
  assert.equal((await fetch(first.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'persistent continuation exactly once',mode:'queue'})})).status,200);
  await until(first.state,s=>s.thread.status==='idle'&&s.thread.turns.some(t=>t.items.some(i=>i.text==='persistent continuation exactly once')));
  const records=JSON.parse(await readFile(join(p.stateRoot,'rust-spike.json'),'utf8')).records;
  let done=once(first.child,'exit');first.child.kill('SIGTERM');await done;
  // Same persisted startup args; fresh observation file belongs to this test invocation.
  const second=await f.running(args,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'persistent-second-port')});
  assert.equal((await second.state()).thread.id,p.imported.sessionId);
  assert.deepEqual(JSON.parse(await readFile(join(p.stateRoot,'rust-spike.json'),'utf8')).records,records);
  done=once(second.child,'exit');second.child.kill('SIGTERM');await done;
  const third=await f.running(args,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'persistent-third-port')});assert.equal((await third.state()).thread.id,p.imported.sessionId);
  assert.equal(await readFile(intentPath,'utf8'),intent);
  const calls=(await readFile(p.env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.filter(e=>e.spawn).length,3);assert.ok(calls.filter(e=>e.spawn).every(e=>e.resume&&e.session===p.imported.sessionId&&e.home===p.imported.environment.CLAUDE_CONFIG_DIR));
  assert.deepEqual(calls.filter(e=>e.text).map(e=>e.text),['persistent continuation exactly once']);await f.checkSource();
});
test('persistent failed initialization cannot bootstrap again or publish interface/checkpoint',async t=>{
  const f=await source(t),p=await f.target('persistent-refused'),args=[...p.args];args[1]='run';
  const first=await f.start(args,{...p.env,LAUNCH_REFUSE_INIT:'1'});await once(first.child,'exit');assert.notEqual(first.child.exitCode,0);
  await assert.rejects(stat(p.env.AGENT_UI_PORT_FILE));await assert.rejects(stat(join(p.stateRoot,'rust-spike.json')));
  const before=await readFile(p.env.LAUNCH_CALLS,'utf8'),intent=await readFile(join(p.destination,'.harness/launch-intent.json'),'utf8');
  const retry=await f.start(args,p.env);await once(retry.child,'exit');assert.notEqual(retry.child.exitCode,0,retry.log());
  assert.equal(await readFile(p.env.LAUNCH_CALLS,'utf8'),before);assert.equal(await readFile(join(p.destination,'.harness/launch-intent.json'),'utf8'),intent);
  await assert.rejects(stat(p.env.AGENT_UI_PORT_FILE));await assert.rejects(stat(join(p.stateRoot,'rust-spike.json')));await f.checkSource();
});
test('persistent startup refuses copied root, wrong session and deleted checkpoint before any new provider',async t=>{
  const f=await source(t),p=await f.target('persistent-binding'),args=[...p.args];args[1]='run';
  const misplaced=await f.start(args,p.env,binary,{cwd:f.root});await once(misplaced.child,'exit');assert.notEqual(misplaced.child.exitCode,0);assert.match(misplaced.log(),/matching_process_cwd/);await assert.rejects(stat(p.env.LAUNCH_CALLS));await assert.rejects(stat(join(p.destination,'.harness/launch-intent.json')));
  const first=await f.running(args,p.env);const done=once(first.child,'exit');first.child.kill('SIGTERM');await done;
  const before=await readFile(p.env.LAUNCH_CALLS,'utf8'),checkpoint=join(p.stateRoot,'rust-spike.json');
  const copied=join(f.root,'copied-private');await mkdir(copied,{mode:0o700});await copyFile(checkpoint,join(copied,'rust-spike.json'));
  async function refuses(argv,env){const p=await f.start(argv,env);await once(p.child,'exit');assert.notEqual(p.child.exitCode,0,p.log());assert.equal(await readFile(env.LAUNCH_CALLS,'utf8'),before);await assert.rejects(stat(env.AGENT_UI_PORT_FILE));}
  await refuses(args,{...p.env,TREER_AGENT_PRIVATE_STATE_DIR:copied,AGENT_UI_PORT_FILE:join(f.root,'copied-port')});
  const wrong=[...args];wrong[5]='00000000-0000-4000-8000-000000000001';await refuses(wrong,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'wrong-port')});
  await rm(checkpoint);await refuses(args,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'deleted-port')});await f.checkSource();
});
test('persistent treer-launch session run registers only initialized target and ignores inherited port observation',async t=>{
  const f=await source(t),p=await f.target('persistent-entry');
  const bin=join(f.root,'persistent-bin'),registered=join(f.root,'persistent-registration');await mkdir(bin,{mode:0o700});
  await writeFile(join(bin,'treer'),`#!${process.execPath}\nimport {appendFileSync} from 'node:fs'; if(process.argv[2]!=='interface'||process.argv[3]!=='register')process.exit(81);appendFileSync(process.env.LAUNCH_REGISTER_LOG,JSON.stringify(process.argv.slice(2))+'\\n',{mode:0o600});`,{mode:0o700});
  const args=[...p.args];args[1]='run';
  const env={...p.env,PATH:bin+':'+p.env.PATH,LAUNCH_REGISTER_LOG:registered};await writeFile(env.AGENT_UI_PORT_FILE,'parent-owned-observation',{mode:0o600});
  const q=await f.target('persistent-entry-refused'),refusedArgs=[...q.args];refusedArgs[1]='run';const refusedRegistration=join(f.root,'refused-registration');
  const refused=await f.start(refusedArgs,{...q.env,PATH:env.PATH,LAUNCH_REGISTER_LOG:refusedRegistration,LAUNCH_REFUSE_INIT:'1'});
  await once(refused.child,'exit');assert.notEqual(refused.child.exitCode,0,refused.log());await assert.rejects(stat(refusedRegistration));await assert.rejects(stat(join(q.stateRoot,'rust-spike.json')));
  async function started(){const owned=await f.start(args,env);const events=await until(()=>readFile(registered,'utf8'),s=>s.trim().split('\n').length===count+1);count++;const argv=JSON.parse(events.trim().split('\n').at(-1)),port=argv[argv.indexOf('--port')+1];const state=await until(()=>fetch(`http://127.0.0.1:${port}/api/state`).then(r=>r.json()),s=>s.ready);assert.equal(state.thread.id,p.imported.sessionId);assert.equal(state.harness.capabilities.sessionTransfer,null);return owned;}
  let count=0;const first=await started();let done=once(first.child,'exit');first.child.kill('SIGTERM');await done;
  const second=await started();done=once(second.child,'exit');second.child.kill('SIGTERM');await done;
  assert.equal(await readFile(env.AGENT_UI_PORT_FILE,'utf8'),'parent-owned-observation');
  const calls=(await readFile(env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);assert.equal(calls.filter(e=>e.spawn).length,2);assert.equal(calls.filter(e=>e.text).length,0);await f.checkSource();
});
test('persistent checkpoint recovery keeps uncertain receipt fenced and AIS reachable without replay',async t=>{
  const f=await source(t),p=await f.target('persistent-unknown'),args=[...p.args];args[1]='run';
  const first=await f.running(args,p.env);
  assert.equal((await fetch(first.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'owned uncertain-state control',mode:'queue'})})).status,200);
  await until(first.state,s=>s.thread.status==='idle'&&s.thread.turns.some(t=>t.items.some(i=>i.text==='owned uncertain-state control')));
  const done=once(first.child,'exit');first.child.kill('SIGTERM');await done;
  // Task-owned checkpoint only: emulate a persisted ambiguous dispatch, never
  // write a real session or manufacture fresh provider terminal authority.
  const path=join(p.stateRoot,'rust-spike.json'),saved=JSON.parse(await readFile(path,'utf8'));
  saved.records[0].submission.state='failed';saved.records[0].submission.error='dispatch outcome uncertain; synthetic persisted-state control';saved.dispatchBlocked=true;
  await writeFile(path,JSON.stringify(saved),{mode:0o600});
  const recovered=await f.running(args,{...p.env,AGENT_UI_PORT_FILE:join(f.root,'unknown-recovered-port')});
  const state=await recovered.state();assert.equal(state.runtime.state,'blocked');assert.equal(recovered.child.exitCode,null);
  const retained=JSON.parse(await readFile(path,'utf8'));assert.equal(retained.records[0].submission.id,saved.records[0].submission.id);assert.match(retained.records[0].submission.error,/dispatch outcome uncertain/);
  const calls=(await readFile(p.env.LAUNCH_CALLS,'utf8')).trim().split('\n').map(JSON.parse);assert.deepEqual(calls.filter(e=>e.text).map(e=>e.text),['owned uncertain-state control']);await f.checkSource();
});
