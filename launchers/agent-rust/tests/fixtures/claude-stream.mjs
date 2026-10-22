// Synthetic producer, no provider installation/home/config or network access.
import { createInterface } from 'node:readline';
import { appendFileSync, existsSync } from 'node:fs';
const args=process.argv.slice(2), session=args.at(-1);
const emit=event=>process.stdout.write(JSON.stringify({session_id:session,...event})+'\n');
let active=false, timer;
const finish=()=>{active=false;emit({type:'result',subtype:'success',is_error:false,result:'synthetic answer'});};
createInterface({input:process.stdin}).on('line',line=>{
  const event=JSON.parse(line);
  if(event.type==='control_request'){
    emit({type:'control_response',response:{subtype:'success',request_id:event.request_id,response:{}}});
    if(event.request.subtype==='interrupt'){clearTimeout(timer);finish();}
    return;
  }
  if(event.type!=='user'||active)process.exit(71);
  active=true;appendFileSync(process.env.CLAUDE_FIXTURE_LOG,JSON.stringify({type:'accepted',resume:args.includes('--resume'),session})+'\n');
  emit({type:'system',subtype:'init'});
  const id='message-'+Date.now();
  emit({type:'stream_event',event:{type:'message_start',message:{id}}});
  emit({type:'stream_event',event:{type:'content_block_start',index:0,content_block:{type:'text',text:''}}});
  emit({type:'stream_event',event:{type:'content_block_delta',index:0,delta:{type:'text_delta',text:'thinking first'}}});
  emit({type:'assistant',message:{id,content:[{type:'text',text:'thinking first'},{type:'tool_use',id:'tool-'+id,name:'Bash',input:{command:'synthetic readonly probe'}}]}});
  emit({type:'user',message:{role:'user',content:[{type:'tool_result',tool_use_id:'tool-'+id,content:'synthetic retained output',is_error:false}]}});
  // Provider idle can precede result; acceptance of another user here is a bug.
  emit({type:'system',subtype:'session_state_changed',state:'idle'});
  const wait=()=>{
    if(process.env.CLAUDE_FIXTURE_HOLD&&!existsSync(process.env.CLAUDE_FIXTURE_HOLD)){timer=setTimeout(wait,20);return;}
    finish();
  };
  timer=setTimeout(wait,200);
});
