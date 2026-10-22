#!/usr/bin/env node
// Synthetic provider, only used by fixture-harness actual-entry tests.
import {createInterface} from 'node:readline';
import {readFileSync,writeFileSync,appendFileSync} from 'node:fs';
const file='recovery-provider.json';
let turns=[];try{turns=JSON.parse(readFileSync(file,'utf8'));}catch(error){if(error.code!=='ENOENT')throw error;}
const threadId='recovery-thread';
const emit=(method,params)=>console.log(JSON.stringify({method,params:{threadId,...params}}));
function finish(turn,status='completed') {
  turn.status=status;turn.items.push({id:`answer-${turn.id}`,type:'agentMessage',text:'synthetic recovered answer'});
  writeFileSync(file,JSON.stringify(turns));emit('turn/completed',{turn});
}
for await(const line of createInterface({input:process.stdin})) {
  const request=JSON.parse(line);
  if(process.env.FAKE_CODEX_CALLS)appendFileSync(process.env.FAKE_CODEX_CALLS,line+'\n');
  if(request.id===undefined)continue;
  let result={};
  if(request.method==='initialize')result={userAgent:'treer-rust-spike/0.162.1'};
  if(request.method==='model/list')result={data:[]};
  if(['thread/start','thread/resume','thread/read'].includes(request.method)) {
    if(request.method==='thread/resume'&&process.env.RECOVERY_STATUS&&turns[0]) {
      turns[0].status=process.env.RECOVERY_STATUS;
      if(turns[0].status!=='inProgress')turns[0].items.push({id:'restored-answer',type:'agentMessage',text:'synthetic recovered answer'});
    }
    result={thread:{id:threadId,status:{type:turns.some(t=>t.status==='inProgress')?'active':'idle'},turns:process.env.RECOVERY_MISSING==='1'?[]:turns}};
  }
  if(request.method==='turn/start') {
    const turn={id:`accepted-${turns.length}`,status:'inProgress',items:[{id:`user-${turns.length}`,type:'userMessage',content:request.params.input}]};
    turns.push(turn);writeFileSync(file,JSON.stringify(turns));result={turn};
    if(process.env.RECOVERY_HOLD!=='1')setTimeout(()=>finish(turn),30);
  }
  console.log(JSON.stringify({id:request.id,result}));
  if(request.method==='thread/goal/get'&&process.env.RECOVERY_STATUS==='inProgress')setTimeout(()=>finish(turns[0]),80);
}
