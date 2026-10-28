// Captured Claude 2.1.306 CronCreate producer shape: no origin/user UUID on
// scheduled init/result; command_lifecycle provides the invocation scope.
import {createInterface} from 'node:readline';
import {appendFileSync,existsSync} from 'node:fs';
const session=process.argv.at(-1),log=process.env.CLAUDE_FIXTURE_LOG;
const id='91d7ab21-8b8e-40ce-913b-4eb60e7e6fa2';
const emit=e=>process.stdout.write(JSON.stringify({session_id:session,...e})+'\n');
const wait=(p,fn)=>existsSync(p)?fn():setTimeout(()=>wait(p,fn),10);
let count=0,queued=[];
const ordinary=e=>{
 emit({type:'command_lifecycle',command_uuid:e.uuid,state:'started'});
 emit({type:'system',subtype:'init',user_message_uuids:[e.uuid],model:'claude-current'});
 emit({type:'assistant',message:{id:e.uuid,content:[{type:'text',text:'ordinary'}]}});
 emit({type:'result',result:'ordinary',user_message_uuids:[e.uuid]});
 emit({type:'command_lifecycle',command_uuid:e.uuid,state:'completed'});
};
createInterface({input:process.stdin}).on('line',l=>{
 const e=JSON.parse(l);
 if(e.type==='control_request'){emit({type:'control_response',response:{subtype:'success',request_id:e.request_id,response:{}}});return;}
 if(e.type!=='user')process.exit(71);
 count++;appendFileSync(log,JSON.stringify({uuid:e.uuid,text:e.message.content})+'\n');
 emit({type:'command_lifecycle',command_uuid:e.uuid,state:'queued'});
 if(count>1){queued.push(e);return;}
 ordinary(e);
 wait('start-scheduled',()=>{
  if(!process.env.CLAUDE_FIXTURE_NO_COMMAND)emit({type:'command_lifecycle',command_uuid:id,state:'started'});
  emit({type:'system',subtype:'init',model:'old-output-model'});
  emit({type:'stream_event',event:{type:'message_start',message:{id:'cron-output'}}});
  emit({type:'assistant',message:{id:'cron-output',model:'old-output-model',content:[{type:'text',text:'own scheduled output'}]}});
  appendFileSync(log+'.scheduled','started');
  wait('finish-scheduled',()=>{
   emit({type:'result',result:'own scheduled output',...(process.env.CLAUDE_FIXTURE_BORROWED_INPUT?{user_message_uuids:[queued[0]?.uuid]}:{})});
   appendFileSync(log+'.result','result');
   wait('complete-command',()=>{
    emit({type:'command_lifecycle',command_uuid:process.env.CLAUDE_FIXTURE_WRONG_COMMAND?'d794df5c-c490-442c-bbf3-11b0e939195a':id,state:'completed'});
    wait('finish-input',()=>{for(const next of queued)ordinary(next);queued=[];});
   });
  });
 });
});
