// Deliberately small, sealed provider for dispatch/outbox failure boundaries.
import readline from 'node:readline';
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
const mode=process.env.RUST_SPIKE_FAULT;
writeFileSync(process.env.RUST_SPIKE_PROVIDER_LOG+'.pid',String(process.pid),{mode:0o600});
if(mode==='descendant') {
  const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});
  appendFileSync(process.env.RUST_SPIKE_PROVIDER_LOG+'.child',String(child.pid),{mode:0o600});
}
const threadId='fault-thread';let started=0, timer;
let turns=mode==='receipt'?[{id:'initial',status:'inProgress',items:[]}]:[];
const emit=(method,params)=>console.log(JSON.stringify({method,params:{threadId,...params}}));
for await(const line of readline.createInterface({input:process.stdin})) {
  const request=JSON.parse(line);
  appendFileSync(process.env.RUST_SPIKE_PROVIDER_LOG,line+'\n',{mode:0o600});
  if(!request.id)continue;
  if(!request.method)continue; // Response to a provider-initiated request.
  if(mode==='shutdown-startup'&&request.method==='initialize')continue;
  let result={};
  if(mode==='goal-ack-lost'&&request.method.startsWith('thread/goal/')) {
    const file='.fault-goal.json';
    let goal=existsSync(file)?JSON.parse(readFileSync(file,'utf8')):null;
    if(request.method==='thread/goal/set') {
      const {threadId,origin,...input}=request.params;
      if(origin!=='user')throw Error('goal origin');
      goal={status:'active',...goal,...input};writeFileSync(file,JSON.stringify(goal));
      console.log(JSON.stringify({id:request.id,error:{message:'sealed lost goal acknowledgement'}}));continue;
    }
    result={goal};
  }
  if(request.method==='model/list')result={data:[]};
  if(mode==='burst-startup'&&request.method==='initialize') {
    process.stdout.write(Array.from({length:64},(_,n)=>JSON.stringify({method:'synthetic/startup',params:{n}})+'\n').join(''));
  }
  if(['thread/start','thread/resume','thread/read'].includes(request.method))result={thread:{id:threadId,turns}};
  if(request.method==='thread/start'&&mode==='unknown-request')setTimeout(()=>console.log(JSON.stringify({id:'provider-unknown',method:'unsafe/unknown',params:{}})),100);
  if(request.method==='thread/start'&&mode==='diagnostic-eof')setTimeout(()=>process.exit(7),150);
  if(request.method==='thread/start'&&mode==='diagnostic-invalid')setTimeout(()=>{
    process.stderr.write('sensitive-fixture-prefix'+'.'.repeat(10000)+'synthetic-private-tail');
    setTimeout(()=>process.stdout.write('invalid synthetic-secret-do-not-echo\n'),30);
  },150);
  if(request.method==='turn/start') {
    const turn={id:'fault-'+(++started),status:'inProgress',items:[]};turns.push(turn);
    if(mode==='burst-turn') {
      emit('turn/started',{turn});
      emit('item/started',{turnId:turn.id,item:{id:'burst-item',type:'agentMessage',text:''}});
      for(let n=0;n<200;n++)emit('item/agentMessage/delta',{turnId:turn.id,itemId:'burst-item',delta:'x'});
      turn.status='completed';
      // Sparse terminal frame must retain all the deltas consumed while the
      // launcher waits for the turn/start response identifying the receipt.
      emit('turn/completed',{turn:{...turn,items:[]}});
      console.log(JSON.stringify({id:request.id,result:{turn:{id:turn.id}}}));continue;
    }
    if(started===1&&['omit','timeout'].includes(mode)) {
      // Provider accepts the operation, but its identity/response is lost.
      setTimeout(()=>emit('turn/started',{turn}),300);
      if(mode==='timeout')continue;
      result={turn:{}};
    } else {
      result={turn:{id:turn.id}};
      setTimeout(()=>{
        if(mode==='observe') turn.status='completed'; // no event; thread/read is authoritative
        if(mode==='sparse'||mode==='goal-ack-lost') {
          turn.status='completed';
          turn.items=[{id:'answer',type:'agentMessage',text:'sparse answer'}];
          emit('turn/completed',{turn});
        }
        if(mode==='stream'&&started===1) {
          turn.status='completed';emit('turn/completed',{turn});
        } else if(mode==='stream') {
          emit('turn/started',{turn});
          emit('item/started',{turnId:turn.id,item:{id:'stream-item',type:'agentMessage',text:''}});
          timer=setInterval(()=>emit('item/agentMessage/delta',{turnId:turn.id,itemId:'stream-item',delta:'chunk '}),20);
        }
      },40);
    }
  }
  if(request.method==='turn/interrupt') {
    clearInterval(timer);
    const turn=turns.find(t=>t.id===request.params.turnId);
    if(turn){turn.status='interrupted';setTimeout(()=>emit('turn/completed',{turn}),20);}
  }
  console.log(JSON.stringify({id:request.id,result}));
}
clearInterval(timer);
