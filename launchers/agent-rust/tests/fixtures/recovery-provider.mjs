#!/usr/bin/env node
// Synthetic provider, only used by fixture-harness actual-entry tests.
import {createInterface} from 'node:readline';
import {readFileSync,writeFileSync,appendFileSync} from 'node:fs';
const file='recovery-provider.json';
appendFileSync('recovery-provider-pids.jsonl',JSON.stringify({pid:process.pid})+'\n');
let turns=[];try{turns=JSON.parse(readFileSync(file,'utf8'));}catch(error){if(error.code!=='ENOENT')throw error;}
const threadId='recovery-thread';
let reads=0;
const emit=(method,params)=>console.log(JSON.stringify({method,params:{threadId,...params}}));
function finish(turn,status='completed') {
  turn.status=status;turn.items.push({id:`answer-${turn.id}`,type:'agentMessage',text:'synthetic recovered answer'});
  writeFileSync(file,JSON.stringify(turns));emit('turn/completed',{turn});
}
for await(const line of createInterface({input:process.stdin})) {
  const request=JSON.parse(line);
  if(process.env.FAKE_CODEX_CALLS)appendFileSync(process.env.FAKE_CODEX_CALLS,line+'\n');
  if(request.id===undefined)continue;
  if(process.env.RECOVERY_STALL_RESUME==='1'&&request.method==='thread/resume')continue;
  if(process.env.RECOVERY_REFUSE_RESUME==='1'&&request.method==='thread/resume') {
    console.log(JSON.stringify({id:request.id,error:{code:-1,message:'synthetic resume refusal'}}));continue;
  }
  let result={};
  if(request.method==='initialize')result={userAgent:'treer-rust-spike/0.162.1'};
  if(request.method==='model/list')result={data:[]};
  if(['thread/start','thread/resume','thread/read'].includes(request.method)) {
    if(request.method==='thread/read')reads++;
    if(request.method==='thread/resume'&&process.env.RECOVERY_STATUS&&turns[0]) {
      turns[0].status=process.env.RECOVERY_STATUS;
      if(turns[0].status!=='inProgress')turns[0].items.push({id:'restored-answer',type:'agentMessage',text:'synthetic recovered answer'});
    }
    if(request.method==='thread/resume'&&process.env.RECOVERY_OLD==='1') {
      turns.push(...Array.from({length:65},(_,n)=>({id:`later-${n}`,status:'completed',items:[]})));
    }
    if(request.method==='thread/read'&&process.env.RECOVERY_LATE==='1'&&reads>=3&&turns[0]) {
      turns[0].status=process.env.RECOVERY_TERMINAL||'completed';
      writeFileSync(file,JSON.stringify(turns));
    }
    const hidden=process.env.RECOVERY_MISSING==='1'||(process.env.RECOVERY_LATE==='1'&&(request.method==='thread/resume'||reads<3));
    result={thread:{id:threadId,status:{type:turns.some(t=>t.status==='inProgress')?'active':'idle'},turns:hidden?[]:turns}};
    if(request.method==='thread/resume'&&process.env.RECOVERY_RESUME_RACE==='1'&&turns[0]) {
      result=structuredClone(result);result.thread.turns[0].status='inProgress';result.thread.status.type='active';
    }
    if(request.method==='thread/resume'&&process.env.RECOVERY_WRONG_THREAD==='1')result.thread.id='unrelated-thread';
    if(hidden&&process.env.RECOVERY_LATE==='1')result.thread.status.type='idle';
  }
  if(request.method==='turn/start') {
    const turn={id:`accepted-${turns.length}`,status:'inProgress',items:[{id:`user-${turns.length}`,type:'userMessage',content:request.params.input}]};
    turns.push(turn);writeFileSync(file,JSON.stringify(turns));result={turn};
    if(process.env.RECOVERY_NO_ID==='1'&&turns.length===1)result={};
    if(process.env.RECOVERY_DISCONNECT==='1'&&turns.length===1) {
      turn.status='interrupted';
      if(process.env.RECOVERY_OLD_DISCONNECT==='1')turns.push(...Array.from({length:65},(_,n)=>({id:`later-${n}`,status:'completed',items:[]})));
      writeFileSync(file,JSON.stringify(turns));
      setTimeout(()=>process.exit(0),600);
    }
    if(process.env.RECOVERY_HOLD!=='1')setTimeout(()=>finish(turn),30);
  }
  console.log(JSON.stringify({id:request.id,result}));
  if(request.method==='thread/goal/get'&&process.env.RECOVERY_STATUS==='inProgress')setTimeout(()=>finish(turns[0]),80);
}
