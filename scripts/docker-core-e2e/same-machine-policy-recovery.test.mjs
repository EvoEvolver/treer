import test from 'node:test';
import assert from 'node:assert/strict';
import {runSameMachinePolicyRecovery} from './same-machine-policy-recovery.mjs';

function fixture(fault, firstSuccess=false) {
  let cold=false, calls=0, ready=false;
  const events=[], commands=[];
  const registration={workspace_id:'ws',server_id:'a',registration_hashes:{0:'a'.repeat(64),1:'b'.repeat(64),2:'c'.repeat(64)},credentials_present:true,service_manager:'nohup'};
  const h={workspace:'ws',names:s=>s,
    snapshot:async()=>({servers:[{name:'docker-a',server_id:'a'},{name:'docker-b',server_id:'b'}]}),
    cli:async()=>({agent_id:'owned'}), command:async()=>({enabled:true}),
    api:async(route,body)=> {
      if(route.endsWith('/output'))return {data:{text:'READY same-cold'}};
      if(route.endsWith('/default-app')) {assert.equal(body.server_id,'a');return {data:{app:{server_id:'a',port:8787},provider:{app_id:'policy'}}};}
      return {status:200,data:{provider:{app_id:'policy',failure_mode:'fail_closed',max_stale_seconds:0},cache:{revision:1,last_error:null}}};
    },
    check:async(_label,fn)=>fn(), wait:async(_label,fn)=>assert(await fn()),
    sleep:async ms=>assert.equal(ms,6000),proxyReady:async()=>{},
    fingerprint:async()=>({...registration,registration_hashes:fault==='identity'&&cold?{...registration.registration_hashes,0:'d'.repeat(64)}:registration.registration_hashes,
      host_reachable:!cold||calls>0,host_record_matches_process:!cold||calls>0,proxy_connected:true,
      host_identity:{pid:fault==='host'&&calls>1?99:42,birth_ticks:'1'}}),
    observeSameMachine:async r=>events.push(r.events.at(-1).stage),
    docker:async(...args)=> {
      commands.push(args);
      if(args[0]==='stop')cold=true;
      if(args.includes('restore')) {
        calls++;
        if(calls===1&&!firstSuccess)throw {stderr:fault==='denial'?'policy_denied':'policy_provider_unavailable identity is unchanged explicitly run'};
        return JSON.stringify({workspace_id:'ws',server_id:'a',items:[{workload_id:'owned',state:'attached'}]});
      }
      if(args.includes('curl')) {ready=true;if(fault==='unready')throw Error('synthetic unavailable');return '{"service":"treer-policy"}';}
      return '';
    },
  };
  return {h,events,commands,read:()=>({calls,ready})};
}
for(const firstSuccess of [false,true])test(`normal same-machine startup firstSuccess=${firstSuccess}`,async()=>{
  const f=fixture(null,firstSuccess);const r=await runSameMachinePolicyRecovery(f.h);
  assert.equal(r.real_policy_acceptance,'passed');assert.equal(r.actual_restore_cli_invocations,firstSuccess?1:2);
  assert(f.events.indexOf('first_restore_invoked')<f.events.indexOf('provider_http_ready'));
  if(!firstSuccess)assert(f.events.indexOf('provider_http_ready')<f.events.indexOf('second_restore_invoked'));
  assert(!f.commands.some(c=>c[0]==='pause'||c[0]==='unpause'));
  assert.deepEqual(f.commands.filter(c=>c[0]==='restart').map(c=>c[1]),['proxy-a','proxy-b']);
});
for(const fault of ['denial','identity','host','unready'])test(`same-machine refuses ${fault} without extra restore`,async()=>{
  const f=fixture(fault);await assert.rejects(runSameMachinePolicyRecovery(f.h));
  assert(f.read().calls <= (fault==='host'?2:1));
});
