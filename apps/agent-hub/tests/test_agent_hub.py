import importlib.util
import json
import os
import sqlite3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import tempfile
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import urlopen

SPEC = importlib.util.spec_from_file_location('agent_hub', Path(__file__).parents[1] / 'app.py')
hub = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hub)


def delivery(mid='msg_1', did='delivery_1', endpoint='team/tools/agent1', body='hello', sender_endpoint=None):
    sender = {'kind': 'agent', 'id': 'ag_sender'}
    if sender_endpoint:
        sender['app_endpoint'] = sender_endpoint
    return {'delivery_id': did, 'recipient': {'kind': 'agent', 'id': 'ag_hub', 'app_endpoint': endpoint},
            'message': {'message_id': mid, 'workspace_id': 'ws_test', 'sender': sender, 'body': body}}


class HubTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        os.chmod(self.tmp.name, 0o700)
        self.open()
        for k, kind, handler in [('team', 'app', None), ('team/tools', 'app', None),
                                 ('team/tools/agent1', 'agent', 'echo'), ('team/tools/agent2', 'agent', 'reverse')]:
            self.store.register(k, kind, k, handler)

    def open(self):
        self.store = hub.Store(self.tmp.name, 'ws_test', 'ag_hub')

    def reopen(self):
        self.store.close()
        self.open()

    def tearDown(self):
        self.store.close()
        self.tmp.cleanup()

    def test_recursive_tree_uses_live_sqlite_and_exact_parent(self):
        root = self.store.tree()
        self.assertEqual(root, {'schema_version': 1, 'path': [], 'nodes': [
            {'key': 'team', 'kind': 'app', 'name': 'team', 'capabilities': ['tree.read']}]})
        self.assertEqual(self.store.tree('team')['nodes'][0]['key'], 'tools')
        self.assertEqual(self.store.tree('team/tools')['path'], ['team', 'tools'])
        self.assertEqual([n['key'] for n in self.store.tree('team/tools')['nodes']], ['agent1', 'agent2'])
        self.store.remove('team/tools/agent1')
        self.assertEqual([n['key'] for n in self.store.tree('team/tools')['nodes']], ['agent2'])
        self.reopen()
        self.assertEqual(len(self.store.tree('team/tools')['nodes']), 1)

    def test_path_escape_cycles_and_invalid_registration_refused(self):
        for path in ['../x', 'team//x', '/team', 'team/', 'a/' * 8 + 'a', 'team/%2e%2e', 'team/é']:
            with self.assertRaises(ValueError):
                hub.key(path)
        with self.assertRaises(ValueError):
            self.store.register('missing/agent', 'agent', 'Agent', 'echo')
        with self.assertRaises(ValueError):
            self.store.register('team/tools/agent1/child', 'agent', 'Agent', 'echo')
        with self.assertRaises(ValueError):
            self.store.register('team', 'agent', 'Agent', 'echo')

    def test_empty_subtree_and_removed_subapp(self):
        self.store.register('team/empty', 'app', 'Empty')
        self.assertEqual(self.store.tree('team/empty')['nodes'], [])
        self.store.remove('team/tools')
        with self.assertRaises(ValueError):
            self.store.tree('team/tools')
        self.assertEqual([n['key'] for n in self.store.tree('team')['nodes']], ['empty'])

    def test_durable_admission_before_ack_and_replay(self):
        item = delivery()
        self.assertEqual(self.store.admit(item, hub.normalize), 'delivery_1')
        self.reopen()  # Death after COMMIT, before ACK.
        self.store.admit(item, hub.normalize)
        output = []
        self.assertTrue(self.store.step(output.append))
        self.assertFalse(self.store.step(output.append))
        self.assertEqual(len(output), 1)
        self.assertEqual(output[0]['body'], hub.REPLY_MARKER + '[team/tools/agent1] hello')
        self.reopen()
        self.store.admit(item, hub.normalize)
        self.assertFalse(self.store.step(output.append))
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM jobs').fetchone()[0], 1)
        self.assertEqual(self.store.db.execute('SELECT request,response FROM jobs').fetchone(), (None, None))

    def test_two_endpoints_same_message_do_not_collapse(self):
        self.store.admit(delivery(), hub.normalize)
        self.store.admit(delivery(did='delivery_2', endpoint='team/tools/agent2'), hub.normalize)
        output = []
        self.store.step(output.append); self.store.step(output.append)
        self.assertEqual([o['body'] for o in output], [hub.REPLY_MARKER + '[team/tools/agent1] hello', hub.REPLY_MARKER + '[team/tools/agent2] olleh'])
        self.assertNotEqual(output[0]['idempotency_key'], output[1]['idempotency_key'])

    def test_outbox_timeout_restart_uses_exact_request_key(self):
        self.store.admit(delivery(sender_endpoint='other/tools/agent'), hub.normalize)
        attempts = []
        def fail_after_acceptance(response):
            attempts.append(response)
            raise TimeoutError('lost receipt')
        with self.assertRaises(TimeoutError):
            self.store.step(fail_after_acceptance)
        self.reopen()
        self.store.step(attempts.append)
        self.assertEqual(attempts[0], attempts[1])
        self.assertEqual(attempts[0]['recipient'], 'ag_sender/other/tools/agent')
        self.assertEqual(attempts[0]['sender_endpoint'], 'team/tools/agent1')
        self.assertEqual(attempts[0]['context'], 'msg_1')

    def test_unknown_key_is_failure_reply_and_not_dropped(self):
        self.store.admit(delivery(endpoint='team/tools/missing'), hub.normalize)
        replies = []
        self.store.step(replies.append)
        self.assertEqual(replies[0]['body'], hub.REPLY_MARKER + '[app_endpoint_not_found] team/tools/missing')

    def test_accepted_handler_snapshot_survives_removal(self):
        self.store.admit(delivery(), hub.normalize)
        self.store.remove('team')
        replies = []
        self.store.step(replies.append)
        self.assertEqual(replies[0]['body'], hub.REPLY_MARKER + '[team/tools/agent1] hello')

    def test_poison_counter_bounded_and_healthy_not_blocked(self):
        for i in range(35):
            item = delivery(did='bad_' + str(i))
            item['recipient']['app_endpoint'] = '../escape'
            self.assertEqual(self.store.admit(item, hub.normalize), 'bad_' + str(i))
        self.assertEqual(self.store.db.execute('SELECT attempts FROM poison').fetchone()[0], 35)
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM poison').fetchone()[0], 1)
        self.store.admit(delivery(), hub.normalize)
        self.assertTrue(self.store.step(lambda _: None))

    def test_conflicting_message_or_delivery_never_overwrites(self):
        self.store.admit(delivery(), hub.normalize)
        with self.assertRaises(ValueError):
            self.store.admit(delivery(body='changed'), hub.normalize)
        with self.assertRaises(ValueError):
            self.store.admit(delivery(mid='msg_other'), hub.normalize)
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM jobs').fetchone()[0], 1)

    def test_wrong_workspace_and_wrong_app_cannot_execute(self):
        for field in ['workspace', 'app']:
            item = delivery(did=field)
            if field == 'workspace':
                item['message']['workspace_id'] = 'ws_other'
            else:
                item['recipient']['id'] = 'ag_other'
            self.store.admit(item, hub.normalize)
        self.assertFalse(self.store.step(lambda _: self.fail('must not execute')))

    def test_human_reply_uses_actual_authenticated_human_id(self):
        item = delivery()
        item['message']['sender'] = {'kind': 'human', 'id': 'usr_owner'}
        self.store.admit(item, hub.normalize)
        replies = []
        self.store.step(replies.append)
        self.assertEqual(replies[0]['recipient'], 'usr_owner')

    def test_pending_capacity_records_busy_and_can_ack_without_execution(self):
        old = hub.MAX_PENDING
        hub.MAX_PENDING = 1
        try:
            self.store.admit(delivery(), hub.normalize)
            self.assertEqual(self.store.admit(delivery(mid='msg_2', did='delivery_2'), hub.normalize), 'delivery_2')
            self.assertEqual(self.store.stats()['counters']['busy'], 1)
            self.assertEqual(self.store.db.execute('SELECT count(*) FROM deliveries').fetchone()[0], 1)
            self.store.step(lambda _: None)
            self.store.admit(delivery(mid='msg_2', did='delivery_2'), hub.normalize)
            self.assertEqual(self.store.db.execute('SELECT count(*) FROM jobs').fetchone()[0], 2)
        finally:
            hub.MAX_PENDING = old

    def test_process_lock_prevents_second_dispatcher(self):
        with self.assertRaises(BlockingIOError):
            hub.Store(self.tmp.name, 'ws_test', 'ag_hub')

    def test_sql_rollback_never_acks_unstored_job(self):
        self.store.db.execute("CREATE TRIGGER refuse BEFORE INSERT ON deliveries BEGIN SELECT RAISE(ABORT,'injected'); END")
        with self.assertRaises(Exception):
            self.store.admit(delivery(), hub.normalize)
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM jobs').fetchone()[0], 0)

    def test_failed_reply_does_not_starve_other_endpoint(self):
        self.store.admit(delivery(), hub.normalize)
        self.store.admit(delivery(mid='msg_2', did='delivery_2', endpoint='team/tools/agent2'), hub.normalize)
        def fail(_):
            raise RuntimeError('policy_denied')
        with self.assertRaises(RuntimeError):
            self.store.step(fail)
        replies = []
        self.store.step(replies.append)
        self.assertEqual(replies[0]['sender_endpoint'], 'team/tools/agent2')
        self.assertEqual(self.store.db.execute("SELECT count(*) FROM jobs WHERE state='sent'").fetchone()[0], 1)

    def test_poll_ack_observes_committed_admission(self):
        store = self.store
        path = str(Path(self.tmp.name) / 'state.sqlite')
        class Cli:
            def run(self, args, body=None):
                if args[1] == 'receive':
                    return {'deliveries': [delivery()]}
                if args[1] == 'ack':
                    with sqlite3.connect(path) as other:
                        if other.execute('SELECT count(*) FROM jobs').fetchone()[0] != 1:
                            raise AssertionError('ACK before durable admission')
                    return {}
                raise AssertionError(args)
            def send(self, _):
                pass
        hub.poll_once(store, Cli())
        self.assertEqual(store.db.execute('SELECT acked FROM deliveries').fetchone()[0], 1)

    def test_actual_http_identity_verifier_fails_closed(self):
        claims = {'service_id': 'svc_hub', 'workspace_id': 'ws_test', 'principal_kind': 'human', 'sub': 'usr_owner'}
        reply = {'active': True, 'claims': claims}
        class Verifier(BaseHTTPRequestHandler):
            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                self.server.observed = (self.path, request)
                raw = json.dumps(reply).encode()
                self.send_response(200)
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers(); self.wfile.write(raw)
            def log_message(self, *_):
                pass
        upstream = ThreadingHTTPServer(('127.0.0.1', 0), Verifier)
        thread = threading.Thread(target=upstream.serve_forever); thread.start()
        identity = hub.Identity('http://127.0.0.1:' + str(upstream.server_port), 'svc_hub', 'ws_test')
        try:
            self.assertFalse(identity({}))
            self.assertFalse(identity({'Authorization': 'Bearer cookie-substitute'}))
            self.assertTrue(identity({'X-Treer-Identity-Token': 'synthetic-test-only'}))
            self.assertEqual(upstream.observed, ('/.treer/apps/identity/verify', {'token': 'synthetic-test-only', 'audience': 'svc_hub'}))
            for field, wrong in [('workspace_id', 'ws_other'), ('service_id', 'svc_other'), ('principal_kind', 'agent'), ('sub', '')]:
                old = claims[field]; claims[field] = wrong
                self.assertFalse(identity({'X-Treer-Identity-Token': 'synthetic-test-only'}))
                claims[field] = old
            reply['active'] = False
            self.assertFalse(identity({'X-Treer-Identity-Token': 'synthetic-test-only'}))
        finally:
            upstream.shutdown(); upstream.server_close(); thread.join()

    def test_live_operator_registration_and_http_auth_refusal(self):
        http = hub.server(self.store, '127.0.0.1', 0, lambda _: False)
        thread = threading.Thread(target=http.serve_forever); thread.start()
        try:
            with self.assertRaises(HTTPError) as caught:
                urlopen('http://127.0.0.1:' + str(http.server_port) + '/api/tree')
            self.assertEqual(caught.exception.code, 401)
            other = hub.Store(self.tmp.name, 'ws_test', 'ag_hub', worker=False)
            try:
                other.register('team/new', 'agent', 'New', 'echo')
                self.assertIn('new', [n['key'] for n in self.store.tree('team')['nodes']])
                other.remove('team/new')
                self.assertNotIn('new', [n['key'] for n in self.store.tree('team')['nodes']])
            finally:
                other.close()
        finally:
            http.shutdown(); http.server_close(); thread.join()

    def test_same_app_endpoint_and_marked_cross_app_reply_terminate(self):
        # App B responds once; App A consumes the response without another send.
        item = delivery()
        item['message']['sender'] = {'id': 'ag_other', 'kind': 'agent', 'app_endpoint': 'team/tools/agent2'}
        self.store.admit(item, hub.normalize)
        replies = []
        self.store.step(replies.append)
        self.assertEqual(len(replies), 1)
        response = delivery(mid='msg_reply', did='delivery_reply', body=replies[0]['body'])
        response['message']['sender'] = {'id': 'ag_other', 'kind': 'agent', 'app_endpoint': 'team/tools/agent2'}
        self.store.admit(response, hub.normalize)
        self.store.step(lambda _: self.fail('reply ping-pong'))
        same = delivery(mid='msg_self', did='delivery_self')
        same['message']['sender'] = {'id': 'ag_hub', 'kind': 'agent', 'app_endpoint': 'team/tools/agent2'}
        self.store.admit(same, hub.normalize)
        self.store.step(lambda _: self.fail('same-App loop'))
        self.assertEqual(self.store.stats()['counters']['reply_suppressed'], 2)

    def test_reply_context_terminates_even_if_body_marker_removed(self):
        self.store.admit(delivery(), hub.normalize)
        self.store.step(lambda _: {'message': {'message_id': 'msg_published'}})
        item = delivery(mid='msg_next', did='delivery_next')
        item['message']['context_ids'] = ['msg_published']
        self.store.admit(item, hub.normalize)
        self.store.step(lambda _: self.fail('context reply amplifier'))
        self.assertEqual(self.store.stats()['counters']['reply_suppressed'], 1)

    def test_10001_sender_flood_does_not_block_healthy_sender(self):
        # Actual SQLite admissions/ACK updates, not a mocked count threshold.
        for i in range(10001):
            item = delivery(mid='flood_' + str(i), did='flood_d_' + str(i))
            did = self.store.admit(item, hub.normalize)
            self.store.acked(did)
        self.assertLessEqual(self.store.stats()['jobs'], hub.SENDER_PENDING)
        healthy = delivery(mid='healthy', did='healthy_d', endpoint='team/tools/agent2')
        healthy['message']['sender']['id'] = 'ag_healthy'
        self.store.admit(healthy, hub.normalize)
        output = []
        while self.store.step(output.append):
            pass
        self.assertTrue(any(r['recipient'] == 'ag_healthy' for r in output))
        self.assertEqual(self.store.stats()['counters']['sender_limited'], 10001 - hub.SENDER_PENDING)
        self.reopen()
        self.assertEqual(self.store.stats()['counters']['sender_limited'], 10001 - hub.SENDER_PENDING)

    def test_10001_terminal_jobs_retire_only_after_confirmed_ack(self):
        now = [1000000]
        self.store.clock = lambda: now[0]
        for i in range(10001):
            # Advance the local test clock only; no host clock/DB timestamp edits.
            now[0] += hub.SENDER_WINDOW + 1
            mid, did = 'complete_' + str(i), 'complete_d_' + str(i)
            self.store.admit(delivery(mid=mid, did=did), hub.normalize)
            self.store.acked(did)
            self.store.step(lambda _: None)
        stats = self.store.stats()
        self.assertLessEqual(stats['jobs'], hub.RECENT_TERMINAL + 1)
        self.assertLessEqual(stats['deliveries'], hub.RECENT_TERMINAL + 1)
        self.assertGreater(stats['counters']['retired'], 9800)
        self.reopen()
        self.assertEqual(self.store.stats(), stats)
        item = delivery(mid='after_retirement', did='after_retirement_d')
        item['message']['sender']['id'] = 'ag_healthy'
        self.store.admit(item, hub.normalize)
        output = []
        while self.store.step(output.append):
            pass
        self.assertTrue(any(r['recipient'] == 'ag_healthy' for r in output))

    def test_unconfirmed_ack_and_pending_jobs_are_never_retired(self):
        old = hub.RECENT_TERMINAL
        hub.RECENT_TERMINAL = 0
        try:
            self.store.admit(delivery(), hub.normalize)
            self.store.step(lambda _: None)  # Published, but original Core ACK unconfirmed.
            with self.store.db:
                self.store.retire()
            self.assertEqual(self.store.stats()['jobs'], 1)
            self.store.acked('delivery_1')
            with self.store.db:
                self.store.retire()
            self.assertEqual(self.store.stats()['jobs'], 0)
            self.store.admit(delivery(mid='pending', did='pending_d'), hub.normalize)
            self.store.acked('pending_d')
            with self.store.db:
                self.store.retire()
            self.assertEqual(self.store.stats()['jobs'], 1)
        finally:
            hub.RECENT_TERMINAL = old

    def test_lost_core_ack_response_is_retried_without_redelivery(self):
        item = delivery()
        state = {'received': False, 'acks': 0}
        class Cli:
            def run(self, args, body=None):
                if args[1] == 'receive':
                    page = [] if state['received'] else [item]
                    state['received'] = True
                    return {'deliveries': page}
                if args[1] == 'ack':
                    state['acks'] += 1
                    if state['acks'] == 1:
                        raise RuntimeError('Core committed ACK, response lost')
                    return {'already_acknowledged': [args[2]]}
                raise AssertionError(args)
            def send(self, _):
                return {'message': {'message_id': 'published'}}
        hub.poll_once(self.store, Cli())
        self.assertEqual(self.store.pending_acks(), ['delivery_1'])
        self.reopen()
        hub.poll_once(self.store, Cli())
        self.assertEqual(self.store.pending_acks(), [])
        self.assertEqual(state['acks'], 2)
        self.assertEqual(self.store.db.execute('SELECT count(*) FROM jobs').fetchone()[0], 1)

    def test_explicit_human_followup_context_still_receives_reply(self):
        first = delivery()
        first['message']['sender'] = {'kind': 'human', 'id': 'usr_owner'}
        self.store.admit(first, hub.normalize)
        output = []
        def published(reply):
            output.append(reply)
            return {'message': {'message_id': 'msg_published'}}
        self.store.step(published)
        item = delivery(mid='msg_human_followup', did='delivery_human_followup', body='next question')
        item['message']['context_ids'] = ['msg_published']
        item['message']['sender'] = {'kind': 'human', 'id': 'usr_owner'}
        self.store.admit(item, hub.normalize)
        self.store.step(output.append)
        self.assertEqual(len(output), 2)
        self.assertEqual(output[1]['body'], hub.REPLY_MARKER + '[team/tools/agent1] next question')

    def test_stop_fence_preserves_pending_jobs_without_more_calls(self):
        self.store.admit(delivery(), hub.normalize)
        class Cli:
            def run(self, *_):
                raise AssertionError('Core called after stop')
            def send(self, *_):
                raise AssertionError('Reply sent after stop')
        hub.poll_once(self.store, Cli(), lambda: True)
        self.reopen()
        replies = []
        self.assertTrue(self.store.step(replies.append))
        self.assertEqual(len(replies), 1)

    def test_root_negotiation_quality_and_browser_fallback(self):
        self.assertEqual(hub.root_representation('text/html;q=0.2,text/markdown;q=0.8', 'Mozilla/'), 'text/markdown')
        self.assertEqual(hub.root_representation('text/html,text/markdown', ''), 'text/html')
        self.assertEqual(hub.root_representation('text/markdown,text/html', ''), 'text/markdown')
        self.assertEqual(hub.root_representation('', 'Mozilla/5'), 'text/html')
        self.assertEqual(hub.root_representation('*/*', 'curl'), 'text/markdown')

    def test_real_http_tree_empty_unavailable_and_mutation_boundary(self):
        http = hub.server(self.store, '127.0.0.1', 0, lambda _: True)
        thread = threading.Thread(target=http.serve_forever)
        thread.start()
        base = 'http://127.0.0.1:' + str(http.server_port)
        try:
            with urlopen(base + '/api/tree?path=team/tools') as r:
                self.assertEqual(json.load(r), self.store.tree('team/tools'))
            with urlopen(base + '/health') as r:
                self.assertEqual(json.load(r), {'ok': True})
            for suffix in ['/api/tree?path=../x', '/api/tree?path=missing', '/api/tree?path=a&path=b', '/api/tree?parent=team', '/missing']:
                with self.assertRaises(HTTPError) as caught:
                    urlopen(base + suffix)
                self.assertEqual(caught.exception.code, 404)
        finally:
            http.shutdown(); http.server_close(); thread.join()


if __name__ == '__main__':
    unittest.main()
