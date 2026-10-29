// Synthetic producer, no provider installation/home/config or network access.
import { createInterface } from 'node:readline';
import { appendFileSync, existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
const args=process.argv.slice(2), session=args.at(-1);
if(process.env.CLAUDE_FIXTURE_ARGV_LOG)appendFileSync(process.env.CLAUDE_FIXTURE_ARGV_LOG,JSON.stringify(args)+'\n');
if(process.env.CLAUDE_FIXTURE_SPAWN_LOG)appendFileSync(process.env.CLAUDE_FIXTURE_SPAWN_LOG,'spawned\n');
const emit=event=>process.stdout.write(JSON.stringify({session_id:session,...event})+'\n');
let active=false, timer;
let model='claude-sonnet-test';
let modelChanges=0;
let effort=null;
const models=[{value:'default',resolvedModel:'claude-sonnet-test'},{value:'sonnet',resolvedModel:'claude-sonnet-test',displayName:'Sonnet from provider'},{value:'opus',resolvedModel:'claude-opus-test'},{value:'legacy',resolvedModel:'claude-legacy-test'}];
if(process.env.CLAUDE_FIXTURE_EFFORTS)for(const option of models)if(option.value!=='legacy'){option.supportsEffort=true;option.supportedEffortLevels=['low','medium','high',' HIGH ','invalid','low'];}
const finish=()=>{active=false;emit({type:'result',subtype:'success',is_error:false,result:'synthetic answer'});};
createInterface({input:process.stdin}).on('line',async line=>{
  const event=JSON.parse(line);
  if(event.type==='control_request'){
    if(process.env.CLAUDE_FIXTURE_GOALS){
      appendFileSync(process.env.CLAUDE_FIXTURE_LOG+'.controls',JSON.stringify(event)+'\n');
      if(event.request.subtype==='initialize'){
        emit({type:'control_response',response:{subtype:'success',request_id:event.request_id,response:{commands:process.env.CLAUDE_FIXTURE_NO_GOAL?[]:[{name:'goal'}]}}});return;
      }
    }
    if(process.env.CLAUDE_FIXTURE_MODELS){
      appendFileSync(process.env.CLAUDE_FIXTURE_LOG+'.controls',JSON.stringify(event)+'\n');
      const reply=(subtype,response={})=>emit({type:'control_response',response:{subtype,request_id:event.request_id,response,error:subtype==='error'?'synthetic model refused':undefined}});
      if(event.request.subtype==='initialize'){reply('success',process.env.CLAUDE_FIXTURE_NO_MODELS?{}:{models});return;}
      if(event.request.subtype==='apply_flag_settings'){
        const probe=!Object.hasOwn(event.request.settings,'effortLevel');
        if(process.env.CLAUDE_FIXTURE_NO_EFFORT){reply('error');return;}
        if(probe){setTimeout(()=>reply('success'),Number(process.env.CLAUDE_FIXTURE_PROBE_DELAY||0));return;}
        const next=event.request.settings.effortLevel;
        if(next===process.env.CLAUDE_FIXTURE_REFUSE_EFFORT){reply('error');return;}
        effort=next;
        if(active&&process.env.CLAUDE_FIXTURE_MODEL_BURST)for(let i=0;i<64;i++)emit({type:'stream_event',event:{type:'content_block_delta',index:0,delta:{type:'text_delta',text:''}}});
        if(process.env.CLAUDE_FIXTURE_WRONG_MODEL_ACK)emit({type:'control_response',response:{subtype:'success',request_id:'999999',response:{}}});
        setTimeout(()=>reply('success'),next===process.env.CLAUDE_FIXTURE_TIMEOUT_EFFORT?18000:Number(process.env.CLAUDE_FIXTURE_EFFORT_DELAY||0));return;
      }
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
  if(event.type==='control_response'){appendFileSync(process.env.CLAUDE_FIXTURE_LOG+'.replies',JSON.stringify(event)+'\n');return;}
  if(event.type!=='user'||active&&!process.env.CLAUDE_FIXTURE_STEER)process.exit(71);
  const text=typeof event.message.content==='string'?event.message.content:event.message.content.filter(b=>b.type==='text').map(b=>b.text).join('\n');
  const images=Array.isArray(event.message.content)?event.message.content.filter(b=>b.type==='image').map(b=>{
    if(b.source.type!=='base64')process.exit(72);
    const bytes=Buffer.from(b.source.data,'base64');return {mime:b.source.media_type,bytes:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')};
  }):[];
  appendFileSync(process.env.CLAUDE_FIXTURE_LOG,JSON.stringify({type:active?'steered':'accepted',resume:args.includes('--resume'),session,...(process.env.CLAUDE_FIXTURE_MODELS||process.env.CLAUDE_FIXTURE_STEER?{model,text,uuid:event.uuid}:{}),...(process.env.CLAUDE_FIXTURE_EFFORTS?{effort}:{}),...(process.env.CLAUDE_FIXTURE_IMAGES?{images}:{})})+'\n');
  if(active){emit({type:'user',uuid:event.uuid,message:event.message});return;}
  active=true;
  if(process.env.CLAUDE_FIXTURE_GOALS&&text.startsWith('/goal ')){
    const args=text.slice(6), id='goal-'+event.uuid;
    const ack=()=>{
      if(!process.env.CLAUDE_FIXTURE_GOAL_NO_ACK)emit({type:'assistant',uuid:id,message:{model:'<synthetic>',id,content:[{type:'text',text:args==='clear'?'Goal cleared':`Goal set: ${args}`}]},local_command_source:'<local-command-stdout>native</local-command-stdout>',local_command_run:{command:'goal',args}});
      if(process.env.CLAUDE_FIXTURE_GOAL_COMPLETION){
        const completion=condition=>process.env.CLAUDE_FIXTURE_GOAL_HOOK?{type:'system',subtype:'stop_hook_summary',hook_infos:[{prompt_text:condition}],has_output:false,prevented_continuation:false,hook_errors:[]}:{type:'attachment',attachment:{type:'goal_status',condition,status:'met'}};
        emit(completion('unrelated condition'));
        setTimeout(()=>emit(completion(args)),120);
      }
      if(process.env.CLAUDE_FIXTURE_GOAL_ACK_GATE){
        const done=()=>{if(!existsSync(process.env.CLAUDE_FIXTURE_GOAL_ACK_GATE)){timer=setTimeout(done,20);return;}finish();};timer=setTimeout(done,20);
      } else timer=setTimeout(finish,200);
    };
    timer=setTimeout(ack,Number(process.env.CLAUDE_FIXTURE_GOAL_ACK_DELAY||0));return;
  }
  if(process.env.CLAUDE_FIXTURE_TASKS){
    if(text==='finish task')emit({type:'system',subtype:'task_notification',task_id:'running-shell',status:'completed',summary:'finished with exit code 0'});
    else {
      emit({type:'system',subtype:'task_started',task_id:'running-shell',task_type:'local_bash',description:'Native running shell',status:'running',output_file:process.cwd()+'/task.output'});
      emit({type:'system',subtype:'task_updated',task_id:'running-shell',patch:{description:'Updated native shell',summary:'still working'}});
      emit({type:'system',subtype:'background_tasks_changed',tasks:[
        {task_id:'finished-agent',task_type:'local_agent',description:'Native finished agent',status:'success',output_file:process.cwd()+'/task.output'},
        {task_id:'outside',description:'Outside',status:'running',output_file:process.cwd()+'/../outside'},
        {task_id:'symlink',status:'running',output_file:process.cwd()+'/link.output'},
        {task_id:'hardlink',status:'running',output_file:process.cwd()+'/hard.output'},
      ]});
      emit({type:'system',subtype:'background_tasks_changed',tasks:Array.from({length:101},(_,i)=>({task_id:'refused-'+i,status:'running'}))});
      if(process.env.CLAUDE_FIXTURE_TASK_ASYNC){
        const notify=()=>{
          if(!existsSync(process.env.CLAUDE_FIXTURE_TASK_ASYNC)){setTimeout(notify,20);return;}
          emit({type:'system',subtype:'task_notification',task_id:'running-shell',status:'completed',summary:'async task exit code 0'});
        };setTimeout(notify,20);
      }
    }
  }
  while(process.env.CLAUDE_FIXTURE_INPUT_GATE&&!existsSync(process.env.CLAUDE_FIXTURE_INPUT_GATE))await new Promise(r=>setTimeout(r,20));
  if(process.env.CLAUDE_FIXTURE_ODDITIES){
    emit({type:'frame_type_from_a_newer_claude',detail:{nested:true}});
    emit({type:'control_request',request_id:'perm-1',request:{subtype:'can_use_tool',tool_name:'Bash',input:{command:'synthetic'}}});
  }
  if(process.env.CLAUDE_FIXTURE_CRASH_ONCE&&!existsSync(process.env.CLAUDE_FIXTURE_CRASH_ONCE)){
    appendFileSync(process.env.CLAUDE_FIXTURE_CRASH_ONCE,'crashed');process.exit(9);
  }
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
