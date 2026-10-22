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
  const f=await fixture(t);let state=await f.snapshot();assert.equal(state.harness.id,'claude');assert.equal(state.harness.capabilities.steer,false);
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
