// Producer-shaped task invocation boundary captured from Claude 2.1.306.
// All data/paths/session/output here are task-owned synthetic values.
import {createInterface} from 'node:readline';
import {appendFileSync,existsSync} from 'node:fs';
const session=process.argv.at(-1),log=process.env.CLAUDE_FIXTURE_LOG;
const emit=e=>process.stdout.write(JSON.stringify({session_id:session,...e})+'\n');
let count=0,queued=[];
const ordinary=e=>{
  emit({type:'system',subtype:'init',user_message_uuids:[e.uuid],model:'claude-current'});
  emit({type:'assistant',message:{id:'ordinary-'+count,content:[{type:'text',text:'user response '+count}]}});
  emit({type:'result',is_error:false,result:'user response '+count,user_message_uuids:[e.uuid]});
};
const wait=(path,run)=>{if(existsSync(path))run();else setTimeout(()=>wait(path,run),10);};
createInterface({input:process.stdin}).on('line',line=>{
  const e=JSON.parse(line);
  if(e.type==='control_request') {emit({type:'control_response',response:{subtype:'success',request_id:e.request_id,response:{}}});return;}
  if(e.type!=='user')process.exit(71);
  count++;appendFileSync(log,JSON.stringify({uuid:e.uuid,text:e.message.content})+'\n');
  if(count>1){queued.push(e);return;}
  ordinary(e);
  wait('start-followup',()=>{
    if(!process.env.CLAUDE_FIXTURE_UNSOLICITED)emit({type:'system',subtype:'task_notification',task_id:'own-task',run_id:'own-run',status:'completed'});
    emit({type:'system',subtype:'init',model:'old-output-model'});
    emit({type:'stream_event',event:{type:'message_start',message:{id:'task-message'}}});
    emit({type:'assistant',message:{id:'task-message',model:'old-output-model',content:[{type:'text',text:'own task followup'}]}});
    appendFileSync(log+'.followup','started');
    wait('finish-followup',()=>{
      emit({type:'result',is_error:false,result:'own task followup',origin:{kind:'task-notification',producer:'session-task',runId:process.env.CLAUDE_FIXTURE_BAD_TASK_ORIGIN?'other-run':'own-run'}});
      // An exact duplicate metadata notification must not arm a second run.
      emit({type:'system',subtype:'task_notification',task_id:'own-task',run_id:'own-run',status:'completed'});
      wait('finish-input',()=>{for(const next of queued)ordinary(next);queued=[];});
    });
  });
});
