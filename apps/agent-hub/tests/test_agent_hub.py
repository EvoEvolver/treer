import importlib.util
import json
import os
import sys
import tempfile
import threading
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


class HubTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name).resolve()
        os.chmod(root, 0o700)
        self.root = root
        self.state_path = root / 'fake.json'
        self.write_state({'agents': {}, 'next': 1, 'calls': [], 'sent': [], 'inbox': [], 'acked': [], 'root': str(root)})
        os.environ['FAKE_TREER_STATE'] = str(self.state_path)
        fake = root / 'treer'
        fake.write_text(f'#!/bin/sh\nexec "{sys.executable}" -I "{HERE / "fake_treer.py"}" "$@"\n')
        fake.chmod(0o700)
        self.clock = [1_000_000.0]
        hub.now = lambda: self.clock[0]
        self.store = hub.Store(root / 'state')
        hub.seed(self.store)
        self.hub = hub.Hub(self.store, hub.Cli(str(fake)), {
            'base_url': 'http://svc_hub.treer/', 'address': '/agent_hub', 'app_id': 'app_hub', 'concurrency': 3,
            'stall': 600, 'stop_grace': 120, 'start_timeout': 300, 'max_runtime': 36000,
            'launcher': '/opt/treer/treer-launch', 'folder': '/agent-hub-workers', 'host_root': str(root),
            'jobs_root': str(root / 'state' / 'jobs')})

        def authorize(headers):
            token = headers.get('X-Treer-Identity-Token') or ''
            kind, _, ident = token.partition(':')
            return {'kind': kind, 'id': ident} if kind in ('human', 'agent') and ident else None
        self.http = hub.server(self.hub, '127.0.0.1', 0, authorize, {'https://proxy.example'})
        self.thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()
        self.base = f'http://127.0.0.1:{self.http.server_address[1]}/'

    def tearDown(self):
        self.http.shutdown()
        self.http.server_close()
        self.store.close()
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

    def call(self, method, path, body=None, token=CALLER, headers=None, raw=False):
        data = None
        h = {'X-Treer-Identity-Token': token} if token else {}
        if token and token.startswith('human:'):
            h['X-Hub-Request'] = '1'
        if body is not None:
            data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
            h['Content-Type'] = 'text/markdown' if isinstance(body, str) else 'application/json'
        h.update(headers or {})
        try:
            with urlopen(Request(self.base + path, data=data, headers=h, method=method), timeout=10) as r:
                text = r.read().decode()
                return r.status, (text if raw else json.loads(text))
        except HTTPError as error:
            return error.code, json.loads(error.read().decode())

    def create(self, template='researcher', task='Investigate X', token=CALLER, **inputs):
        status, job = self.call('POST', 'api/jobs', {'template': template, 'task': task, 'inputs': inputs}, token)
        self.assertEqual(status, 201, job)
        return job

    def ready(self, ready=True, status=None):
        def change(state):
            for agent in state['agents'].values():
                agent['ready'] = ready
                if status:
                    agent['status'] = status
        self.edit(change)

    def running_job(self, **kw):
        job = self.create(**kw)
        self.edit(lambda s: s.update(ready_on_create=True))
        self.hub.tick()
        current = self.store.job(job['id'])
        self.assertEqual(current['status'], 'running')
        return current

    # tests
    def test_templates_are_seeded_versioned_and_recorded_by_jobs(self):
        status, data = self.call('GET', 'api/templates')
        names = {t['name'] for t in data['templates']}
        self.assertTrue({'researcher', 'implementer', 'web-implementer', 'launcher-implementer', 'reviewer',
                         'verifier', 'releaser'} <= names)
        status, reviewer = self.call('GET', 'api/templates/reviewer')
        self.assertEqual((reviewer['harness'], reviewer['effort'], reviewer['version']), ('codex', 'high', 1))
        self.assertIn('Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>', reviewer['agent_md'])
        self.assertIn('/api/jobs/<job>/handoff', reviewer['agent_md'])
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

    def test_lifecycle_create_run_handoff_archive_delete_notify(self):
        job = self.create(repo='/Users/dooms/treer', branch='apps-integration', notes='be careful')
        self.assertEqual((job['id'], job['status'], job['requester']), ('j-0001', 'queued', {'id': 'ag_caller', 'kind': 'agent'}))
        self.hub.tick()
        current = self.store.job('j-0001')
        self.assertEqual(current['status'], 'starting', 'interface not ready yet: no prompt')
        agent_id = current['agent_id']
        worker = self.fake()['agents'][agent_id]
        self.assertEqual((worker['name'], worker['folder'], worker['cwd']), ('hub-j-0001', '/agent-hub-workers', 'state/jobs/j-0001'))
        launch = worker['launch']
        self.assertEqual(launch[:3], ['/opt/treer/treer-launch', '--harness', 'claude'])
        instructions = launch[launch.index('--instructions') + 1]
        self.assertEqual(Path(instructions).read_text(), self.store.template('researcher')['agent_md'])
        self.assertTrue((self.root / 'state' / 'jobs' / 'j-0001').is_dir())
        self.ready()
        self.hub.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'running')
        prompts = self.fake()['agents'][agent_id]['prompts']
        self.assertEqual(len(prompts), 1)
        first = prompts[0]['text']
        for expected in ('Investigate X', 'j-0001', '/agent_hub/jobs/j-0001', 'branch: apps-integration', 'be careful',
                         "curl -fsS -X POST -H 'Content-Type: text/markdown' --data-binary @handoff.md http://svc_hub.treer/api/jobs/j-0001/handoff",
                         'status: done'):
            self.assertIn(expected, first)
        self.assertNotIn('Treer working rules', first, 'agent.md goes in as system instructions, not the prompt')
        node = self.call('GET', 'api/tree?path=jobs')[1]['nodes'][0]
        self.assertEqual((node['key'], node['kind']), ('j-0001', 'agent'))
        self.assertIn('message.send', node['capabilities'])
        self.assertEqual(self.call('GET', 'api/jobs/j-0001')[1]['ui_url'], f'https://ag-{agent_id[3:]}.apps.example/')

        status, accepted = self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:' + agent_id)
        self.assertEqual((status, accepted['status']), (200, 'handoff'), accepted)
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:' + agent_id)[0], 409)
        self.hub.tick()
        final = self.store.job('j-0001')
        self.assertEqual((final['status'], final['cleanup'], final['session_id']), ('archived', 'deleted', 'thread-' + agent_id))
        self.assertTrue(self.fake()['agents'][agent_id]['deleted'])
        notice = self.fake()['sent'][-1]
        self.assertEqual(notice['args'][:6], ['message', 'send', '--to', 'ag_caller', '--from-endpoint', 'jobs/j-0001'])
        self.assertIn('j-0001', notice['body'])
        self.assertIn('handoff status: done; Found it.', notice['body'])
        self.assertEqual(self.fake()['wakes'][-1]['target'], 'ag_caller')
        self.assertEqual(final['notice'], 'sent+woken')
        status, text = self.call('GET', 'api/handoffs/j-0001?format=markdown', raw=True)
        self.assertEqual(text, handoff())
        detail = self.call('GET', 'api/handoffs/j-0001')[1]
        self.assertEqual(detail['refs'], {'branch': 'feat/x', 'commits': ['0123456789abcdef', 'abc1234']})
        self.assertEqual(self.call('GET', 'api/jobs/j-0001/transcript')[0], 200)
        view = self.call('GET', 'api/jobs/j-0001')[1]
        self.assertIsNone(view['agent_id'])
        node = self.call('GET', 'api/tree?path=jobs')[1]['nodes'][0]
        self.assertEqual(node['kind'], 'app')
        self.assertEqual(self.call('GET', 'api/tree?path=handoffs')[1]['nodes'][0]['key'], 'j-0001')
        self.hub.tick()
        self.assertEqual(len([s for s in self.fake()['sent']]), 1, 'notice is sent once')

        # Chain: the next job's prompt contains the handoff in full.
        chained = self.create('implementer', 'Implement the finding', handoffs=['j-0001'])
        self.ready()
        self.edit(lambda s: s.update(ready_on_create=True))
        self.hub.tick()
        worker2 = self.store.job(chained['id'])['agent_id']
        text = self.fake()['agents'][worker2]['prompts'][0]['text']
        self.assertIn('## Input handoff j-0001 (researcher@1)', text)
        self.assertIn('unit tests: 3 passed', text)
        self.assertEqual(self.call('POST', 'api/jobs', {'template': 'implementer', 'task': 't', 'inputs': {'handoffs': ['j-0099']}})[0], 409)

    def test_only_the_worker_or_a_human_may_submit_and_handoffs_are_validated(self):
        job = self.running_job()
        worker = job['agent_id']
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:ag_other')[0], 403)
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token=None)[0], 401)
        status, error = self.call('POST', 'api/jobs/j-0001/handoff', handoff(job='j-0002', status='great'), token='agent:' + worker)
        self.assertEqual(status, 400)
        self.assertIn('`job` must be j-0001', error['error']['message'])
        self.assertIn('`status` must be done, partial or blocked', error['error']['message'])
        status, error = self.call('POST', 'api/jobs/j-0001/handoff', '---\njob: j-0001\n---\nhi', token='agent:' + worker)
        self.assertIn('## Next steps', error['error']['message'])
        status, _ = self.call('POST', 'api/jobs/j-0001/handoff', {'markdown': handoff(status='blocked')}, token=HUMAN)
        self.assertEqual(status, 200)
        self.assertEqual(self.store.handoff('j-0001')['submitted_by'], 'user_1')

    def test_concurrency_limit_queues_the_rest(self):
        self.hub.config['concurrency'] = 2
        for n in range(3):
            self.create(task=f'task {n}')
        self.edit(lambda s: s.update(ready_on_create=True))
        self.hub.tick()
        self.assertEqual([self.store.job(f'j-000{n}')['status'] for n in (1, 2, 3)], ['running', 'running', 'queued'])
        worker = self.store.job('j-0001')['agent_id']
        self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:' + worker)
        self.hub.tick()
        self.assertEqual([self.store.job(f'j-000{n}')['status'] for n in (1, 2, 3)], ['archived', 'running', 'running'])
        self.assertEqual(self.call('GET', 'api/jobs?status=running')[1]['jobs'][0]['id'], 'j-0003')
        self.assertEqual(len(self.call('GET', 'api/jobs?requester=me')[1]['jobs']), 3)
        self.assertEqual(self.call('GET', 'api/jobs?status=bogus')[0], 400)

    def test_stop_asks_for_an_interruption_handoff_then_stops(self):
        job = self.running_job()
        worker = job['agent_id']
        status, view = self.call('POST', 'api/jobs/j-0001/stop', {}, token=HUMAN)
        self.assertEqual((status, view['status'], view['stop_requested']), (200, 'running', True))
        last = self.fake()['agents'][worker]['prompts'][-1]
        self.assertTrue(last['steer'])
        self.assertIn('STOP', last['text'])
        self.assertIn('status: partial', last['text'])
        self.hub.tick()
        self.assertEqual(self.store.job('j-0001')['status'], 'running', 'within the grace period')
        self.call('POST', 'api/jobs/j-0001/handoff', handoff(status='partial'), token='agent:' + worker)
        self.hub.tick()
        final = self.store.job('j-0001')
        self.assertEqual((final['status'], final['cleanup']), ('stopped', 'deleted'))
        self.assertIsNotNone(self.store.handoff('j-0001'))

        # No interruption handoff: stopped after the grace period anyway.
        second = self.running_job(task='second')
        self.call('POST', f"api/jobs/{second['id']}/stop", {}, token=CALLER)
        self.clock[0] += 121
        self.hub.tick()
        final = self.store.job(second['id'])
        self.assertEqual((final['status'], final['cleanup']), ('stopped', 'deleted'))
        self.assertIn('no interruption handoff', final['error'])
        self.assertIn(f"{second['id']}", self.fake()['sent'][-1]['body'])
        self.assertIn('no handoff', self.fake()['sent'][-1]['body'])

        # Queued jobs stop immediately and never create an Agent.
        self.hub.config['concurrency'] = 0
        third = self.create(task='third')
        self.call('POST', f"api/jobs/{third['id']}/stop", {}, token=CALLER)
        self.assertEqual(self.store.job(third['id'])['status'], 'stopped')
        self.assertIsNone(self.store.job(third['id'])['agent_id'])

    def test_stalled_or_exited_worker_fails_and_is_archived(self):
        job = self.running_job()
        worker = job['agent_id']
        self.ready(status='idle')
        self.hub.tick()                      # idle observed
        self.clock[0] += 601
        self.hub.tick()                      # reminder
        reminder = self.fake()['agents'][worker]['prompts'][-1]['text']
        self.assertIn('no handoff was received', reminder)
        self.ready(status='idle')
        self.hub.tick()
        self.clock[0] += 601
        self.hub.tick()
        final = self.store.job('j-0001')
        self.assertEqual((final['status'], final['cleanup']), ('failed', 'deleted'))
        self.assertIn('stalled', final['error'])
        self.assertIsNotNone(self.store.one('SELECT transcript FROM archives WHERE job_id=?', ('j-0001',)))
        self.assertIn('failed', self.fake()['sent'][-1]['body'])

        second = self.running_job(task='exits')
        self.ready(status='exited')
        self.hub.tick()
        self.assertEqual(self.store.job(second['id'])['status'], 'failed')
        self.assertTrue(self.fake()['agents'][second['agent_id']]['deleted'])

    def test_failed_creation_and_start_timeout(self):
        self.edit(lambda s: s.update(fail_create=True))
        self.create()
        self.hub.tick()
        job = self.store.job('j-0001')
        self.assertEqual(job['status'], 'failed')
        self.assertIn('agent_create_denied', job['error'])
        self.assertIn('j-0001', self.fake()['sent'][-1]['body'])
        self.edit(lambda s: s.update(fail_create=False))
        self.create(task='never ready')
        self.hub.tick()
        self.clock[0] += 301
        self.hub.tick()
        job = self.store.job('j-0002')
        self.assertEqual((job['status'], job['cleanup']), ('failed', 'deleted'))

    def test_messages_are_forwarded_to_the_live_worker(self):
        queued = self.create()
        status, result = self.call('POST', 'api/jobs/j-0001/message', {'text': 'early note', 'mode': 'queue'})
        self.assertEqual(result['state'], 'pending')
        self.edit(lambda s: s.update(ready_on_create=True))
        self.hub.tick()
        worker = self.store.job(queued['id'])['agent_id']
        prompts = self.fake()['agents'][worker]['prompts']
        self.assertEqual(len(prompts), 2)
        self.assertIn('early note', prompts[1]['text'])
        status, result = self.call('POST', 'api/jobs/j-0001/message', {'text': 'change course', 'mode': 'steer'}, token=HUMAN)
        self.assertEqual(result['state'], 'delivered')
        self.assertEqual(self.fake()['agents'][worker]['prompts'][-1], {
            'text': '[Agent Hub message for job j-0001 from user_1]\nchange course', 'steer': True})
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/message', {'text': 'x', 'mode': 'shout'})[0], 400)

        # A Core Message to /agent_hub/jobs/j-0001 is forwarded the same way, once.
        delivery = {'delivery_id': 'dlv_1', 'recipient': {'kind': 'agent', 'id': 'app_hub', 'app_endpoint': 'jobs/j-0001'},
                    'message': {'message_id': 'msg_1', 'workspace_id': 'ws', 'body': 'via core', 'sender': {'kind': 'agent', 'id': 'ag_peer'}}}
        self.edit(lambda s: s['inbox'].extend([delivery, dict(delivery, delivery_id='dlv_2')]))
        self.hub.poll_messages()
        texts = [p['text'] for p in self.fake()['agents'][worker]['prompts']]
        self.assertEqual(sum('via core' in t for t in texts), 1)
        self.assertEqual(self.fake()['acked'], ['dlv_1', 'dlv_2'])
        self.assertIn('from ag_peer', texts[-1])

        self.call('POST', 'api/jobs/j-0001/handoff', handoff(), token='agent:' + worker)
        self.hub.tick()
        self.assertEqual(self.call('POST', 'api/jobs/j-0001/message', {'text': 'late'})[0], 409)
        late = dict(delivery, delivery_id='dlv_3', message=dict(delivery['message'], message_id='msg_3', body='late'))
        self.edit(lambda s: s['inbox'].append(late))
        self.hub.poll_messages()
        reply = self.fake()['sent'][-1]
        self.assertEqual(reply['args'][3], 'ag_peer')
        self.assertIn('job_finished', reply['body'])

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
        self.assertTrue(all(hub.KEY.fullmatch(n['key']) and n['kind'] == 'app' and 'tree.read' not in n['capabilities']
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
        import http.client
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
        with urlopen(Request(self.base + '?node=templates/researcher', headers={'User-Agent': 'Mozilla/5.0'})) as r:
            self.assertTrue(r.headers['Content-Type'].startswith('text/html'))
            self.assertIn('Agent Hub', r.read().decode())
        with urlopen(self.base + 'health') as r:
            self.assertEqual(json.loads(r.read()), {'ok': True})


if __name__ == '__main__':
    unittest.main()
