#!/usr/bin/env node
// Disposable producer-shaped multi-input turn. No native history/config reads.
import {createInterface} from 'node:readline';
import {readFileSync,writeFileSync,appendFileSync} from 'node:fs';
const path='legacy-steer-order.json',threadId='legacy-steer-thread';
let saved={turns:[],inputs:[]};
try{saved=JSON.parse(readFileSync(path,'utf8'));}catch{}
const send=value=>console.log(JSON.stringify(value));
const emit=(method,params)=>send({method,params:{threadId,...params}});
const user=(id,text)=>({id,type:'userMessage',content:[{type:'text',text}]});
const save=()=>writeFileSync(path,JSON.stringify(saved));
for await(const line of createInterface({input:process.stdin})){
  const r=JSON.parse(line);if(r.id===undefined)continue;
  if(process.env.FAKE_CODEX_CALLS)appendFileSync(process.env.FAKE_CODEX_CALLS,JSON.stringify(r)+'\n');
  let result={};
  if(r.method==='initialize')result={userAgent:'codex/0.162.1'};
  if(r.method==='model/list')result={data:[]};
  if(['thread/start','thread/resume','thread/read'].includes(r.method)){
    result={thread:{id:threadId,status:{type:saved.turns.at(-1)?.status==='inProgress'?'active':'idle'},turns:saved.turns}};
    if(r.method==='thread/resume'&&process.env.LEGACY_ORDER_ECHO==='1')setTimeout(()=>{
      const turn=saved.turns.at(-1);
      const full=[...turn.items.slice(0,3)];
      saved.inputs.forEach((text,n)=>{full.push(user('native-steer-'+n,text),turn.items[3+n]);});
      // Late wire echoes arrive in their real position in the subsequent full
      // provider snapshot, not in the legacy receipt's guessed leading slot.
      for(const item of full.filter(i=>i.id.startsWith('native-steer-')))emit('item/completed',{turnId:turn.id,item});
      turn.items=full;turn.status='completed';save();emit('turn/completed',{turn});
      if(process.env.LEGACY_ORDER_CONTRADICT==='1')setTimeout(()=>{
        const changed={...turn,items:[full[0],full[3],...full.slice(1,3),...full.slice(4)]};
        emit('turn/completed',{turn:changed});
      },300);
    },750);
  }
  if(r.method==='turn/start'){
    const turn={id:'multi-user-turn',status:'inProgress',items:[user('initial-user',r.params.input[0].text),{id:'before',type:'agentMessage',text:'before steering'},{id:'tool',type:'commandExecution',command:'read-only tool',status:'completed',aggregatedOutput:'result'}]};
    saved.turns.push(turn);save();result={turn};send({id:r.id,result});emit('turn/started',{turn});continue;
  }
  if(r.method==='turn/steer'){
    const n=saved.inputs.length;saved.inputs.push(r.params.input[0].text);
    const turn=saved.turns.at(-1);turn.items.push({id:'after-'+n,type:'agentMessage',text:'after steering '+n});save();
    result={turnId:turn.id};emit('item/completed',{turnId:turn.id,item:turn.items.at(-1)});
  }
  send({id:r.id,result});
}
