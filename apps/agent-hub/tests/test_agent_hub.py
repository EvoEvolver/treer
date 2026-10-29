import base64
import http.client
import importlib.util
import json
import os
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('agent_hub', HERE.parent / 'app.py')
hub = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hub)

CALLER = 'agent:ag_caller'
HUMAN = 'human:user_1'


def handoff(job='j-0001', template='researcher', status='done', summary='Found it.'):
    return (f'---\njob: {job}\ntemplate: {template}\nstatus: {status}\nsummary: {summary}\nbranch: feat/x\n'
            'commits: abc1234 0123456789abcdef\n---\n\n## What changed\nnothing\n\n## Verification\n'
            'unit tests: 3 passed\n\n## Open questions\nnone\n\n## Next steps\nimplement it\n')


def gone(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    # A dead child of this process stays a zombie until reaped.
    try:
        return os.waitpid(pid, os.WNOHANG)[0] == pid
    except ChildProcessError:
        return False


def authorize(headers):
    token = headers.get('X-Treer-Identity-Token') or ''
    kind, _, ident = token.partition(':')
    return {'kind': kind, 'id': ident} if kind in ('human', 'agent') and ident else None


class HubTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name).resolve()
        os.chmod(root, 0o700)
        self.root = root
        self.state_path = root / 'fake.json'
        self.write_state({'calls': [], 'sent': [], 'inbox': [], 'acked': [], 'root': str(root)})
        self.launches = root / 'launches'
        self.launches.mkdir()
        self.saved_env = dict(os.environ)
        # The hub runtime's own environment: workers inherit its identity, never its registration/private state.
        os.environ.update({'FAKE_TREER_STATE': str(self.state_path), 'FAKE_LAUNCH_DIR': str(self.launches),
                           'TREER_AGENT_ID': 'ag_hubruntime', 'TREER_WORKLOAD_CREDENTIAL': 'hub-credential',
                           'TREER_AGENT_PRIVATE_STATE_DIR': str(root / 'hub-private'), 'RUST_SPIKE_REGISTER': '1',
                           'AGENT_UI_PORT_FILE': str(root / 'hub-port')})
        self.treer = root / 'treer'
        self.treer.write_text(f'#!/bin/sh\nexec "{sys.executable}" -I "{HERE / "fake_treer.py"}" "$@"\n')
        self.treer.chmod(0o700)
        self.launcher = root / 'treer-launch'
        self.launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" -I "{HERE / "fake_launch.py"}" "$@"\n')
        self.launcher.chmod(0o700)
        self.clock = [1_000_000.0]
        hub.now = lambda: self.clock[0]
        self.store = hub.Store(root / 'state')
        hub.seed(self.store)
        self.config = {
            'base_url': 'http://svc_hub.treer/', 'address': '/agent_hub', 'app_id': 'app_hub', 'concurrency': 3,
            'stall': 600, 'stop_grace': 120, 'start_timeout': 300, 'max_runtime': 36000, 'max_restarts': 1,
            'kill_grace': 3, 'launcher': str(self.launcher), 'host_root': str(root),
            'jobs_root': str(root / 'state' / 'jobs')}
        self.hubs = []
        self.hub, self.http, self.base = self.start_hub()

    def start_hub(self):
        instance = hub.Hub(self.store, hub.Cli(str(self.treer)), dict(self.config))
        http = hub.server(instance, '127.0.0.1', 0, authorize, {'https://proxy.example'})
        instance.config['hub_url'] = f'http://127.0.0.1:{http.server_address[1]}'
        threading.Thread(target=http.serve_forever, daemon=True).start()
        self.hubs.append((instance, http))
        return instance, http, f'http://127.0.0.1:{http.server_address[1]}/'

    def tearDown(self):
        for instance, http in self.hubs:
            instance.shutdown()
            http.shutdown()
            http.server_close()
        for spawn in [s for job in self.launches.iterdir() for s in self.spawns(job.name)]:
            for pid in (spawn['pid'], spawn['grandchild']):
                try:
                    os.kill(pid, 9)
                except ProcessLookupError:
                    pass
        self.store.close()
        os.environ.clear()
        os.environ.update(self.saved_env)
        self.tmp.cleanup()

    # helpers
    def write_state(self, state):
        self.state_path.write_text(json.dumps(state))

    def fake(self):
        return json.loads(self.state_path.read_text())

    def edit(self, fn):
        state = self.fake()
        fn(state)
        self.write_state(state)

    def jsonl(self, job, name):
        path = self.launches / job / name
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def spawns(self, job='j-0001'):
        return self.jsonl(job, 'spawns.jsonl')

    def prompts(self, job='j-0001'):
        return self.jsonl(job, 'prompts.jsonl')

    def control(self, job='j-0001', **values):
        path = self.launches / job / 'control.json'
        path.parent.mkdir(exist_ok=True)
        current = json.loads(path.read_text()) if path.exists() else {}
        current.update(values)
        path.write_text(json.dumps(current))

    def until(self, predicate, what, hub_=None, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            (hub_ or self.hub).tick()
            if predicate():
                return
            time.sleep(0.05)
        self.fail('timed out waiting for ' + what)

    def call(self, method, path, body=None, token=CALLER, headers=None, raw=False, base=None):
        data = None
        h = {'X-Treer-Identity-Token': token} if token else {}
        if token and token.startswith('human:'):
            h['X-Hub-Request'] = '1'
        if body is not None:
            data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
            h['Content-Type'] = 'text/markdown' if isinstance(body, str) else 'application/json'
        h.update(headers or {})
        try:
            with urlopen(Request((base or self.base) + path, data=data, headers=h, method=method), timeout=10) as r:
                text = r.read().decode()
                return r.status, (text if raw else json.loads(text))
        except HTTPError as error:
            text = error.read().decode()
            return error.code, (text if raw else json.loads(text))

    def submit(self, job='j-0001', markdown=None, spawn=-1, base=None, headers=None):
        token = self.spawns(job)[spawn]['env']['TREER_HUB_TOKEN']
        return self.call('POST', f'api/jobs/{job}/handoff', markdown or handoff(job=job), token=None,
                         headers={'X-Hub-Job-Token': token, **(headers or {})}, base=base)

    def create(self, template='researcher', task='Investigate X', token=CALLER, **inputs):
        status, job = self.call('POST', 'api/jobs', {'template': template, 'task': task, 'inputs': inputs}, token)
        self.assertEqual(status, 201, job)
        return job

    def running_job(self, **kw):
        job = self.create(**kw)
        self.until(lambda: self.store.job(job['id'])['status'] == 'running', 'running')
        return self.store.job(job['id'])

    # tests
    def test_templates_are_seeded_versioned_and_recorded_by_jobs(self):
        status, data = self.call('GET', 'api/templates')
        names = {t['name'] for t in data['templates']}
        self.assertTrue({'researcher', 'implementer', 'web-implementer', 'launcher-implementer', 'reviewer',
                         'verifier', 'releaser'} <= names)
        status, reviewer = self.call('GET', 'api/templates/reviewer')
        self.assertEqual((reviewer['harness'], reviewer['effort'], reviewer['version']), ('codex', 'high', 1))
        self.assertIn('Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>', reviewer['agent_md'])
        self.assertIn('$TREER_HUB_URL/api/jobs/$TREER_HUB_JOB/handoff', reviewer['agent_md'])
        status, updated = self.call('PUT', 'api/templates/reviewer', {'agent_md': '# Reviewer v2\nBe brief.\n'})
        self.assertEqual((status, updated['version'], updated['harness']), (200, 2, 'codex'))
        self.assertEqual([v['version'] for v in updated['versions']], [2, 1])
        self.assertEqual(self.call('GET', 'api/templates/reviewer?version=1')[1]['version'], 1)
        self.assertEqual(self.call('PUT', 'api/templates/reviewer', {'agent_md': '# Reviewer v2\nBe brief.\n'})[1]['version'], 2)
        self.assertEqual(self.call('POST', 'api/templates', {'name': 'reviewer', 'agent_md': 'x'})[0], 409)
        self.assertEqual(self.call('POST', 'api/templates', {'name': '../x', 'agent_md': 'x'})[0], 400)
        self.assertEqual(self.call('PUT', 'api/templates/reviewer', {'harness': 'acp'})[0], 400)
        job = self.create('reviewer@1')
        self.assertEqual(job['template'], 'reviewer@1')
        self.assertEqual(self.create('reviewer')['template'], 'reviewer@2')
        hub.seed(self.store)   # a restart never overwrites edits
        self.assertEqual(self.store.template('reviewer')['version'], 2)
        # An unedited bundled template follows the bundled file as a new version.
        self.store.put_template('verifier', {'agent_md': '# old bundled text\n'}, 'seed')
        hub.seed(self.store)
        verifier = self.store.template('verifier')
        self.assertEqual((verifier['version'], verifier['created_by']), (3, 'seed'))
        self.assertEqual(verifier['agent_md'], hub.template_file(HERE.parent / 'templates' / 'verifier.md')['agent_md'])

    def test_lifecycle_spawn_run_handoff_archive_kill_notify(self):
        job = self.create(repo='/Users/dooms/treer', branch='apps-integration', notes='be careful')
        self.assertEqual((job['id'], job['status'], job['requester']), ('j-0001', 'queued', {'id': 'ag_caller', 'kind': 'agent'}))
        self.control(ready=False)
        self.hub.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'starting', 'interface not ready yet: no prompt')
        self.until(lambda: self.spawns(), 'spawn')
        spawn = self.spawns()[0]
        argv = spawn['argv'][1:]
        self.assertEqual(argv[:2], ['--harness', 'claude'])
        state = self.root / 'state'
        self.assertEqual(Path(argv[argv.index('--instructions') + 1]).read_text(), self.store.template('researcher')['agent_md'])
        self.assertIn('--no-register', argv)
        self.assertEqual(argv[argv.index('--state-dir') + 1], str(state / 'workers' / 'j-0001' / 'launcher'))
        self.assertEqual(argv[argv.index('--listen') + 1], '127.0.0.1:0')
        self.assertEqual(argv[argv.index('--port-file') + 1], str(state / 'workers' / 'j-0001' / 'port'))
        self.assertNotIn('--session-id', argv)
        self.assertEqual(spawn['cwd'], str(state / 'jobs' / 'j-0001'))
        self.assertEqual(spawn['pgid'], spawn['pid'], 'the worker leads its own process group')
        env = spawn['env']
        self.assertEqual((env['TREER_HUB_URL'], env['TREER_HUB_JOB']), (self.base.rstrip('/'), 'j-0001'))
        self.assertGreater(len(env['TREER_HUB_TOKEN']), 30)
        self.assertEqual((env['TREER_AGENT_ID'], env['TREER_WORKLOAD_CREDENTIAL']), ('ag_hubruntime', 'hub-credential'),
                         'the worker uses treer as the hub')
        self.assertEqual((env['TREER_AGENT_PRIVATE_STATE_DIR'], env['RUST_SPIKE_REGISTER']), (None, None))
        self.assertEqual(env['GIT_AUTHOR_NAME'], 'agent_hub/jobs/j-0001')
        self.assertEqual(self.store.job('j-0001')['token_hash'], hub.digest(env['TREER_HUB_TOKEN']))
        self.assertFalse(any(c[:2] == ['agent', 'admin'] for c in self.fake()['calls']), 'no Core Agent is created')
        self.assertEqual(self.prompts(), [])
        self.control(ready=True)
        self.until(lambda: self.store.job('j-0001')['status'] == 'running', 'running')
        prompts = self.prompts()
        self.assertEqual([(p['operation_id'], p['delivery']) for p in prompts], [('hub-j-0001-first', 'queue')])
        first = prompts[0]['text']
        for expected in ('Investigate X', 'j-0001', '/agent_hub/jobs/j-0001', 'branch: apps-integration', 'be careful',
                         hub.Hub.HANDOFF_CMD, 'status: done', '$TREER_HUB_TOKEN'):
            self.assertIn(expected, first)
        self.assertNotIn(env['TREER_HUB_TOKEN'], first, 'the secret stays in the environment')
        self.assertNotIn('Treer working rules', first, 'agent.md goes in as system instructions, not the prompt')
        node = self.call('GET', 'api/tree?path=jobs')[1]['nodes'][0]
        self.assertEqual((node['key'], node['kind']), ('j-0001', 'agent'))
        self.assertEqual(node['capabilities'], ['message.send', 'message.read', 'page.read'])
        view = self.call('GET', 'api/jobs/j-0001')[1]
        self.assertEqual(view['worker'], {'running': True, 'pid': spawn['pid'], 'restarts': 0, 'ui': 'workers/j-0001/'})
        self.assertEqual(view['session_id'], spawn['session'])

        # The job token: its own record and handoff, over loopback, never from a browser.
        token = {'X-Hub-Job-Token': env['TREER_HUB_TOKEN']}
        self.assertEqual(self.call('GET', 'api/jobs/j-0001', token=None, headers=token)[1]['id'], 'j-0001')
        self.assertEqual(self.call('GET', 'api/jobs', token=None, headers=token)[0], 403)
        self.assertEqual(self.call('GET', 'api/jobs/j-0001', token=None, headers={'X-Hub-Job-Token': 'wrong'})[0], 401)
        self.assertEqual(self.submit(headers={'Origin': 'https://proxy.example'})[0], 403)
        status, accepted = self.submit()
        self.assertEqual((status, accepted['status']), (200, 'handoff'), accepted)
        self.assertEqual(self.submit()[0], 409)
        self.hub.tick()
        final = self.store.job('j-0001')
        self.assertEqual((final['status'], final['session_id'], final['worker_pid']), ('archived', spawn['session'], None))
        self.until(lambda: gone(spawn['pid']) and gone(spawn['grandchild']), 'worker process group gone')
        self.assertEqual(self.store.handoff('j-0001')['submitted_by'], 'worker:j-0001')
        self.assertEqual(sorted(p.name for p in (state / 'workers' / 'j-0001').iterdir()), ['launcher.log'])
        notice = self.fake()['sent'][-1]
        self.assertEqual(notice['args'][:6], ['message', 'send', '--to', 'ag_caller', '--from-endpoint', 'jobs/j-0001'])
        self.assertIn('handoff status: done; Found it.', notice['body'])
        self.assertEqual(self.fake()['wakes'][-1]['target'], 'ag_caller')
        self.assertEqual(final['notice'], 'sent+woken')
        self.assertEqual(self.call('GET', 'api/handoffs/j-0001?format=markdown', raw=True)[1], handoff())
        detail = self.call('GET', 'api/handoffs/j-0001')[1]
        self.assertEqual(detail['refs'], {'branch': 'feat/x', 'commits': ['0123456789abcdef', 'abc1234']})
        transcript = self.call('GET', 'api/jobs/j-0001/transcript')[1]
        self.assertEqual(transcript['session_id'], spawn['session'])
        self.assertIn('Investigate X', transcript['entries'][0]['content'])
        view = self.call('GET', 'api/jobs/j-0001')[1]
        self.assertEqual(view['worker']['running'], False)
        self.assertEqual(self.call('GET', 'api/jobs/j-0001', token=None, headers=token)[0], 401, 'the token dies with the worker')
        node = self.call('GET', 'api/tree?path=jobs')[1]['nodes'][0]
        self.assertEqual((node['kind'], node['capabilities']), ('app', ['message.read', 'page.read']))
        self.assertEqual(self.call('GET', 'api/tree?path=handoffs')[1]['nodes'][0]['key'], 'j-0001')
        self.hub.tick()
        self.assertEqual(len(self.fake()['sent']), 1, 'notice is sent once')

        # Chain: the next job's prompt contains the handoff in full.
        chained = self.create('implementer', 'Implement the finding', handoffs=['j-0001'])
        self.until(lambda: self.prompts(chained['id']), 'chained prompt')
        text = self.prompts(chained['id'])[0]['text']
        self.assertIn('## Input handoff j-0001 (researcher@1)', text)
        self.assertIn('unit tests: 3 passed', text)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'implementer', 'task': 't', 'inputs': {'handoffs': ['j-0099']}})[0], 409)

    def test_only_the_worker_or_a_human_may_submit_and_handoffs_are_validated(self):
        self.running_job()
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:ag_other')[0], 403)
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token=None)[0], 401)
        status, error = self.submit(markdown=handoff(job='j-0002', status='great'))
        self.assertEqual(status, 400)
        self.assertIn('`job` must be j-0001', error['error']['message'])
        self.assertIn('`status` must be done, partial or blocked', error['error']['message'])
        status, error = self.submit(markdown='---\njob: j-0001\n---\nhi')
        self.assertIn('## Next steps', error['error']['message'])
        other = self.running_job(task='other')
        self.assertEqual(self.submit(job=other['id'], markdown=handoff(), spawn=0)[0], 400, 'job j-0002 token, j-0001 handoff')
        token = self.spawns(other['id'])[0]['env']['TREER_HUB_TOKEN']
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token=None,
                                   headers={'X-Hub-Job-Token': token})[0], 401, 'another job\'s token')
        status, _ = self.call('POST', 'api/jobs/j-0001/handoff', {'markdown': handoff(status='blocked')}, token=HUMAN)
        self.assertEqual(status, 200)
        self.assertEqual(self.store.handoff('j-0001')['submitted_by'], 'user_1')

    def test_large_job_descriptions_go_through_a_file(self):
        self.running_job(task='T' * 60000, notes='N' * 30000)
        text = self.prompts()[0]['text']
        self.assertLess(len(text), 1000)
        path = text.split('`')[1]
        self.assertIn('T' * 60000, Path(path).read_text())
        self.assertIn(hub.Hub.HANDOFF_CMD, Path(path).read_text())

    def test_concurrency_limit_queues_the_rest(self):
        self.hub.config['concurrency'] = 2
        for n in range(3):
            self.create(task=f'task {n}')
        self.until(lambda: [self.store.job(f'j-000{n}')['status'] for n in (1, 2)] == ['running', 'running'], 'two running')
        self.assertEqual(self.store.job('j-0003')['status'], 'queued')
        self.submit('j-0001')
        self.until(lambda: self.store.job('j-0003')['status'] == 'running', 'third runs')
        self.assertEqual(self.store.job('j-0001')['status'], 'archived')
        self.assertEqual(self.call('GET', 'api/jobs?status=running')[1]['jobs'][0]['id'], 'j-0003')
        self.assertEqual(len(self.call('GET', 'api/jobs?requester=me')[1]['jobs']), 3)
        self.assertEqual(self.call('GET', 'api/jobs?status=bogus')[0], 400)

    def test_stop_asks_for_an_interruption_handoff_then_stops(self):
        self.running_job()
        status, view = self.call('POST', 'api/jobs/j-0001/stop', {}, token=HUMAN)
        self.assertEqual((status, view['status'], view['stop_requested']), (200, 'running', True))
        last = self.prompts()[-1]
        self.assertEqual((last['delivery'], last['operation_id']), ('steer', 'hub-j-0001-stop'))
        self.assertIn('STOP', last['text'])
        self.assertIn('status: partial', last['text'])
        self.hub.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'running', 'within the grace period')
        self.submit(markdown=handoff(status='partial'))
        self.hub.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'stopped')
        self.assertIsNotNone(self.store.handoff('j-0001'))
        self.until(lambda: gone(self.spawns()[0]['pid']), 'stopped worker gone')

        # No interruption handoff: stopped after the grace period anyway.
        second = self.running_job(task='second')
        self.call('POST', f"api/jobs/{second['id']}/stop", {}, token=CALLER)
        self.clock[0] += 121
        self.hub.tick()
        final = self.store.job(second['id'])
        self.assertEqual(final['status'], 'stopped')
        self.assertIn('no interruption handoff', final['error'])
        self.until(lambda: gone(self.spawns(second['id'])[0]['pid']), 'second worker gone')
        self.assertIn('no handoff', self.fake()['sent'][-1]['body'])

        # Queued jobs stop immediately and never spawn a worker.
        self.hub.config['concurrency'] = 0
        third = self.create(task='third')
        self.call('POST', f"api/jobs/{third['id']}/stop", {}, token=CALLER)
        self.assertEqual(self.store.job(third['id'])['status'], 'stopped')
        self.assertEqual(self.spawns(third['id']), [])

    def test_stalled_worker_is_reminded_then_fails_and_is_archived(self):
        self.running_job()
        self.control(status='idle')
        self.hub.tick()                      # idle observed
        self.clock[0] += 601
        self.hub.tick()                      # reminder
        reminder = self.prompts()[-1]
        self.assertEqual(reminder['operation_id'], 'hub-j-0001-nudge')
        self.assertIn('no handoff was received', reminder['text'])
        self.hub.tick()
        self.clock[0] += 601
        self.hub.tick()
        final = self.store.job('j-0001')
        self.assertEqual(final['status'], 'failed')
        self.assertIn('stalled', final['error'])
        self.assertIn('no handoff was received', self.store.one('SELECT transcript FROM archives WHERE job_id=?', ('j-0001',))[0])
        self.assertIn('failed', self.fake()['sent'][-1]['body'])
        self.until(lambda: gone(self.spawns()[0]['pid']), 'stalled worker gone')

    def test_worker_crash_is_restarted_on_its_session_then_fails(self):
        self.running_job()
        first = self.spawns()[0]
        self.control(exit=3)
        self.until(lambda: len(self.spawns()) == 2 and self.store.job('j-0001')['status'] == 'running', 'respawned')
        second = self.spawns()[1]
        self.assertEqual(second['argv'][second['argv'].index('--session-id') + 1], first['session'])
        self.assertEqual(second['session'], first['session'])
        self.assertNotEqual(second['env']['TREER_HUB_TOKEN'], first['env']['TREER_HUB_TOKEN'])
        self.until(lambda: gone(first['grandchild']), 'crashed worker group cleaned up')
        ops = [p['operation_id'] for p in self.prompts()]
        self.assertEqual(ops[0], 'hub-j-0001-first')
        self.assertTrue(ops[1].startswith('hub-j-0001-resume-'), ops)
        self.assertIn('restarted your worker process', self.prompts()[1]['text'])
        self.assertEqual(self.store.job('j-0001')['restarts'], 1)
        self.control(exit=4)
        self.until(lambda: self.store.job('j-0001')['status'] == 'failed', 'failed after the restart limit')
        self.assertIn('worker exited (code 4)', self.store.job('j-0001')['error'])
        self.assertEqual(len(self.spawns()), 2)
        self.until(lambda: gone(second['grandchild']), 'second group cleaned up')

    def test_start_failures_and_start_timeout(self):
        self.hub.config['launcher'] = str(self.root / 'missing-launcher')
        self.create()
        self.hub.tick()
        job = self.store.job('j-0001')
        self.assertEqual(job['status'], 'failed')
        self.assertIn('worker start failed', job['error'])
        self.assertIn('j-0001', self.fake()['sent'][-1]['body'])
        self.hub.config['launcher'] = str(self.launcher)
        self.control('j-0002', ready=False)
        self.create(task='never ready')
        self.until(lambda: self.spawns('j-0002'), 'spawned')
        self.clock[0] += 301
        self.until(lambda: self.store.job('j-0002')['status'] == 'failed', 'start timeout')
        self.assertIn('never became ready', self.store.job('j-0002')['error'])
        self.until(lambda: gone(self.spawns('j-0002')[0]['pid']), 'unready worker gone')

    def test_messages_are_forwarded_to_the_live_worker(self):
        queued = self.create()
        status, result = self.call('POST', 'api/jobs/j-0001/message', {'text': 'early note', 'mode': 'queue'})
        self.assertEqual(result['state'], 'pending')
        self.until(lambda: len(self.prompts()) == 2, 'pending message delivered')
        self.assertIn('early note', self.prompts()[1]['text'])
        status, result = self.call('POST', 'api/jobs/j-0001/message', {'text': 'change course', 'mode': 'steer'}, token=HUMAN)
        self.assertEqual(result['state'], 'delivered')
        last = self.prompts()[-1]
        self.assertEqual((last['text'], last['delivery']), ('[Agent Hub message for job j-0001 from user_1]\nchange course', 'steer'))
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/message', {'text': 'x', 'mode': 'shout'})[0], 400)

        # A Core Message to /agent_hub/jobs/j-0001 is forwarded the same way, once.
        delivery = {'delivery_id': 'dlv_1', 'recipient': {'kind': 'agent', 'id': 'app_hub', 'app_endpoint': 'jobs/j-0001'},
                    'message': {'message_id': 'msg_1', 'workspace_id': 'ws', 'body': 'via core', 'sender': {'kind': 'agent', 'id': 'ag_peer'}}}
        self.edit(lambda s: s['inbox'].extend([delivery, dict(delivery, delivery_id='dlv_2')]))
        self.hub.poll_messages()
        texts = [p['text'] for p in self.prompts()]
        self.assertEqual(sum('via core' in t for t in texts), 1)
        self.assertEqual(self.fake()['acked'], ['dlv_1', 'dlv_2'])
        self.assertIn('from ag_peer', texts[-1])
        steer = dict(delivery, delivery_id='dlv_s', message=dict(delivery['message'], message_id='msg_s',
                                                                 body=json.dumps({'text': 'now', 'mode': 'steer'})))
        self.edit(lambda s: s['inbox'].append(steer))
        self.hub.poll_messages()
        self.assertEqual((self.prompts()[-1]['delivery'], self.prompts()[-1]['text'].splitlines()[-1]), ('steer', 'now'))

        self.submit()
        self.hub.tick()
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/message', {'text': 'late'})[0], 409)
        late = dict(delivery, delivery_id='dlv_3', message=dict(delivery['message'], message_id='msg_3', body='late'))
        self.edit(lambda s: s['inbox'].append(late))
        self.hub.poll_messages()
        reply = self.fake()['sent'][-1]
        self.assertEqual(reply['args'][3], 'ag_peer')
        self.assertIn('job_finished', reply['body'])
        self.assertEqual(queued['id'], 'j-0001')

    def test_hub_restart_stops_orphans_and_resumes_workers_on_their_sessions(self):
        self.running_job()
        first = self.spawns()[0]
        # The hub process dies without stopping its worker; a new hub process starts on the same state.
        hub2, http2, base2 = self.start_hub()
        hub2.recover()
        self.assertTrue(gone(first['pid']), 'the orphaned worker was stopped')
        self.assertEqual(self.store.job('j-0001')['status'], 'starting')
        self.until(lambda: self.store.job('j-0001')['status'] == 'running', 'resumed', hub_=hub2)
        second = self.spawns()[1]
        self.assertEqual(second['argv'][second['argv'].index('--session-id') + 1], first['session'])
        self.assertEqual(second['env']['TREER_HUB_URL'], base2.rstrip('/'))
        self.assertEqual([p['operation_id'] for p in self.prompts()][0], 'hub-j-0001-first')
        self.assertEqual(sum(p['operation_id'] == 'hub-j-0001-first' for p in self.prompts()), 1, 'never re-sent')
        self.assertEqual(self.store.job('j-0001')['restarts'], 0, 'a hub restart is not a worker crash')
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token=None, base=base2,
                                   headers={'X-Hub-Job-Token': first['env']['TREER_HUB_TOKEN']})[0], 401)
        self.assertEqual(self.submit(base=base2)[0], 200)
        hub2.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'archived')
        self.until(lambda: gone(second['pid']), 'resumed worker gone', hub_=hub2)

        # A clean hub stop ends its workers; the next start resumes them.
        self.create(task='again')
        self.until(lambda: self.store.job('j-0002')['status'] == 'running', 'second job runs', hub_=hub2)
        third = self.spawns('j-0002')[0]
        hub2.shutdown()
        self.assertTrue(gone(third['pid']))
        hub3, _, _ = self.start_hub()
        hub3.recover()
        self.until(lambda: len(self.spawns('j-0002')) == 2 and self.store.job('j-0002')['status'] == 'running',
                   'resumed after a clean stop', hub_=hub3)

    def test_legacy_tree_agent_jobs_are_failed_and_their_agents_deleted(self):
        job = self.create()
        self.store.update_job(job['id'], status='running', agent_id='ag_legacy', started_at=self.clock[0], prompted=1)
        self.hub.recover()
        final = self.store.job(job['id'])
        self.assertEqual((final['status'], final['cleanup']), ('failed', 'deleted'))
        self.assertEqual(self.fake()['deleted'], ['ag_legacy'])

    def test_worker_ui_reverse_proxy_http_and_websocket(self):
        self.running_job()
        spawn = self.spawns()[0]
        self.assertEqual(self.call('GET', 'workers/j-0001/', token=None, raw=True)[0], 401)
        connection = http.client.HTTPConnection('127.0.0.1', self.http.server_address[1], timeout=10)
        connection.request('GET', '/workers/j-0001?theme=dark', headers={'X-Treer-Identity-Token': HUMAN})
        response = connection.getresponse()
        response.read()
        self.assertEqual((response.status, response.getheader('Location')), (308, 'j-0001/?theme=dark'))
        connection.close()
        browser = {'Origin': 'https://proxy.example', 'Cookie': 'session=secret', 'Sec-Fetch-Site': 'same-origin',
                   'Sec-Fetch-Mode': 'navigate', 'X-Treer-Interface-Source': 'proxy-ui', 'Referer': 'https://proxy.example/x'}
        status, page = self.call('GET', 'workers/j-0001/', token=HUMAN, headers=browser, raw=True)
        self.assertEqual(status, 200)
        self.assertIn('<base href="./">', page)
        self.assertEqual(self.call('GET', 'workers/j-0001/ui.js', token=HUMAN, raw=True), (200, '// fake'))
        seen = self.jsonl('j-0001', 'requests.jsonl')[-2]
        self.assertEqual(seen['path'], '/')
        lower = {k.lower(): v for k, v in seen['headers'].items()}
        self.assertEqual(lower['host'], f"127.0.0.1:{spawn['port']}")
        for header in ('origin', 'cookie', 'sec-fetch-site', 'sec-fetch-mode', 'x-treer-identity-token',
                       'x-treer-interface-source', 'referer', 'x-hub-request'):
            self.assertNotIn(header, lower)
        # Worker UI mutations: same-site browser requests only, no X-Hub-Request needed.
        body = {'prompt': 'from the embedded UI'}
        self.assertEqual(self.call('POST', 'workers/j-0001/api/prompt', body, token=HUMAN,
                                   headers={'X-Hub-Request': '0', 'Sec-Fetch-Site': 'cross-site'})[0], 403)
        self.assertEqual(self.call('POST', 'workers/j-0001/api/prompt', body, token=HUMAN,
                                   headers={'X-Hub-Request': '0', 'Sec-Fetch-Site': 'same-origin',
                                            'Origin': 'https://proxy.example'})[0], 200)
        self.assertEqual(self.jsonl('j-0001', 'ui-prompts.jsonl'), [body])

        # WebSocket upgrade through the hub path.
        sock = socket.create_connection(('127.0.0.1', self.http.server_address[1]), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        sock.sendall((f'GET /workers/j-0001/ws HTTP/1.1\r\nHost: hub\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                      f'Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: https://proxy.example\r\n'
                      f'X-Treer-Identity-Token: {HUMAN}\r\n\r\n').encode())
        received = b''
        while b'\r\n\r\n' not in received:
            received += sock.recv(4096)
        head, _, rest = received.partition(b'\r\n\r\n')
        self.assertTrue(head.startswith(b'HTTP/1.1 101'), head)

        def frame():
            nonlocal rest
            while len(rest) < 2 or len(rest) < 2 + rest[1]:
                rest += sock.recv(4096)
            data, rest = rest[2:2 + rest[1]], rest[2 + rest[1]:]
            return data
        self.assertEqual(json.loads(frame()), {'type': 'state', 'session': spawn['session']})
        mask = os.urandom(4)
        payload = b'hello'
        sock.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))
        self.assertEqual(frame(), b'echo:hello')
        sock.close()
        upgrade = [r for r in self.jsonl('j-0001', 'requests.jsonl') if r['path'] == '/ws'][-1]
        self.assertNotIn('origin', {k.lower() for k in upgrade['headers']})

        self.submit()
        self.hub.tick()
        status, error = self.call('GET', 'workers/j-0001/', token=HUMAN)
        self.assertEqual((status, error['error']['code']), (503, 'worker_not_running'))
        self.assertEqual(self.call('GET', 'workers/j-0099/', token=HUMAN)[0], 404)

    def test_core_message_to_the_hub_root_creates_a_job_once(self):
        body = json.dumps({'template': 'researcher', 'task': 'from a message'})
        delivery = {'delivery_id': 'dlv_a', 'recipient': {'kind': 'agent', 'id': 'app_hub'},
                    'message': {'message_id': 'msg_a', 'workspace_id': 'ws', 'body': body, 'sender': {'kind': 'agent', 'id': 'ag_req'}}}
        self.edit(lambda s: s['inbox'].extend([delivery, dict(delivery, delivery_id='dlv_b')]))
        self.hub.poll_messages()
        jobs = self.store.jobs()
        self.assertEqual(len(jobs), 1)
        self.assertEqual((jobs[0]['requester_id'], jobs[0]['task']), ('ag_req', 'from a message'))
        self.assertIn('job j-0001 queued', self.fake()['sent'][0]['body'])
        bad = dict(delivery, delivery_id='dlv_c', message=dict(delivery['message'], message_id='msg_c', body='{"template":"nope","task":"x"}'))
        prose = dict(delivery, delivery_id='dlv_d', message=dict(delivery['message'], message_id='msg_d', body='please help'))
        mine = dict(delivery, delivery_id='dlv_e', message=dict(delivery['message'], message_id='msg_e', sender={'kind': 'agent', 'id': 'app_hub'}))
        self.edit(lambda s: s['inbox'].extend([bad, prose, mine]))
        self.hub.poll_messages()
        bodies = [s['body'] for s in self.fake()['sent']]
        self.assertIn('template_not_found', bodies[1])
        self.assertIn('send a JSON job', bodies[2])
        self.assertEqual(len(bodies), 3, 'the hub never answers itself')
        self.assertEqual(len(self.store.jobs()), 1)

    def test_tree_contract_and_authentication(self):
        root = self.call('GET', 'api/tree?path=')[1]
        self.assertEqual(root['schema_version'], 1)
        self.assertEqual(root['path'], [])
        self.assertEqual([n['key'] for n in root['nodes']], ['templates', 'jobs', 'handoffs'])
        self.assertTrue(all(n['capabilities'] == ['tree.read'] and n['kind'] == 'app' for n in root['nodes']))
        templates = self.call('GET', 'api/tree?path=templates')[1]
        self.assertEqual(templates['path'], ['templates'])
        self.assertTrue(all(hub.KEY.fullmatch(n['key']) and n['kind'] == 'app' and n['capabilities'] == ['page.read']
                            for n in templates['nodes']))
        self.assertEqual(self.call('GET', 'api/tree?path=templates/researcher')[0], 404)
        self.assertEqual(self.call('GET', 'api/tree?path=', token=None)[0], 401)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'researcher', 'task': 'x'}, token=HUMAN,
                                   headers={'X-Hub-Request': '0'})[0], 403)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'researcher', 'task': 'x'}, token=HUMAN,
                                   headers={'Origin': 'https://evil.example'})[0], 403)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'researcher', 'task': 'x'}, token=HUMAN,
                                   headers={'Origin': 'https://proxy.example', 'Sec-Fetch-Site': 'same-origin'})[0], 201)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'researcher', 'task': 'x', 'extra': 1})[0], 400)
        self.assertEqual(self.call('GET', 'api/me')[1]['principal'], {'id': 'ag_caller', 'kind': 'agent'})

    def test_identity_is_verified_by_the_proxy_for_this_service(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        seen = []

        class Proxy(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                seen.append(body)
                kind, _, sub = body['token'].partition(':')
                claims = {'sub': sub, 'principal_kind': kind, 'service_id': 'svc_hub', 'workspace_id': 'ws_1'}
                if kind == 'other':
                    claims.update(principal_kind='agent', service_id='svc_other')
                raw = json.dumps({'active': kind != 'inactive', 'claims': claims}).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def log_message(self, *_):
                pass
        proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        try:
            verify = hub.Identity(f'http://127.0.0.1:{proxy.server_address[1]}/', 'svc_hub', 'ws_1')
            self.assertEqual(verify({'X-Treer-Identity-Token': 'agent:ag_w'}), {'id': 'ag_w', 'kind': 'agent'})
            self.assertEqual(verify({'X-Treer-Identity-Token': 'human:user_1'}), {'id': 'user_1', 'kind': 'human'})
            self.assertEqual(seen[0], {'token': 'agent:ag_w', 'audience': 'svc_hub'})
            self.assertEqual(verify({'X-Treer-Identity-Token': 'agent:ag_w'}), {'id': 'ag_w', 'kind': 'agent'})
            self.assertEqual(len(seen), 2, 'a verified token is remembered briefly')
            self.assertIsNone(verify({'X-Treer-Identity-Token': 'other:ag_x'}))
            self.assertIsNone(verify({'X-Treer-Identity-Token': 'inactive:ag_x'}))
            self.assertIsNone(verify({}))
            # The runtime proxy sends the same token as Bearer; any other Authorization is refused.
            self.assertIsNotNone(verify({'X-Treer-Identity-Token': 'agent:ag_w', 'Authorization': 'Bearer agent:ag_w'}))
            self.assertIsNone(verify({'X-Treer-Identity-Token': 'agent:ag_w', 'Authorization': 'Bearer app-token'}))
        finally:
            proxy.shutdown()
            proxy.server_close()

    def test_keep_alive_requests_drain_unused_bodies(self):
        self.hub.config['concurrency'] = 0
        self.create()
        connection = http.client.HTTPConnection('127.0.0.1', self.http.server_address[1], timeout=10)
        headers = {'X-Treer-Identity-Token': HUMAN, 'X-Hub-Request': '1', 'Content-Type': 'application/json'}
        connection.request('POST', '/api/jobs/j-0001/stop', body='{}', headers=headers)
        self.assertEqual(connection.getresponse().read() and 200, 200)
        connection.request('GET', '/api/jobs/j-0001', headers=headers)
        response = connection.getresponse()
        self.assertEqual((response.status, json.loads(response.read())['status']), (200, 'stopped'))
        connection.close()

    def test_root_negotiates_manual_and_ui(self):
        with urlopen(Request(self.base, headers={'Accept': 'text/markdown'})) as r:
            manual = r.read().decode()
        self.assertIn('http://svc_hub.treer/api/jobs', manual)
        self.assertIn('--to /agent_hub', manual)
        self.assertNotIn('HUB_URL', manual)
        with urlopen(Request(self.base + '?node=jobs/j-0001', headers={'User-Agent': 'Mozilla/5.0'})) as r:
            self.assertTrue(r.headers['Content-Type'].startswith('text/html'))
            page = r.read().decode()
        self.assertIn('Agent Hub', page)
        self.assertIn('id="worker"', page, 'the job page embeds the worker UI')
        with urlopen(self.base + 'health') as r:
            self.assertEqual(json.loads(r.read()), {'ok': True})


if __name__ == '__main__':
    unittest.main()
