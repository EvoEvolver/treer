// Only generated native/config paths supplied by session-launch-entry.mjs.
import { createInterface } from 'node:readline';
import { appendFileSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
const session=process.argv.at(-1),cwd=process.cwd(),home=process.env.CLAUDE_CONFIG_DIR||join(process.env.HOME,'.claude');
const dir=join(home,'projects',cwd.replace(/[^a-zA-Z0-9]/g,'-')),file=join(dir,session+'.jsonl');
mkdirSync(dir,{recursive:true,mode:0o700});
const log=value=>appendFileSync(process.env.LAUNCH_CALLS,JSON.stringify(value)+'\n',{mode:0o600});
const resume=process.argv.includes('--resume');
const context=resume?readFileSync(file,'utf8'):'';
log({spawn:true,session,cwd,home,resume,context:context.includes('original synthetic context'),auth:process.env.ANTHROPIC_API_KEY==='synthetic-launch-token'});
const row=value=>appendFileSync(file,JSON.stringify({sessionId:session,cwd,...value})+'\n',{mode:0o600});
if(!resume)row({type:'system',content:'synthetic'});
const emit=value=>process.stdout.write(JSON.stringify({session_id:session,...value})+'\n');
createInterface({input:process.stdin}).on('line',line=>{
  const event=JSON.parse(line);
  if(event.type==='control_request'){
    log({control:event.request.subtype});
    if(event.request.subtype==='initialize'&&process.env.LAUNCH_REFUSE_INIT==='1'){
      emit({type:'system',subtype:'task_notification',task_id:'bootstrap-only',status:'completed'});
      emit({type:'control_response',response:{subtype:'error',request_id:event.request_id,error:'synthetic initialization refusal'}});return;
    }
    emit({type:'control_response',response:{subtype:'success',request_id:event.request_id,response:{}}});return;
  }
  if(event.type!=='user')process.exit(71);
  const text=Array.isArray(event.message.content)?event.message.content.filter(b=>b.type==='text').map(b=>b.text).join(''):event.message.content;
  log({text,uuid:event.uuid});row({type:'user',uuid:event.uuid,message:{role:'user',content:text}});
  emit({type:'assistant',message:{id:'answer-'+event.uuid,model:'synthetic',content:[{type:'text',text:'synthetic continued answer'}]}});
  row({type:'assistant',uuid:'answer-'+event.uuid,message:{role:'assistant',content:'synthetic continued answer'}});
  emit({type:'result',subtype:'success',is_error:false,result:'synthetic continued answer'});
});
