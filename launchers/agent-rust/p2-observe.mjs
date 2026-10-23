// #158 external observer. The third pair requires an explicit reviewed inventory.
// No Codex-home reads. Human checks gate round1 and each six-round block.
import {execFile, spawn} from 'node:child_process';
import {promisify} from 'node:util';
import {lstat, open, readFile} from 'node:fs/promises';
import {request} from 'node:http';
import {createInterface} from 'node:readline';

const exec = promisify(execFile);
const treer = '/Users/dooms/.local/bin/treer';
const output = '/Users/dooms/.cache/treer-158-tUql8b8E/third-run.jsonl';
const end = Date.parse('2026-10-17T23:59:30Z');
const inventory = '/Users/dooms/.cache/treer-158-tUql8b8E/third-inventory.json';
const inventoryStat = await lstat(inventory);
if(!inventoryStat.isFile()||inventoryStat.isSymbolicLink()||(inventoryStat.mode&0o077))throw Error('unsafe_inventory');
const agents=JSON.parse(await readFile(inventory,'utf8'));
if(agents.length!==2||agents[0].name!=='node'||agents[1].name!=='rust'||agents.some(a=>!/^ag_[a-f0-9]{32}$/.test(a.id)||a.processes?.length!==(a.name==='node'?4:3)||a.processes.some(p=>!['launcher','provider','mcp'].includes(p.role)||!Number.isInteger(p.pid)||!p.startTime)))throw Error('invalid_inventory');
const run = async(args, timeout=10000) => JSON.parse((await exec(treer,args,{timeout,maxBuffer:1048576})).stdout);
const sleep = ms => new Promise(resolve=>setTimeout(resolve,ms));
let file, stopping=false, stopPromise, round=2, nextRound=Date.now()+60000, inFlight;
let approvedThrough=6, boundaryNotified=0;
const write = async value => {
  const line=JSON.stringify({at:new Date().toISOString(),...value});
  if(file){await file.write(line+'\n');await file.sync();}
  console.log(line);
};
async function notify(reason){
  const body=`#158 third-run observer ${reason}. Completed scripted round ${round-1}; evidence ${output}. ${reason.startsWith('check_block_')?'Submissions paused for human transcript check; process sampling/deadline remain active.':'Both exact experimental IDs stop requested; leader verify exact PID+start-time exits (including detached MCP), rehash artifacts, preserve evidence and teardown per P2-CREATE/P2-RUN.'} No Codex-home cleanup.`;
  for(const to of ['/treer-dev-team/leader','/treer-dev-team/launcher-dev']){
    await new Promise((resolve,reject)=>{
      const child=spawn(treer,['message','send','--to',to,'--idempotency-key',`158-third-observer-${to.split('/').at(-1)}-${reason}`],{stdio:['pipe','ignore','ignore']});
      const timer=setTimeout(()=>{child.kill();reject(Error('notify_timeout'));},10000);
      child.on('error',()=>{clearTimeout(timer);reject(Error('notify_failed'));});
      child.on('exit',code=>{clearTimeout(timer);code===0?resolve():reject(Error('notify_failed'));});
      child.stdin.end(body);
    });
  }
}
function stop(reason){
  if(stopPromise)return stopPromise;
  stopping=true;
  stopPromise=(async()=>{
    const results=await Promise.allSettled(agents.map(a=>run(['agent','stop',a.id])));
    await write({type:'stop',reason,results:results.map(r=>r.status),recordedProcesses:agents.map(a=>({name:a.name,processes:a.processes}))});
    // No automatic kill: the operator revalidates PID+start time individually.
    try{
      const recorded=new Set(agents.flatMap(a=>a.processes.map(p=>p.pid)));
      await write({type:'post_stop_inventory',processes:(await processSnapshot()).filter(p=>recorded.has(p.pid))});
    }catch{await write({type:'post_stop_inventory_failed'});}
    const failed=results.some(r=>r.status==='rejected');
    try{await notify(failed?`STOP_FAILED_OPERATOR_RETRY_VERIFY_BOTH_EXITED_${reason}`:reason);}catch{await write({type:'notification_failed'});}
  })();
  return stopPromise;
}
// Native loopback client, with no browser Sec-Fetch metadata synthesized by fetch.
function stateAt(port){
  return new Promise((resolve,reject)=>{
    const req=request({host:'127.0.0.1',port,path:'/api/state',method:'GET',signal:AbortSignal.timeout(5000)},res=>{
      const chunks=[];let bytes=0;
      res.on('data',chunk=>{bytes+=chunk.length;if(bytes>2097152){req.destroy(Error('state_bound'));return;}chunks.push(chunk);});
      res.on('error',reject);
      res.on('end',()=>{try{if(res.statusCode!==200)throw Error('state_http_failed');resolve(JSON.parse(Buffer.concat(chunks)));}catch(e){reject(e);}});
    });
    req.on('error',reject);req.end();
  });
}
function processRows(text){
  return text.trim().split('\n').filter(Boolean).map(line=>{
    const [pid,ppid,pgid,rss,cpu,...rest]=line.trim().split(/\s+/);
    const startTime=rest.slice(0,5).join(' '),comm=rest.slice(5).join(' ');
    return {pid:+pid,ppid:+ppid,pgid:+pgid,rssKiB:+rss,cpu,startTime,comm};
  });
}
async function processSnapshot(){
  return processRows((await exec('/bin/ps',['-axo','pid=,ppid=,pgid=,rss=,time=,lstart=,comm='],{timeout:5000,maxBuffer:2097152})).stdout);
}
async function sample(){
  const rows=await processSnapshot();
  const values=await Promise.all(agents.map(async a=>{
    const status=await run(['agent','show',a.id]);
    if(status.pid!==a.pid||status.run_generation!==1||!['idle','working'].includes(status.status)||status.interface?.instance_id!==a.instance)
      throw Error('generation_or_lifecycle_changed');
    const state=await stateAt(a.port);
    if(!state.ready||state.runtime?.error||!['idle','working'].includes(state.runtime?.state)||state.thread?.id!==a.thread)
      throw Error('runtime_or_thread_changed');
    if(state.thread.model!=='gpt-6.1-sol'||state.thread.reasoningEffort!=='medium')throw Error('model_changed');
    const turns=state.thread.turns??[], kinds={};
    for(const turn of turns){
      if(turn.error||['failed','error','interrupted'].includes(turn.status))throw Error('turn_failed');
      for(const item of turn.items??[]){
        kinds[item.kind]=(kinds[item.kind]??0)+1;
        if(!['userMessage','agentMessage','reasoning'].includes(item.kind))throw Error('unexpected_tool_item');
      }
    }
    const pids=new Set([a.pid]);
    let changed=true;
    while(changed){changed=false;for(const row of rows)if(pids.has(row.ppid)&&!pids.has(row.pid)){pids.add(row.pid);changed=true;}}
    const processes=rows.filter(r=>pids.has(r.pid));
    if(processes.length!==a.processes.length||a.processes.some(expected=>!processes.some(r=>r.pid===expected.pid&&r.ppid===expected.ppid&&r.pgid===expected.pgid&&r.startTime===expected.startTime&&r.comm===expected.comm)))
      throw Error('unexpected_process_tree');
    for(const process of processes)process.role=a.processes.find(p=>p.pid===process.pid).role;
    const rssKiB=Object.fromEntries(['launcher','provider','mcp'].map(role=>[role,processes.filter(p=>p.role===role).reduce((sum,p)=>sum+p.rssKiB,0)]));
    const journal=await lstat(a.journal);
    if(!journal.isFile()||journal.isSymbolicLink())throw Error('journal_type_changed');
    return {name:a.name,id:a.id,generation:status.run_generation,runtime:state.runtime.state,thread:a.thread,
      turnCount:turns.length,itemKinds:kinds,stateBytes:Buffer.byteLength(JSON.stringify(state)),journalBytes:journal.size,rssKiB,processes};
  }));
  await write({type:'sample',agents:values});
}
async function submitPair(n){
  await sample(); // Revalidate the full tree immediately before dispatch too.
  if(stopping)throw Error('stopping');
  const prompt=`Without using any tools, reading files, or making network requests, reply with exactly: experiment 158 round ${n} acknowledged.`;
  const started=Date.now();
  await write({type:'round_start',round:n});
  const settled=await Promise.allSettled(agents.map(async a=>{
    if(stopping||Date.now()>=end)throw Error('deadline');
    const began=Date.now();
    const result=await run(['agent','prompt',a.id,prompt,'--wait','--timeout','120000'],135000);
    if(result.completion?.state!=='done'||result.completion?.assistant_preview!==`experiment 158 round ${n} acknowledged.`)
      throw Error('unexpected_completion');
    return {name:a.name,submission:result.submission.id,turn:result.completion.turn_id,durationMs:Date.now()-began};
  }));
  if(settled.some(r=>r.status==='rejected'))throw Error('prompt_delivery_or_completion_failed');
  await write({type:'round_done',round:n,elapsedMs:Date.now()-started,results:settled.map(r=>r.value)});
  round=n+1;nextRound=Date.now()+60000;
}

if(process.argv.includes('--sample-only')){
  await sample();
}else{
  if(!process.argv.includes('--cadence-approved'))throw Error('explicit first-turn human approval required');
  if(Date.now()>=end-7200000)throw Error('less than two hours remain');
  file=await open(output,'wx',0o600);
  const input=createInterface({input:process.stdin});
  input.on('line',line=>{
    const match=/^approve-through (12|18|24)$/.exec(line.trim());
    const requested=Number(match?.[1]);
    if(match&&round===approvedThrough+1&&!inFlight&&requested===approvedThrough+6){
      approvedThrough=requested;
      void write({type:'human_block_approval',through:approvedThrough,actor:'/treer-dev-team/launcher-dev',basis:'leader transcript/tree approval on #158'});
    }
  });
  const deadline=setTimeout(()=>{void stop('deadline');},end-Date.now());
  for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>{void stop('operator_stop');});
  try{
    await write({type:'observer_start',end:new Date(end).toISOString(),round1:'manual_done_and_checked',pollMs:10000});
    while(!stopping){
      await sample();
      if(!stopping&&!inFlight&&round>approvedThrough&&boundaryNotified!==approvedThrough){
        boundaryNotified=approvedThrough;
        await write({type:'human_check_required',completed:approvedThrough});
        await notify(`check_block_${approvedThrough}`);
      }
      if(!stopping&&!inFlight&&round<=Math.min(24,approvedThrough)&&Date.now()>=nextRound){
        inFlight=submitPair(round).catch(()=>stop('prompt_failure')).finally(()=>{inFlight=undefined;});
      }
      await sleep(10000);
    }
  }catch{
    await stop('observation_fault');
  }finally{
    clearTimeout(deadline);
    input.close();
    if(inFlight)await inFlight;
    if(stopPromise)await stopPromise;
    await file.close();
  }
}
