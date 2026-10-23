#!/usr/bin/env node
// Producer-shaped synthetic history; only this disposable fixture's own file.
import {createInterface} from 'node:readline';
import {readFileSync,writeFileSync,appendFileSync} from 'node:fs';
const path=process.env.ORDER_HISTORY_FILE,threadId='order-thread';
let turns;
try {turns=JSON.parse(readFileSync(path,'utf8'));} catch {turns=Array.from({length:12},(_,n)=>({id:'past-'+n,status:'completed',startedAt:n,items:[{id:'past-user-'+n,type:'userMessage',content:[{type:'text',text:'past question '+n}]},{id:'past-answer-'+n,type:'agentMessage',text:'past answer '+n}]}));}
const send=value=>console.log(JSON.stringify(value));
const emit=(method,params)=>send({method,params:{threadId,...params}});
let live;
const save=()=>writeFileSync(path,JSON.stringify(turns));
function complete(){if(!live||live.status!=='inProgress')return;live.status='completed';save();emit('turn/completed',{turn:process.env.ORDER_SUMMARY==='1'?{...live,items:live.items.filter(i=>i.type!=='commandExecution')}:live});}
for await (const line of createInterface({input:process.stdin})) {
  const request=JSON.parse(line);if(request.id===undefined)continue;
  if(process.env.FAKE_CODEX_CALLS)appendFileSync(process.env.FAKE_CODEX_CALLS,JSON.stringify(request)+'\n');
  let result={};
  if(request.method==='initialize')result={userAgent:'codex/0.162.1'};
  if(request.method==='model/list')result={data:[]};
  if(['thread/start','thread/resume','thread/read'].includes(request.method))result={thread:{id:threadId,status:{type:live?.status==='inProgress'?'active':'idle'},turns}};
  if(request.method==='turn/start') {
    const n=turns.length,id='live-'+n;
    live={id,status:'inProgress',startedAt:n,items:[{id:'native-user-'+n,type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]},{id:'before-'+n,type:'agentMessage',text:'assistant before steering'},{id:'tool-'+n,type:'commandExecution',command:'synthetic read only',status:'completed',aggregatedOutput:'synthetic output'}]};
    turns.push(live);save();result={turn:live};
    send({id:request.id,result});emit('turn/started',{turn:live});
    // No automatic completion of the held turn; explicit ordinary follow-up
    // completes itself, letting the test assert queued/new-turn ordering too.
    if(request.params.input[0].text==='ordinary follow-up')setTimeout(complete,40);
    continue;
  }
  if(request.method==='turn/steer') {
    if(process.env.ORDER_NATIVE_ECHO==='1')live.items.push({id:'native-steer',type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]});
    live.items.push({id:'after-'+live.id,type:'agentMessage',text:'assistant after steering'});save();
    result={turnId:live.id};setTimeout(complete,80);
  }
  send({id:request.id,result});
}
