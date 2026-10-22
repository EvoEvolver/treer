// Orchestration controls only: fake adapters are NOT native/Policy acceptance.
import test from 'node:test';
import assert from 'node:assert/strict';
import {runColdPolicyRecovery, restoreReport, sameRegistration, scenario} from './cold-policy-recovery.mjs';

function harness(fault) {
  let cold = false, restored = 0, paused = false;
  const calls = [];
  const registration = {workspace_id:'ws', server_id:'a', service_manager:'nohup', credentials_present:true,
    registration_hashes:{0:'a'.repeat(64),1:'b'.repeat(64),2:'c'.repeat(64)}};
  const report = {workspace_id:'ws', server_id:'a',items:[{workload_id:'agent',state:'started'}]};
  const h = {
    calls, workspace:'ws', names: name=>name,
    snapshot: async()=>({servers:[{name:'docker-a',server_id:'a'},{name:'docker-b',server_id:'b'}]}),
    cli: async()=>({agent_id:'agent'}),
    command: async()=>({enabled:fault!=='startup'}),
    api: async (route, body)=> {
      if(route.endsWith('/output')) return {data:{text:'READY cold'}};
      if(route.endsWith('/default-app')) return {data:{provider:{app_id:'policy'}, app:{port:9000}}};
      return {data:{provider:{app_id:'policy',failure_mode:'fail_closed',max_stale_seconds:0},
        cache:{revision: restored < 2 ? (fault==='stale' ? 1 : null) : 7,last_error:null}}};
    },
    docker: async (...args)=> {
      calls.push(args);
      if(args[0]==='pause') paused = true;
      if(args[0]==='unpause') paused = false;
      if(args[0]==='stop') cold = true;
      if(args.includes('restart-controller')) throw {stderr:'Host is not reachable; restore --workspace'};
      if(args.includes('restore')) {
        restored++;
        if(restored===1) {
          if(fault==='unexpected-success') return JSON.stringify(report);
          throw {stderr: fault==='wrong-denial' ? 'access_denied' : 'policy_provider_unavailable identity is unchanged explicitly run'};
        }
        return JSON.stringify({...report, server_id:fault==='identity'?'other':'a'});
      }
      if(args.includes('curl')) return JSON.stringify({service:'treer-policy'});
      return '';
    },
    fingerprint: async()=>({...registration,
      registration_hashes:fault==='registration' && cold ? {...registration.registration_hashes,0:'d'.repeat(64)} : registration.registration_hashes,
      host_reachable:!cold || restored>0,host_record_matches_process:!cold || restored>0,
      proxy_connected:true,host_identity:{pid:42,birth_ticks:'1'}}),
    wait: async(_label,fn)=>assert(await fn()), check:async(_label,fn)=>fn(),
    sleep:async ms=>assert.equal(ms,6000), proxyReady:async()=>{},
    state:()=>({restored,paused}),
  };
  return h;
}
test('two explicit restore calls, unchanged registration and provider release', async()=>{
  const h=harness(); const receipt=await runColdPolicyRecovery(h);
  assert.equal(receipt.real_policy_acceptance,'passed');
  assert.equal(receipt.actual_restore_cli_invocations,2);
  assert.equal(receipt.host_reused_after_explicit_retry,true);
  assert.deepEqual(h.state(),{restored:2,paused:false});
  assert.equal(h.calls.filter(args=>args[0]==='pause').length,1);
});
for(const fault of ['wrong-denial','unexpected-success','stale','registration','identity','startup']) {
  test(`refuse ${fault}; release provider without automatic restore retry`,async()=>{
    const h=harness(fault);
    await assert.rejects(runColdPolicyRecovery(h));
    assert.equal(h.state().paused,false);
    assert.equal(h.state().restored,fault==='identity'?2:['registration','startup'].includes(fault)?0:1);
  });
}
test('report must have exact identity and successful selected workload',()=>{
  const good={workspace_id:'ws',server_id:'a',items:[{workload_id:'job',state:'attached'}]};
  assert.equal(restoreReport(JSON.stringify(good),'ws','a','job').workload_state,'attached');
  for(const bad of [{...good,server_id:'other'},{...good,workspace_id:'other'},
    {...good,items:[]},{...good,items:[{workload_id:'job',state:'failed'}]}]) {
    assert.throws(()=>restoreReport(JSON.stringify(bad),'ws','a','job'));
  }
  assert.throws(()=>restoreReport('not JSON','ws','a','job'));
});
test('unknown scenario and incomplete registration proof refuse',()=>{
  assert.equal(scenario(),'full');
  assert.throws(()=>scenario('remote'));
  const reg={workspace_id:'ws',server_id:'a',registration_hashes:{},credentials_present:true,service_manager:'nohup'};
  assert.throws(()=>sameRegistration(reg,reg));
});
