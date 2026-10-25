// Synthetic producer, no provider installation/home/config or network access.
import { createInterface } from 'node:readline';
import { appendFileSync, existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
const args=process.argv.slice(2), session=args.at(-1);
if(process.env.CLAUDE_FIXTURE_SPAWN_LOG)appendFileSync(process.env.CLAUDE_FIXTURE_SPAWN_LOG,'spawned\n');
const emit=event=>process.stdout.write(JSON.stringify({session_id:session,...event})+'\n');
let active=false, timer;
let model='claude-sonnet-test';
let modelChanges=0;
const models=[{value:'default',resolvedModel:'claude-sonnet-test'},{value:'sonnet',resolvedModel:'claude-sonnet-test',displayName:'Sonnet from provider'},{value:'opus',resolvedModel:'claude-opus-test'},{value:'legacy',resolvedModel:'claude-legacy-test'}];
const finish=()=>{active=false;emit({type:'result',subtype:'success',is_error:false,result:'synthetic answer'});};
createInterface({input:process.stdin}).on('line',async line=>{
  const event=JSON.parse(line);
  if(event.type==='control_request'){
    if(process.env.CLAUDE_FIXTURE_MODELS){
      appendFileSync(process.env.CLAUDE_FIXTURE_LOG+'.controls',JSON.stringify(event)+'\n');
      const reply=(subtype,response={})=>emit({type:'control_response',response:{subtype,request_id:event.request_id,response,error:subtype==='error'?'synthetic model refused':undefined}});
      if(event.request.subtype==='initialize'){reply('success',process.env.CLAUDE_FIXTURE_NO_MODELS?{}:{models});return;}
      if(event.request.subtype==='set_model'){
        const selected=event.request.model, option=models.find(m=>m.value===selected||m.resolvedModel===selected);
        if(selected==='invalid'){setTimeout(()=>reply('error'),300);return;}
        model=option?.resolvedModel||selected;
        if(process.env.CLAUDE_FIXTURE_OLD_MODEL_AFTER_TIMEOUT)setTimeout(()=>{
          emit({type:'assistant',message:{id:'old-model-after-timeout',model:'claude-sonnet-test',content:[{type:'text',text:'old timeout tail'}]}});
          emit({type:'system',subtype:'init',model:'claude-sonnet-test'});
        },16000);
        if(active&&process.env.CLAUDE_FIXTURE_MODEL_BURST)for(let i=0;i<64;i++)emit({type:'stream_event',event:{type:'content_block_delta',index:0,delta:{type:'text_delta',text:''}}});
        const delay=selected===process.env.CLAUDE_FIXTURE_TIMEOUT_MODEL?18000:process.env.CLAUDE_FIXTURE_MODEL_DELAY_ONCE&&modelChanges++>0?0:Number(process.env.CLAUDE_FIXTURE_MODEL_DELAY||0);
        if(process.env.CLAUDE_FIXTURE_WRONG_MODEL_ACK)emit({type:'control_response',response:{subtype:'success',request_id:'999999',response:{}}});
        setTimeout(()=>{
          reply('success');
          if(active&&process.env.CLAUDE_FIXTURE_OLD_MODEL_AFTER_ACK)emit({type:'assistant',message:{id:'old-model-after-ack',model:'claude-sonnet-test',content:[{type:'text',text:'old turn tail'}]}});
        },delay);return;
      }
    }
    if(event.request.subtype==='interrupt'&&process.env.CLAUDE_FIXTURE_REFUSE_INTERRUPT){
      emit({type:'control_response',response:{subtype:'error',request_id:event.request_id,error:'synthetic interrupt refused'}});
      clearTimeout(timer);timer=setTimeout(finish,150);return;
    }
    emit({type:'control_response',response:{subtype:'success',request_id:event.request_id,response:{}}});
    if(event.request.subtype==='interrupt'){clearTimeout(timer);finish();}
    return;
  }
  if(event.type!=='user'||active&&!process.env.CLAUDE_FIXTURE_STEER)process.exit(71);
  const text=typeof event.message.content==='string'?event.message.content:event.message.content.filter(b=>b.type==='text').map(b=>b.text).join('\n');
  const images=Array.isArray(event.message.content)?event.message.content.filter(b=>b.type==='image').map(b=>{
    if(b.source.type!=='base64')process.exit(72);
    const bytes=Buffer.from(b.source.data,'base64');return {mime:b.source.media_type,bytes:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')};
  }):[];
  appendFileSync(process.env.CLAUDE_FIXTURE_LOG,JSON.stringify({type:active?'steered':'accepted',resume:args.includes('--resume'),session,...(process.env.CLAUDE_FIXTURE_MODELS||process.env.CLAUDE_FIXTURE_STEER?{model,text,uuid:event.uuid}:{}),...(process.env.CLAUDE_FIXTURE_IMAGES?{images}:{})})+'\n');
  if(active){emit({type:'user',uuid:event.uuid,message:event.message});return;}
  active=true;
  while(process.env.CLAUDE_FIXTURE_INPUT_GATE&&!existsSync(process.env.CLAUDE_FIXTURE_INPUT_GATE))await new Promise(r=>setTimeout(r,20));
  emit({type:'system',subtype:'init',...(process.env.CLAUDE_FIXTURE_MODELS?{model}:{})});
  const id='message-'+Date.now();
  emit({type:'stream_event',event:{type:'message_start',message:{id}}});
  emit({type:'stream_event',event:{type:'content_block_start',index:0,content_block:{type:'text',text:''}}});
  emit({type:'stream_event',event:{type:'content_block_delta',index:0,delta:{type:'text_delta',text:'thinking first'}}});
  emit({type:'assistant',message:{id,content:[{type:'text',text:'thinking first'},{type:'tool_use',id:'tool-'+id,name:'Bash',input:{command:'synthetic readonly probe'}}]}});
  if(process.env.CLAUDE_FIXTURE_DISTINCT_FINAL)emit({type:'assistant',message:{id:id+'-different',content:[{type:'text',text:'thinking first'}]}});
  emit({type:'user',message:{role:'user',content:[{type:'tool_result',tool_use_id:'tool-'+id,content:'synthetic retained output',is_error:false}]}});
  // Provider idle can precede result; acceptance of another user here is a bug.
  emit({type:'system',subtype:'session_state_changed',state:'idle'});
  const wait=()=>{
    if(process.env.CLAUDE_FIXTURE_HOLD&&!existsSync(process.env.CLAUDE_FIXTURE_HOLD)){timer=setTimeout(wait,20);return;}
    finish();
  };
  timer=setTimeout(wait,200);
});
