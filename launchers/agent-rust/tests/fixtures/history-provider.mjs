#!/usr/bin/env node
// Synthetic history only. No credential, config or real provider access.
import { createInterface } from 'node:readline';
import { writeFileSync, appendFileSync } from 'node:fs';
import {once} from 'node:events';

const threadId = 'synthetic-history';
let next = 0;
writeFileSync(process.env.HISTORY_PID_FILE, String(process.pid));
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  let result = {};
  if (request.method === 'model/list') result = { data: [] };
  if (request.method.startsWith('thread/')) result = { thread: { id: threadId, turns:
    process.env.HISTORY_OVERSIZED === '1' ? [{id:'too-large',status:'completed',items:[{id:'large',type:'agentMessage',text:'x'.repeat(17*1024*1024)}]}] : [] } };
  if (['thread/resume','thread/read'].includes(request.method) && process.env.HISTORY_LARGE_RESUME === '1') {
    result.thread.turns = Array.from({length:530},(_,n)=>({id:`old-${n}`,status:'completed',items:[
      {id:`answer-${n}`,type:'agentMessage',text:'x'.repeat(96*1024)},
    ]}));
    if(process.env.HISTORY_LARGE_ACTIVE==='1')result.thread.turns[0].status='inProgress';
    if(process.env.HISTORY_PROJECTION_OLD==='1')result.thread.turns[430]={id:'history-1',status:'completed',items:[]};
    next=Math.max(next,530);
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
    const suffix=']}}}';let used=Buffer.byteLength(prefix),n=0;
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
  if (request.method === 'turn/start') {
    const id = `history-${++next}`;
    result = { turn: { id } };
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
  const response=JSON.stringify({ id: request.id, result });
  if (process.env.HISTORY_FRAME_LOG) appendFileSync(process.env.HISTORY_FRAME_LOG,JSON.stringify({method:request.method,bytes:Buffer.byteLength(response)})+'\n');
  console.log(response);
}
