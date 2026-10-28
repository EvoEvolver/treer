import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp,realpath,mkdir,readFile,writeFile,rm,stat,readdir,symlink,rename} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {createHash,randomUUID} from 'node:crypto';
import {hermeticEnv,localFetch as fetch} from './hermetic.mjs';
import {fakePrivateState} from './fake-private-state.mjs';
const binary=process.env.RUST_SPIKE_BIN;assert.ok(binary);
const sha=b=>createHash('sha256').update(b).digest('hex'),key=p=>p.replace(/[^a-zA-Z0-9]/g,'-');
async function until(read,accept,ms=12000){const end=Date.now()+ms;while(Date.now()<end){const v=await read().catch(()=>null);if(v&&accept(v))return v;await new Promise(r=>setTimeout(r,15));}throw Error('owner-default fixture deadline');}
async function setup(t){
 const root=await realpath(await mkdtemp(join(tmpdir(),'owner247-'))),home=join(root,'owner'),cwd=join(root,'source');
 await mkdir(home,{mode:0o700});await mkdir(cwd,{mode:0o700});await mkdir(join(home,'.claude','projects'),{recursive:true,mode:0o700});
 // Sentinel names model sensitive files. Neither importer nor fixture reads
 // these; unchanged hashes only test that publication leaves them untouched.
 await writeFile(join(home,'.claude','settings.json'),'synthetic settings',{mode:0o600});
 const controller=await fakePrivateState('ag_owner_fixture'),children=[];
 t.after(async()=>{for(const c of children)if(c.exitCode===null&&c.signalCode===null){const exit=once(c,'exit');c.kill('SIGTERM');await exit;}await controller.close();await rm(root,{recursive:true,force:true});});
 const env=hermeticEnv();for(const k of Object.keys(env))if(/^(CLAUDE_|ANTHROPIC_|AGENT_UI_|RUST_SPIKE_|RUST_SESSION_|LAUNCH_)/.test(k))delete env[k];
 Object.assign(env,{HOME:home,AGENT_UI_CWD:cwd,AGENT_UI_HARNESS:'claude',AGENT_UI_PUBLIC_DIR:resolve('public'),AGENT_UI_PORT:'0',TREER_AGENT_ID:'ag_owner_fixture',TREER_AGENT_SERVER_URL:controller.url,TREER_WORKLOAD_CREDENTIAL:'fixture-only',AGENT_UI_MESSAGE_WATCH:'0',AGENT_UI_INSTANCE_ID:'owner-fixture',RUST_SPIKE_REGISTER:'0',CLAUDE_BIN:process.execPath,RUST_SPIKE_CLAUDE_ARGS:JSON.stringify([resolve('tests/fixtures/claude-launch.mjs')])});
 async function runtime(name,args=[],extra={}){
  const privateRoot=join(root,name+'-private');await mkdir(privateRoot,{mode:0o700}).catch(e=>{if(e.code!=='EEXIST')throw e;});
  const port=join(root,name+'-port-'+randomUUID()),calls=join(root,name+'-calls');
  const e={...env,TREER_AGENT_PRIVATE_STATE_DIR:privateRoot,AGENT_UI_PORT_FILE:port,LAUNCH_CALLS:calls,...extra};
  const i=args.indexOf('--directory'),c=spawn(binary,args,{env:e,cwd:i>=0?args[i+1]:cwd,stdio:['ignore','pipe','pipe']});children.push(c);let log='';c.stderr.on('data',b=>log+=b);c.stdout.on('data',b=>log+=b);
  return {c,e,calls,privateRoot,log:()=>log,async ready(){const base=await until(()=>readFile(port,'utf8').then(p=>'http://127.0.0.1:'+Number(p)),Boolean).catch(e=>{throw Error(e.message+log);});const state=()=>fetch(base+'/api/state').then(r=>r.json());await until(state,s=>s.ready);return {base,state};}};
 }
 const source=await runtime('source'),live=await source.ready();
 await fetch(live.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'original synthetic context',mode:'queue'})});
 const state=await until(live.state,s=>s.thread.status==='idle'&&s.thread.turns.length===1),id=state.thread.id;
 const headers={'content-type':'application/json','x-treer-agent-id':'ag_owner_fixture','x-treer-interface-instance':'owner-fixture','x-treer-interface-source':'controller-api'};
 assert.equal((await fetch(live.base+'/api/session/export',{method:'POST',headers,body:JSON.stringify({threadId:id})})).status,409);
 const r=await fetch(live.base+'/api/session/export',{method:'POST',headers,body:JSON.stringify({threadId:id,auth:'owner-default'})});assert.equal(r.status,200,await r.clone().text());
 const bytes=Buffer.from(await r.text()),bundle=JSON.parse(bytes);assert.equal(bundle.runtimeSnapshot.auth,'owner-default');
 const input=join(root,'snapshot.json');await writeFile(input,bytes,{mode:0o600});
 const native=join(home,'.claude','projects',key(cwd),id+'.jsonl'),original=sha(await readFile(native));
 const target=join(root,'target'),newId=randomUUID(),opts=['--input',input,'--directory',target,'--session-id',newId,'--sha256',sha(bytes),'--auth','owner-default'];
 const cli=(op,options=opts,extra={})=>spawnSync(binary,['session',op,...options],{env:{...env,...extra},encoding:'utf8',timeout:10000});
 const launch=['session','run','--directory',target,'--session-id',newId,'--auth','owner-default'];
 return {root,home,cwd,env,source,live,id,newId,input,bytes,target,opts,cli,launch,runtime,async sourceUnchanged(){assert.equal(sha(await readFile(native)),original);assert.equal((await live.state()).thread.id,id);assert.equal(await readFile(join(home,'.claude','settings.json'),'utf8'),'synthetic settings');}};
}
test('owner-default actual snapshot -> inspect -> dual prepare -> persistent exact session, next input once',async t=>{
 const f=await setup(t),i=f.cli('inspect');assert.equal(i.status,0,i.stderr);const inspected=JSON.parse(i.stdout);assert.equal(inspected.auth,'owner-default');assert.deepEqual(inspected.environment,{});await assert.rejects(stat(f.target));
 const p=f.cli('prepare');assert.equal(p.status,0,p.stderr);const receipt=JSON.parse(p.stdout);assert.deepEqual(receipt,{...inspected,phase:'prepared'});
 const statusOpts=['--directory',f.target,'--session-id',f.newId,'--auth','owner-default'];
 const pub=f.cli('publication',statusOpts);assert.equal(pub.status,0,pub.stderr);assert.equal(JSON.parse(pub.stdout).state,'prepared');
 assert.notEqual(f.cli('prepare').status,0); // No retry/publication overwrite.
 const target=await f.runtime('target',f.launch),live=await target.ready(),s=await live.state();assert.equal(s.thread.id,f.newId);assert.equal(s.harness.capabilities.sessionTransfer,null);assert.ok(s.thread.turns.some(t=>t.projectionOnly));
 let calls=(await readFile(target.calls,'utf8')).trim().split('\n').map(JSON.parse);assert.ok(calls.find(c=>c.spawn&&c.resume&&c.context&&c.home===join(f.home,'.claude')&&c.auth===false));
 await fetch(live.base+'/api/prompt',{method:'POST',body:JSON.stringify({prompt:'new explicit input',mode:'queue'})});await until(live.state,s=>s.thread.status==='idle'&&s.queue.length===0&&s.thread.turns.some(t=>t.items.some(i=>i.text==='new explicit input')));
 const exit=once(target.c,'exit');target.c.kill('SIGTERM');await exit;
 const restart=await f.runtime('target',f.launch),again=await restart.ready();assert.equal((await again.state()).thread.id,f.newId);
 calls=(await readFile(target.calls,'utf8')).trim().split('\n').map(JSON.parse);assert.deepEqual(calls.filter(c=>c.text).map(c=>c.text),['new explicit input']);assert.equal(calls.filter(c=>c.spawn).length,2);
 assert.equal(JSON.parse(f.cli('publication',statusOpts).stdout).state,'launch_claimed');await f.sourceUnchanged();
});
test('owner-default hash/path/occupied native project refuse before partial publication',async t=>{
 const f=await setup(t),bad=[...f.opts];bad[bad.indexOf('--sha256')+1]='0'.repeat(64);assert.notEqual(f.cli('prepare',bad).status,0);await assert.rejects(stat(f.target));
 const project=join(f.home,'.claude','projects',key(f.target));await mkdir(project,{mode:0o700});await writeFile(join(project,'sentinel'),'preserve',{mode:0o600});assert.notEqual(f.cli('prepare').status,0);assert.equal(await readFile(join(project,'sentinel'),'utf8'),'preserve');await assert.rejects(stat(f.target));
 await rm(project,{recursive:true});await symlink(f.cwd,project);assert.notEqual(f.cli('prepare').status,0);await assert.rejects(stat(f.target));
 assert.notEqual(f.cli('prepare',f.opts,{CLAUDE_CONFIG_DIR:f.cwd}).status,0);await f.sourceUnchanged();
 const isolated=await f.runtime('isolated',[],{CLAUDE_CONFIG_DIR:join(f.root,'isolated-config')}),s=await isolated.ready(),state=await s.state();
 const headers={'content-type':'application/json','x-treer-agent-id':'ag_owner_fixture','x-treer-interface-instance':'owner-fixture','x-treer-interface-source':'controller-api'};
 const refused=await fetch(s.base+'/api/session/export',{method:'POST',headers,body:JSON.stringify({threadId:state.thread.id,auth:'owner-default'})});assert.equal(refused.status,409);assert.match(await refused.text(),/snapshot_explicit_config_required/);
});
test('owner-default after-provider failure retains explicit partial intent and refuses repeat',async t=>{
 const f=await setup(t),p=f.cli('prepare',f.opts,{RUST_SESSION_OWNER_FAIL_AFTER_PROVIDER:'1'});assert.notEqual(p.status,0);assert.match(p.stderr,/publication_unknown_do_not_retry/);await assert.rejects(stat(f.target));
 const project=join(f.home,'.claude','projects',key(f.target));assert.ok(await stat(join(project,f.newId+'.jsonl')));
 const opts=['--directory',f.target,'--session-id',f.newId,'--auth','owner-default'],status=f.cli('publication',opts);assert.equal(status.status,0,status.stderr);const v=JSON.parse(status.stdout);assert.equal(v.state,'partial_or_unknown_do_not_retry');assert.equal(v.targetPublished,false);assert.equal(v.providerPublished,true);
 const before=await readdir(join(f.home,'.claude','projects'));assert.notEqual(f.cli('prepare').status,0);assert.deepEqual(await readdir(join(f.home,'.claude','projects')),before);await f.sourceUnchanged();
});
test('owner-default failed initialize retains exact once intent, no checkpoint/input/rebootstrap',async t=>{
 const f=await setup(t),p=f.cli('prepare');assert.equal(p.status,0,p.stderr);
 const failed=await f.runtime('failed',f.launch,{LAUNCH_REFUSE_INIT:'1'});await once(failed.c,'exit');assert.notEqual(failed.c.exitCode,0,failed.log());await assert.rejects(stat(join(failed.privateRoot,'rust-spike.json')));
 const calls=await readFile(failed.calls,'utf8');assert.equal(calls.trim().split('\n').map(JSON.parse).filter(c=>c.text).length,0);
 const retry=await f.runtime('failed',f.launch);await once(retry.c,'exit');assert.notEqual(retry.c.exitCode,0);assert.equal(await readFile(failed.calls,'utf8'),calls);await f.sourceUnchanged();
});
test('owner-default canonical existing config alias stays bound and provider uses normal default path',async t=>{
 const f=await setup(t),original=join(f.home,'.claude'),physical=join(f.home,'owner-native-config');
 await rename(original,physical);await symlink(physical,original);
 const i=f.cli('inspect');assert.equal(i.status,0,i.stderr);const inspected=JSON.parse(i.stdout);assert.equal(inspected.ownerProject,join(physical,'projects',key(f.target)));
 const p=f.cli('prepare');assert.equal(p.status,0,p.stderr);assert.deepEqual(JSON.parse(p.stdout),{...inspected,phase:'prepared'});
 const target=await f.runtime('alias',f.launch),live=await target.ready();assert.equal((await live.state()).thread.id,f.newId);
 const calls=(await readFile(target.calls,'utf8')).trim().split('\n').map(JSON.parse);assert.ok(calls.find(c=>c.spawn&&c.home===original&&c.context&&c.resume));await f.sourceUnchanged();
});
