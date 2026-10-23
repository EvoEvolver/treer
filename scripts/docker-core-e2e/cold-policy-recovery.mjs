// NEW, owned Docker stack only. Never import operator credentials or target an
// enrolled machine. The adapter is shared with the ordinary local e2e harness.
import assert from 'node:assert/strict';

export function scenario(value = 'full') {
  assert(['full', 'cold-policy-recovery-226'].includes(value), 'unknown local e2e scenario');
  return value;
}
export function restoreReport(stdout, workspace, server, workload) {
  const start = stdout.indexOf('{');
  assert(start >= 0, 'missing restore report');
  const report = JSON.parse(stdout.slice(start));
  assert.equal(report.workspace_id, workspace);
  assert.equal(report.server_id, server);
  assert(Array.isArray(report.items));
  const item = report.items.find(i => i.workload_id === workload);
  assert(item && ['started', 'attached'].includes(item.state), 'workload not restored');
  return {workspace_id: workspace, server_id: server, workload_state: item.state};
}
export function sameRegistration(before, after) {
  assert.equal(after.workspace_id, before.workspace_id);
  assert.equal(after.server_id, before.server_id);
  assert.deepEqual(Object.keys(before.registration_hashes).sort(), ['0', '1', '2']);
  assert(Object.values(before.registration_hashes).every(hash => /^[a-f0-9]{64}$/.test(hash)));
  assert.deepEqual(after.registration_hashes, before.registration_hashes);
  assert.equal(after.credentials_present, true);
  assert.equal(after.service_manager, 'nohup');
}
export async function runColdPolicyRecovery(h) {
  const {api, cli, docker, names, snapshot, wait, check, sleep, fingerprint} = h;
  const workspace = h.workspace;
  const servers = (await snapshot()).servers;
  assert(servers.some(s => s.name === 'docker-a') && servers.some(s => s.name === 'docker-b'));
  const a = servers.find(s => s.name === 'docker-a').server_id;
  const b = servers.find(s => s.name === 'docker-b').server_id;
  const ids = servers.map(s => s.server_id).sort();
  const receipt = {schema: 1, scope: 'private_linux_nohup_cold_restore',
    actual_restore_cli_invocations: 0, provider_pause_released: false,
    historical_incident_cause: 'not_established', real_policy_acceptance: 'pending'};
  const machine = names('machine-a');
  const providerMachine = names('machine-b');
  let paused = false;
  try {
    await check('226 real startup declaration and fail-closed Policy fixture', async () => {
      const created = await check('226 create owned startup workload', () => cli('a', 'agent', 'admin', 'create', '--machine', 'docker-a', '--kind', 'command', '--name', 'cold-recovery', '--', 'python3', '/fixtures/worker.py', 'cold', '8200'));
      receipt.workload_id = (created.agent ?? created).agent_id;
      await check('226 owned workload readiness', () => wait('226 startup declaration', async () =>
        (await api(`/api/workspaces/${workspace}/agents/${receipt.workload_id}/output`)).data.text.includes('READY cold')));
      await check('226 verify real startup declaration', async () => {
        assert.equal((await h.command(receipt.workload_id, '226-startup', ['agent', 'startup', 'show'])).enabled, true);
      });
      const installed = await check('226 install real default Policy App', async () =>
        (await api(`/api/workspaces/${workspace}/policy-provider/default-app`, {server_id: b})).data);
      await check('226 validate installed Policy identity', async () => {
        assert(installed.provider?.app_id && Number.isInteger(installed.app?.port));
      });
      receipt.provider_app_id = installed.provider.app_id;
      receipt.provider_port = installed.app.port;
      assert(receipt.provider_port > 0 && receipt.provider_port <= 65535);
      const configured = await check('226 configure real fail-closed Policy', async () => (await api(`/api/workspaces/${workspace}/policy-provider`, {
        app_id: receipt.provider_app_id, failure_mode: 'fail_closed', max_stale_seconds: 0,
      }, {method: 'PUT'})).data);
      assert.equal(configured.provider.failure_mode, 'fail_closed');
      assert.equal(configured.provider.max_stale_seconds, 0);
    });
    const before = await fingerprint();
    assert.equal(before.workspace_id, workspace);
    assert.equal(before.server_id, a);
    assert.equal(before.host_reachable, true);
    assert.equal(before.host_record_matches_process, true);
    assert(Number.isSafeInteger(before.host_identity?.pid) && before.host_identity.pid > 0);
    assert(/^[0-9]+$/.test(before.host_identity.birth_ticks));
    sameRegistration(before, before);
    receipt.registration_hashes = before.registration_hashes;
    // Pause only this new fixture's provider container. Restart both NEW Proxy
    // containers so no old compiled cache can turn provider failure into allow.
    await docker('pause', providerMachine); paused = true;
    for (const side of ['a', 'b']) await docker('restart', names(`proxy-${side}`));
    await h.proxyReady();
    await check('226 cold Host loss and one refused operator restore', async () => {
      await check('226 stop and cold-start owned machine', async () => {
        await docker('stop', '-t', '2', machine);
        await docker('start', machine);
      });
      await check('226 cold registration retained and Host absent', async () => {
        const cold = await fingerprint();
        sameRegistration(before, cold);
        assert.equal(cold.host_reachable, false);
        assert.equal(cold.host_record_matches_process, false);
      });
      await check('226 missing Host restart guidance', async () => {
      let restartFailed = false;
      try { await docker('exec', machine, 'treer-agent-server', 'service', '--workspace', workspace, 'restart-controller'); }
      catch (e) {
        const text = `${e.stdout ?? ''}\n${e.stderr ?? ''}`;
        assert(text.includes('Host is not reachable') && text.includes('restore --workspace'));
        restartFailed = true;
      }
      assert(restartFailed, 'missing Host restart must not silently succeed');
      });
      await check('226 first explicit restore denied by unavailable Policy', async () => {
      let refused = false;
      receipt.actual_restore_cli_invocations++;
      try { await docker('exec', machine, 'treer-agent-server', 'restore', '--workspace', workspace); }
      catch (e) {
        const text = `${e.stdout ?? ''}\n${e.stderr ?? ''}`;
        assert(text.includes('policy_provider_unavailable'));
        assert(text.includes('identity is unchanged') && text.includes('explicitly run'));
        refused = true;
      }
      assert(refused, 'provider outage must not restore through a fallback');
      });
      await check('226 refused restore retains registration and new Host', async () => {
      const denied = await fingerprint();
      sameRegistration(before, denied);
      assert.equal(denied.host_reachable, true);
      assert.equal(denied.host_record_matches_process, true);
      assert.equal(denied.proxy_connected, true);
      receipt.denied_host_identity = denied.host_identity;
      });
      await check('226 unavailable provider has no cached revision', async () => {
      const status = (await api(`/api/workspaces/${workspace}/policy-provider`)).data;
      assert.equal(status.provider.failure_mode, 'fail_closed');
      assert.equal(status.cache.revision, null, 'no stale compiled Policy allowance');
      receipt.first_restore = 'policy_provider_unavailable';
      });
    });
    await docker('unpause', providerMachine); paused = false;
    receipt.provider_pause_released = true;
    await check('226 provider resumes real HTTP health', () => wait('226 actual Policy HTTP readiness', async () => {
      const text = await docker('exec', providerMachine, 'curl', '-fsS', '--max-time', '3', `http://127.0.0.1:${receipt.provider_port}/health`);
      return JSON.parse(text).service === 'treer-policy';
    }));
    // Production caches provider failure for five seconds. This is a bounded
    // wait, not a restore retry; the next mutation is a second explicit call.
    await sleep(6000);
    await check('226 explicit Policy-ready restore retains enrollment and Host', async () => {
      receipt.actual_restore_cli_invocations++;
      const output = await check('226 second explicit restore succeeds', () => docker('exec', machine, 'treer-agent-server', 'restore', '--workspace', workspace));
      await check('226 restored workload report identity', async () => {
        receipt.report = restoreReport(output, workspace, a, receipt.workload_id);
      });
      await check('226 successful restore reuses Host and registration', async () => {
      const after = await fingerprint();
      sameRegistration(before, after);
      assert.equal(after.host_record_matches_process, true);
      assert.deepEqual(after.host_identity, receipt.denied_host_identity);
      assert.equal(after.proxy_connected, true);
      });
      await check('226 recovered Policy revision and machine identities', async () => {
      const status = (await api(`/api/workspaces/${workspace}/policy-provider`)).data;
      assert.equal(status.provider.app_id, receipt.provider_app_id);
      assert.equal(status.provider.failure_mode, 'fail_closed');
      assert(Number.isInteger(status.cache.revision));
      assert.equal(status.cache.last_error, null);
      assert.deepEqual((await snapshot()).servers.map(s => s.server_id).sort(), ids);
      receipt.policy_revision = status.cache.revision;
      });
      receipt.real_policy_acceptance = 'passed';
      receipt.registration_unchanged = true;
      receipt.host_reused_after_explicit_retry = true;
      assert.equal(receipt.actual_restore_cli_invocations, 2);
    });
    return receipt;
  } finally {
    // If an earlier assertion failed, release only our provider container before
    // the outer harness stops its exact owned containers. Never retry restore.
    if (paused) {
      await docker('unpause', providerMachine);
      receipt.provider_pause_released = true;
    }
  }
}
