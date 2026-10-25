// Same-machine NEW Docker fixture only; no provider prestart or Policy bypass.
import assert from 'node:assert/strict';
import {sameRegistration, restoreReport, policyObservation} from './cold-policy-recovery.mjs';

export async function runSameMachinePolicyRecovery(h) {
  const {api, cli, docker, names, snapshot, wait, check, sleep, fingerprint} = h;
  const workspace = h.workspace;
  const machine = names('machine-a');
  const server = (await snapshot()).servers.find(s => s.name === 'docker-a').server_id;
  const receipt = {schema:1, scope:'private_linux_same_machine_policy_cold_restore',
    real_policy_acceptance:'pending', actual_restore_cli_invocations:0,
    historical_incident_cause:'not_established', events:[]};
  const start = performance.now();
  const event = async stage => {
    receipt.events.push({stage, elapsed_ms:Math.floor(performance.now()-start)});
    await h.observeSameMachine?.(receipt);
  };
  const created = await check('226 same-machine create declared workload', () => cli('a', 'agent', 'admin', 'create', '--machine', 'docker-a', '--kind', 'command', '--name', 'same-machine-cold', '--', 'python3', '/fixtures/worker.py', 'same-cold', '8200'));
  receipt.workload_id = (created.agent ?? created).agent_id;
  await check('226 same-machine workload startup ready', async () => {
    await wait('same-machine workload', async () => (await api(`/api/workspaces/${workspace}/agents/${receipt.workload_id}/output`)).data.text.includes('READY same-cold'));
    assert.equal((await h.command(receipt.workload_id,'same-startup',['agent','startup','show'])).enabled,true);
  });
  const installed = await check('226 same-machine install real Policy App', async () =>
    (await api(`/api/workspaces/${workspace}/policy-provider/default-app`, {server_id:server})).data);
  assert.equal(installed.app.server_id,server);
  assert(installed.provider?.app_id && Number.isInteger(installed.app.port));
  receipt.provider_app_id = installed.provider.app_id;
  receipt.provider_port = installed.app.port;
  assert(receipt.provider_port>0 && receipt.provider_port<=65535);
  await check('226 same-machine configure fail-closed zero stale', async () => {
    const configured = (await api(`/api/workspaces/${workspace}/policy-provider`, {app_id:receipt.provider_app_id,failure_mode:'fail_closed',max_stale_seconds:0},{method:'PUT'})).data;
    assert.equal(configured.provider.failure_mode,'fail_closed');
    assert.equal(configured.provider.max_stale_seconds,0);
  });
  const before = await fingerprint();
  assert.equal(before.server_id,server);
  sameRegistration(before,before);
  assert.equal(before.host_record_matches_process,true);
  receipt.registration_hashes = before.registration_hashes;
  await event('configured_same_machine');
  await check('226 same-machine cold stop/start and discard Proxy caches', async () => {
    await docker('stop','-t','2',machine);
    for (const side of ['a','b']) await docker('restart',names(`proxy-${side}`));
    await h.proxyReady();
    await docker('start',machine);
    const cold = await fingerprint();
    sameRegistration(before,cold);
    assert.equal(cold.host_reachable,false);
    assert.equal(cold.host_record_matches_process,false);
  });
  await event('cold_host_absent');
  let firstOutput;
  await check('226 same-machine first explicit restore classified', async () => {
    receipt.actual_restore_cli_invocations++;
    await event('first_restore_invoked');
    try {
      firstOutput = await docker('exec',machine,'treer-agent-server','restore','--workspace',workspace);
      receipt.report = restoreReport(firstOutput,workspace,server,receipt.workload_id);
      receipt.first_restore = 'succeeded';
    } catch (error) {
      const text = `${error.stdout ?? ''}\n${error.stderr ?? ''}`;
      assert(text.includes('policy_provider_unavailable') && text.includes('identity is unchanged') && text.includes('explicitly run'), 'unexpected first restore failure');
      receipt.first_restore = 'policy_provider_unavailable';
    }
  });
  await event('first_restore_returned');
  const first = await fingerprint();
  sameRegistration(before,first);
  assert.equal(first.host_record_matches_process,true);
  assert.equal(first.proxy_connected,true);
  receipt.first_host_identity = first.host_identity;
  await check('226 same-machine normal startup makes provider ready', () => wait('same-machine Policy health', async () => {
    // Observation only: never launch the provider or issue runtime restore here.
    const text = await docker('exec',machine,'curl','-fsS','--max-time','3',`http://127.0.0.1:${receipt.provider_port}/health`);
    return JSON.parse(text).service === 'treer-policy';
  }));
  await event('provider_http_ready');
  if (receipt.first_restore === 'policy_provider_unavailable') {
    await sleep(6000); // bounded failure-cache expiry, not a retry loop
    await check('226 same-machine second explicit restore succeeds', async () => {
      receipt.actual_restore_cli_invocations++;
      await event('second_restore_invoked');
      const output = await docker('exec',machine,'treer-agent-server','restore','--workspace',workspace);
      receipt.report = restoreReport(output,workspace,server,receipt.workload_id);
    });
  }
  await check('226 same-machine registration Host and real Policy retained', async () => {
    const after = await fingerprint();
    sameRegistration(before,after);
    assert.equal(after.host_record_matches_process,true);
    assert.equal(after.proxy_connected,true);
    assert.deepEqual(after.host_identity,receipt.first_host_identity);
    const response = await api(`/api/workspaces/${workspace}/policy-provider`,undefined,{raw:true});
    await h.observePolicy?.(policyObservation(response,'recovered'));
    assert.equal(response.status,200);
    assert.equal(response.data.provider.app_id,receipt.provider_app_id);
    assert.equal(response.data.provider.failure_mode,'fail_closed');
    assert.equal(response.data.provider.max_stale_seconds,0);
    assert(Number.isSafeInteger(response.data.cache.revision));
    assert.equal(response.data.cache.last_error,null);
    receipt.policy_revision = response.data.cache.revision;
    assert.equal(receipt.actual_restore_cli_invocations,receipt.first_restore==='succeeded'?1:2);
  });
  receipt.registration_unchanged = true;
  receipt.host_reused_after_explicit_retry = true;
  receipt.real_policy_acceptance = 'passed';
  await event('completed');
  return receipt;
}
