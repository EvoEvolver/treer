#!/usr/bin/env node
// Synthetic history only. No credential, config or real provider access.
import { createInterface } from 'node:readline';
import { writeFileSync } from 'node:fs';

const threadId = 'synthetic-history';
let next = 0;
writeFileSync(process.env.HISTORY_PID_FILE, String(process.pid));
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  let result = {};
  if (request.method === 'model/list') result = { data: [] };
  if (request.method.startsWith('thread/')) result = { thread: { id: threadId, turns:
    process.env.HISTORY_OVERSIZED === '1' ? Array.from({length:10_001},(_,n)=>({id:`old-${n}`,status:'completed',items:[]})) : [] } };
  if (request.method === 'turn/start') {
    const id = `history-${++next}`;
    result = { turn: { id } };
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
        threadId, turn: { id, status: 'completed', items: [
          { id: `answer-${id}`, type: 'agentMessage', text: 'x'.repeat(512 * 1024) },
        ] },
      } }));
    }, 20);
  }
  console.log(JSON.stringify({ id: request.id, result }));
}
