#!/usr/bin/env node
// Synthetic history only. No credential, config or real provider access.
import { createInterface } from 'node:readline';
import { writeFileSync, appendFileSync } from 'node:fs';
import {once} from 'node:events';

const threadId = 'synthetic-history';
let next = 0;
let echoTurn,echoUser;
let currentTurn;
writeFileSync(process.env.HISTORY_PID_FILE, String(process.pid));
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  let result = {};
  if (request.method === 'model/list') result = { data: [] };
  if (['thread/start','thread/resume','thread/read'].includes(request.method)) result = { thread: { id: threadId, turns:
    process.env.HISTORY_OVERSIZED === '1' ? [{id:'too-large',status:'completed',items:[{id:'large',type:'agentMessage',text:'x'.repeat(17*1024*1024)}]}] : [] } };
  if (['thread/resume','thread/read'].includes(request.method) && process.env.HISTORY_LARGE_RESUME === '1') {
    result.thread.turns = Array.from({length:530},(_,n)=>({id:`old-${n}`,status:'completed',items:[
      {id:`answer-${n}`,type:'agentMessage',text:'x'.repeat((n>=430&&n<480?256:96)*1024)},
    ]}));
    if(process.env.HISTORY_LARGE_ACTIVE==='1')result.thread.turns[0].status='inProgress';
    if(process.env.HISTORY_PROJECTION_OLD==='1')result.thread.turns[430]={id:'history-1',status:'completed',items:[]};
    next=Math.max(next,530);
  }
  if (['thread/resume','thread/read'].includes(request.method) && process.env.HISTORY_LIVE_SHAPE) {
    result.thread.turns=Array.from({length:787},(_,n)=>({id:`old-${n}`,status:'completed',items:[{id:`answer-${n}`,type:'agentMessage',text:'x'.repeat(64*1024)}]}));
    next=Math.max(next,787);
  }
  if (['thread/resume','thread/read'].includes(request.method) && process.env.HISTORY_STALE_SNAPSHOT === '1') {
    // Producer-shaped ordered history: provider restarts can leave historical
    // inProgress statuses. All bytes/IDs/text are synthetic, never copied from
    // a user rollout. Put status AFTER turns to exercise late metadata.
    result.thread.turns=Array.from({length:778},(_,n)=>({id:`old-${n}`,status:[0,51,700,777].includes(n)?'inProgress':'completed',startedAt:n,items:[{id:`answer-${n}`,type:'agentMessage',text:'x'.repeat(64*1024)}]}));
    if(currentTurn)result.thread.turns.push(currentTurn);
    result.thread.status={type:currentTurn?.status==='inProgress'?'active':'idle',activeFlags:[]};
    if(process.env.HISTORY_STALE_INVALID) {
      const mode=process.env.HISTORY_STALE_INVALID;
      result.thread.turns=result.thread.turns.slice(0,60);
      if(mode.startsWith('missing'))delete result.thread.status;
      if(mode==='missingZero'||mode==='missingOne') {
        for(const turn of result.thread.turns)turn.status='completed';
        if(mode==='missingOne')result.thread.turns[0].status='inProgress';
      }
      if(mode==='unknown')result.thread.status.type='unknown';
      if(mode==='paged')result.thread.nextCursor='synthetic-next';
      if(mode==='duplicate')result.thread.turns[51].id='old-0';
      if(mode==='reordered')result.thread.turns[51].startedAt=-1;
      if(mode==='activeNoNewest')result.thread.status.type='active';
    }
    next=Math.max(next,778);
  }
  if (request.method === 'thread/resume' && process.env.HISTORY_MALFORMED_OLD === '1') {
    const turns=Array.from({length:60},(_,n)=>({id:`old-${n}`,status:'completed',items:[]}));
    // The malformed value is outside the retained last-50 window.
    console.log(JSON.stringify({id:request.id,result:{thread:{id:threadId,turns}}}).replace('"old-0"','"old-0",INVALID'));
    continue;
  }
  if(request.method==='thread/read'&&process.env.HISTORY_RECONCILE_OLD==='1') {
    result.thread.turns=[{id:'history-1',status:'completed',items:[]},...Array.from({length:80},(_,n)=>({id:`later-${n}`,status:'completed',items:[]}))];
  }
  if (request.method === 'thread/resume' && process.env.HISTORY_FRAME_BYTES) {
    const target=Number(process.env.HISTORY_FRAME_BYTES);
    if(process.env.HISTORY_FRAME_LOG)appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:target})+'\n');
    const write=async text=>{if(!process.stdout.write(text))await once(process.stdout,'drain');};
    const prefix=JSON.stringify({id:request.id}).slice(0,-1)+',"result":{"thread":{"id":"'+threadId+'","turns":[';
    const suffix='],"status":{"type":"idle"}}}}';let used=Buffer.byteLength(prefix),n=0;
    await write(prefix);
    while(used+suffix.length<target) {
      const comma=n?',':'';
      const turn={id:`boundary-${n}`,status:'completed',items:[{id:`answer-${n}`,type:'agentMessage',text:''}]};
      const empty=JSON.stringify(turn);const remaining=target-used-suffix.length-comma.length-empty.length;
      // Leave enough bytes for a final valid turn rather than an incomplete tail.
      const padding=remaining>1024*1024+256?1024*1024:remaining;
      if(padding<0)throw Error('fixture frame arithmetic');
      turn.items[0].text='x'.repeat(padding);const chunk=comma+JSON.stringify(turn);
      used+=Buffer.byteLength(chunk);await write(chunk);n++;
    }
    await write(suffix+'\n');continue;
  }
  if(request.method==='turn/steer'&&process.env.HISTORY_IDENTICAL_ECHO==='1') {
    const second={id:'second-native',type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]};
    setTimeout(async()=>{
      console.log(JSON.stringify({method:'item/started',params:{threadId,turnId:echoTurn,item:second}}));
      console.log(JSON.stringify({method:'item/started',params:{threadId,turnId:echoTurn,item:second}}));
      await new Promise(resolve=>setTimeout(resolve,100));
      console.log(JSON.stringify({method:'item/completed',params:{threadId,turnId:echoTurn,item:echoUser}}));
      console.log(JSON.stringify({method:'turn/completed',params:{threadId,turn:{id:echoTurn,status:'completed',items:[echoUser,second]}}}));
    },30);
  }
  if (request.method === 'turn/start') {
    const id = `history-${++next}`;
    result = { turn: { id } };
    if(process.env.HISTORY_STALE_SNAPSHOT==='1') {
      const user={id:`native-user-${id}`,type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]};
      currentTurn={id,status:'inProgress',items:[user]};
      console.log(JSON.stringify({method:'turn/started',params:{threadId,turn:currentTurn}}));
      console.log(JSON.stringify({method:'turn/started',params:{threadId,turn:currentTurn}}));
      if(process.env.HISTORY_CONCURRENT_ACTIVE==='1') {
        setTimeout(()=>console.log(JSON.stringify({method:'turn/started',params:{threadId,turn:{id:'genuinely-other-live-turn',status:'inProgress',items:[]}}})),100);
      } else {
        setTimeout(()=>{
          currentTurn={...currentTurn,status:'completed',items:[user,{id:`answer-${id}`,type:'agentMessage',text:'synthetic completion'}]};
          console.log(JSON.stringify({method:'turn/completed',params:{threadId,turn:currentTurn}}));
          if(process.env.HISTORY_REOPEN_TERMINAL==='1')setTimeout(()=>console.log(JSON.stringify({method:'turn/started',params:{threadId,turn:{...currentTurn,status:'inProgress'}}})),100);
        },4500);
      }
      if(process.env.HISTORY_MISMATCH_START==='1')result.turn.id='different-response-id';
      if(process.env.HISTORY_FRAME_LOG)appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(JSON.stringify({id:request.id,result}))})+'\n');
      console.log(JSON.stringify({id:request.id,result}));continue;
    }
    if(process.env.HISTORY_IDENTICAL_ECHO==='1') {
      echoTurn=id;echoUser={id:'first-native',type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]};
      setTimeout(()=>console.log(JSON.stringify({method:'turn/started',params:{threadId,turn:{id,status:'inProgress',items:[echoUser]}}})),30);
      if(process.env.HISTORY_FRAME_LOG)appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(JSON.stringify({id:request.id,result}))})+'\n');
      console.log(JSON.stringify({id:request.id,result}));continue;
    }
    if(process.env.HISTORY_LIVE_SHAPE || process.env.HISTORY_ECHO_USER==='1') {
      const user={id:`native-user-${id}`,type:'userMessage',content:[{type:'text',text:request.params.input[0].text}]};
      const rich={id:`tool-${id}`,type:'dynamicToolCall',tool:'synthetic',output:process.env.HISTORY_LIVE_SHAPE?'x'.repeat(5*1024*1024):'',content:Array.from({length:Number(process.env.HISTORY_LIVE_SHAPE)||1},()=>({a:[0,1,2],b:true}))};
      setTimeout(async()=>{
        for(const event of [
          {method:'turn/started',params:{threadId,turn:{id,status:'inProgress',items:[user]}}},
          {method:'item/started',params:{threadId,turnId:id,item:user}},
          {method:'item/completed',params:{threadId,turnId:id,item:rich}},
          {method:'item/completed',params:{threadId,turnId:id,item:user}},
          {method:'turn/completed',params:{threadId,turn:{id,status:'completed',items:[user,rich,{id:`answer-${id}`,type:'agentMessage',text:'done'}]}}},
        ]){console.log(JSON.stringify(event));await new Promise(resolve=>setTimeout(resolve,100));}
      },Number(process.env.HISTORY_LIVE_DELAY)||30);
      if(process.env.HISTORY_FRAME_LOG)appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(JSON.stringify({id:request.id,result}))})+'\n');
      console.log(JSON.stringify({id:request.id,result}));continue;
    }
    if(process.env.HISTORY_RECONCILE_OLD==='1') {
      if(process.env.HISTORY_FRAME_LOG)appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(JSON.stringify({id:request.id,result}))})+'\n');
      console.log(JSON.stringify({id:request.id,result}));continue;
    }
    if (process.env.HISTORY_DELTA === '1') {
      setTimeout(() => console.log(JSON.stringify({method:'item/started',params:{threadId,turnId:id,item:{id:'stream',type:'agentMessage',text:''}}})),20);
      let remaining=10;
      const timer=setInterval(()=>{
        console.log(JSON.stringify({method:'item/agentMessage/delta',params:{threadId,turnId:id,itemId:'stream',delta:'x'.repeat(2*1024*1024)}}));
        if(--remaining===0)clearInterval(timer);
      },500);
      console.log(JSON.stringify({ id: request.id, result }));
      continue;
    }
    setTimeout(() => {
      console.log(JSON.stringify({ method: 'turn/completed', params: {
        threadId, turn: { id: process.env.HISTORY_INVALID_ID === '1' ? { secret: 'synthetic-do-not-echo' } : id, status: 'completed', items: [
          { id: `answer-${id}`, type: 'agentMessage', text: 'x'.repeat(512 * 1024) },
        ] },
      } }));
    }, 20);
  }
  if(result.thread&&!result.thread.status&&!process.env.HISTORY_STALE_INVALID)
    result.thread.status={type:result.thread.turns.some(t=>t.status==='inProgress')?'active':'idle'};
  const response=JSON.stringify({ id: request.id, result });
  if (process.env.HISTORY_FRAME_LOG) appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(response)})+'\n');
  console.log(response);
}
