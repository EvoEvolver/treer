// Task-owned protocol/native-file producer. No real provider/config/auth access.
import { createInterface } from 'node:readline';
import { appendFileSync, mkdirSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
const session=process.argv.at(-1), cwd=process.cwd(), home=process.env.SNAPSHOT_NATIVE_HOME;
const dir=join(home,'projects',cwd.replace(/[^a-zA-Z0-9]/g,'-'));
mkdirSync(dir,{recursive:true,mode:0o700});
const file=join(dir,session+'.jsonl');
const row=value=>appendFileSync(file,JSON.stringify({sessionId:session,cwd,...value})+'\n',{mode:0o600});
row({type:'system',content:'synthetic native fixture'});
const emit=event=>process.stdout.write(JSON.stringify({session_id:session,...event})+'\n');
createInterface({input:process.stdin}).on('line',line=>{
  const event=JSON.parse(line);
  if(event.type==='control_request'){
    appendFileSync(process.env.SNAPSHOT_CALLS,JSON.stringify({control:event.request.subtype})+'\n');
    emit({type:'control_response',response:{subtype:'success',request_id:event.request_id,response:{}}});return;
  }
  if(event.type!=='user')process.exit(71);
  const text=Array.isArray(event.message.content)?event.message.content.filter(c=>c.type==='text').map(c=>c.text).join(''):event.message.content;
  appendFileSync(process.env.SNAPSHOT_CALLS,JSON.stringify({text,uuid:event.uuid})+'\n');
  row({type:'user',message:{role:'user',content:text}});
  if(text==='background')emit({type:'system',subtype:'task_started',task_id:'own-running',task_type:'local_bash',status:'running',description:'Synthetic running task'});
  emit({type:'assistant',message:{id:'message-'+event.uuid,model:'synthetic',content:[{type:'text',text:'synthetic answer'}]}});
  setTimeout(()=>{row({type:'assistant',message:{role:'assistant',content:'synthetic answer'}});emit({type:'result',subtype:'success',is_error:false,result:'synthetic answer'});},300);
});
if(process.env.SNAPSHOT_AFTER_GATE){
  const timer=setInterval(()=>{
    if(!existsSync(process.env.SNAPSHOT_AFTER_GATE))return;
    clearInterval(timer);
    for(let i=0;i<64;i++)emit({type:'system',subtype:'task_notification',task_id:'own-terminal-'+i,status:'completed',description:'Synthetic metadata'});
  },10);
}
