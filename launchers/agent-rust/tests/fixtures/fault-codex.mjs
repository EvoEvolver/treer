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
let turns=['receipt','burst-previous'].includes(mode)?[{id:'initial',status:'inProgress',items:[]}]:[];
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
  if(['thread/start','thread/resume','thread/read'].includes(request.method))result={thread:{id:threadId,turns,status:{type:turns.some(t=>t.status==='inProgress')?'active':'idle'}}};
  if(request.method==='thread/start'&&mode==='unknown-request')setTimeout(()=>console.log(JSON.stringify({id:'provider-unknown',method:'unsafe/unknown',params:{}})),100);
  if(request.method==='thread/start'&&mode==='diagnostic-eof')setTimeout(()=>process.exit(7),150);
  if(request.method==='thread/start'&&mode==='diagnostic-invalid')setTimeout(()=>{
    process.stderr.write('sensitive-fixture-prefix'+'.'.repeat(10000)+'synthetic-private-tail');
    setTimeout(()=>process.stdout.write('invalid synthetic-secret-do-not-echo\n'),30);
  },150);
  if(request.method==='turn/start') {
    const turn={id:'fault-'+(++started),status:'inProgress',items:[]};turns.push(turn);
    if(mode?.startsWith('final-')) {
      const finalMode=mode.replace('final-observer-','final-');
      const observer=mode==='final-observer'||mode.startsWith('final-observer-');
      const user={id:'native-user',type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]};
      let tool={id:'synthetic-command',type:'commandExecution',command:'synthetic public-file checksum',status:'completed',aggregatedOutput:'synthetic bytes=42 sha256=fixture-only'};
      if(finalMode.startsWith('final-file-'))tool={id:tool.id,type:'fileChange',status:'completed',changes:[{path:'synthetic.txt',kind:'update',diff:'first diff'},{path:'other.txt',kind:'add',diff:'second diff'}]};
      if(finalMode.startsWith('final-tool-'))tool={id:tool.id,type:'dynamicToolCall',tool:'synthetic-tool',namespace:'fixture',arguments:{input:'sealed'},durationMs:1,status:'completed',contentItems:[{type:'inputText',text:'first result'},{type:'inputText',text:'second result'}]};
      const answer={id:'synthetic-answer',type:'agentMessage',text:'synthetic answer'};
      if(finalMode==='final-bound')tool.aggregatedOutput='x'.repeat(9*1024*1024);
      turn.items=[user];emit('turn/started',{turn});
      emit('item/started',{turnId:turn.id,item:user});
      emit('item/started',{turnId:turn.id,item:{...tool,status:'inProgress',aggregatedOutput:''}});
      emit('item/completed',{turnId:turn.id,item:tool});
      // Hold the completed tool live before a nonempty partial final snapshot.
      // IDs/text are synthetic; neither this fixture nor the command accesses
      // any actual provider or file content.
      setTimeout(()=>{
        turn.status='completed';
        turn.itemsView=finalMode==='final-empty'?'notLoaded':finalMode==='final-full'?'full':'summary';
        turn.items=finalMode==='final-full'?[user,tool,answer]:[user,answer];
        if(finalMode==='final-empty')turn.items=[];
        if(finalMode==='final-forward')turn.items=[user,{...tool,aggregatedOutput:tool.aggregatedOutput+' appended'},answer];
        if(finalMode==='final-kind')turn.items=[user,{...answer,id:tool.id}];
        if(finalMode==='final-order')turn.items=[tool,user,answer];
        if(finalMode==='final-item-status')turn.items=[user,{...tool,status:'inProgress'},answer];
        if(finalMode==='final-output')turn.items=[user,{...tool,aggregatedOutput:'conflicting output'},answer];
        if(finalMode==='final-duplicate')turn.items=[user,answer,answer];
        if(finalMode==='final-empty-id')turn.items=[user,{...answer,id:''}];
        if(finalMode==='final-missing-id')turn.items=[user,{type:'agentMessage',text:'no identity'}];
        if(finalMode==='final-id-type')turn.items=[user,{...answer,id:42}];
        if(finalMode==='final-late-user')emit('item/completed',{turnId:turn.id,item:user});
        if(finalMode==='final-id-mismatch') {
          emit('item/completed',{turnId:turn.id,item:answer});
          turn.items=[user,{...answer,id:'different-answer-id'}];
        }
        if(finalMode==='final-bound')turn.items=[user,{...answer,text:'y'.repeat(9*1024*1024)}];
        if(finalMode.startsWith('final-file-')||finalMode.startsWith('final-tool-')) {
          const field=tool.type==='fileChange'?'changes':'contentItems';
          const change=finalMode.split('-').at(-1);
          const values=change==='empty'?[]:change==='shrink'?tool[field].slice(0,1):change==='conflict'?[{...tool[field][0],...(field==='changes'?{diff:'conflict'}:{text:'conflict'})},tool[field][1]]:['name','argument','duration','path'].includes(change)?tool[field]:[...tool[field],field==='changes'?{path:'third.txt',kind:'add',diff:'third diff'}:{type:'inputText',text:'third result'}];
          turn.items=[user,{...tool,[field]:values},answer];
          if(change==='name')turn.items[1].tool+='-extra';
          if(change==='argument')turn.items[1].arguments={input:'sealed-extra'};
          if(change==='duration')turn.items[1].durationMs=2;
          if(change==='path')turn.items[1].changes=[{...tool.changes[0],path:'synthetic.txt-extra'},tool.changes[1]];
        }
        if(finalMode==='final-other-turn')turn.id='other-final-turn';
        if(!observer) {
          emit('turn/completed',{turn});
          // Duplicate summaries must not duplicate retained output; an
          // incompatible terminal outcome must still fence after receipt DONE.
          if(finalMode==='final-terminal')turn.status='failed';
          if(finalMode==='final-terminal-error')turn.error={message:'different error'};
          if(['final-event','final-terminal','final-terminal-error'].includes(finalMode))setTimeout(()=>emit('turn/completed',{turn}),100);
        }
      },700);
      result={turn:{id:turn.id}};
      console.log(JSON.stringify({id:request.id,result}));continue;
    }
    if(mode==='stdin-stall') {
      process.stdin.pause();
      // Replies mirror these bounded permissions, filling the unread stdin
      // pipe while a foreground turn/start response is outstanding.
      for(let n=0;n<32;n++)console.log(JSON.stringify({id:'approval-'+n,method:'item/permissions/requestApproval',params:{permissions:{synthetic:'x'.repeat(64000)}}}));
      setTimeout(()=>console.log(JSON.stringify({id:request.id,result:{turn:{id:turn.id}}})),30);
      timer=setInterval(()=>{},1000);
      await new Promise(()=>{});
    }
    if(mode==='burst-previous') {
      emit('turn/completed',{turn:{id:'initial',status:'completed',items:[]}});
      for(let n=0;n<200;n++)emit('synthetic/notice',{n});
      console.log(JSON.stringify({id:request.id,result:{turn:{}}}));continue;
    }
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
