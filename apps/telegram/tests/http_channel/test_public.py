import concurrent.futures
import http.client
import inspect
import json
from pathlib import Path
import secrets
import socket
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, str(Path(__file__).parents[2]))
from http_app import public
from http_app.ipc import PairServer
from http_app.store import AdminStore, QueueStore


class PublicTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='http142-', dir='/tmp')
        self.root = Path(self.tmp.name)
        self.q = QueueStore(self.root / 'data.sqlite')
        self.a = AdminStore(self.root / 'admin.sqlite', self.q)
        credentials = self.a.redeem(self.a.issue('human:owner', 'ag_poc', 'Fixture')['code'])
        self.client, self.token = credentials['client_id'], credentials['token']
        self.pair = PairServer(self.root / 'pair.sock', self.a)
        self.app = public.Application(self.q, self.root / 'pair.sock')
        self.server = public.Server(('127.0.0.1', 0), public.Handler, self.app)
        self.threads = []
        for server in (self.pair, self.server):
            thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': .01}, daemon=True)
            thread.start(); self.threads.append(thread)

    def tearDown(self):
        for server in (self.server, self.pair): server.shutdown(); server.server_close()
        for thread in self.threads: thread.join(2)
        self.tmp.cleanup()

    def request(self, method, path, value=None, *, token='self', headers=None):
        headers = dict(headers or {})
        if token == 'self': token = self.token
        if token is not None: headers['Authorization'] = 'Bearer ' + token
        if value is not None:
            headers.setdefault('Content-Type', 'application/json')
            body = json.dumps(value).encode()
        else: body = None
        connection = http.client.HTTPConnection(*self.server.server_address, timeout=35)
        try:
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            payload = response.read()
            return response.status, json.loads(payload) if payload else None, dict(response.headers)
        finally: connection.close()

    def test_only_public_routes_and_no_authentication_fallback(self):
        for path in ('/', '/health', '/settings', '/v1/clients', '/api/status', '/.treer/identity', '/v1/inbox/'):
            self.assertEqual(self.request('GET', path)[0], 404)
        for method in ('OPTIONS', 'PUT', 'DELETE', 'TRACE', 'BREW'):
            status, _, headers = self.request(method, '/v1/inbox')
            self.assertEqual(status, 404)
            self.assertFalse(any(k.lower().startswith('access-control-') for k in headers))
        self.assertEqual(self.request('GET', '/v1/outbox', token=None, headers={'Cookie':'fixture', 'X-Treer-Identity-Token':'forged'})[0], 401)
        self.assertNotIn('AdminStore', inspect.getsource(public))

    def test_post_is_durable_and_idempotent(self):
        body = {'body':'<script>fixture</script>', 'idempotency_key':'one'}
        first = self.request('POST', '/v1/inbox', body)
        self.assertEqual(first[0], 202)
        self.assertEqual(first[2]['Cache-Control'], 'no-store')
        self.assertEqual(first[1], self.request('POST', '/v1/inbox', body)[1])
        self.assertEqual(self.q.lease()['message_id'], first[1]['message_id'])
        self.assertEqual(self.request('POST', '/v1/inbox', {**body, 'body':'different'})[0], 409)
        self.assertEqual(self.request('POST', '/v1/inbox', {**body, 'source':{'role':'owner'}})[0], 400)

    def test_pair_replay_and_private_rpc_surface(self):
        code = self.a.issue('human:owner', 'ag_poc', 'Paired')['code']
        result = self.request('POST', '/v1/pair', {'code':code}, token=None)
        self.assertEqual(result[0], 200)
        self.assertEqual(len(result[1]['token']), 43)
        self.assertEqual(self.request('POST', '/v1/pair', {'code':code}, token=None)[0], 401)
        self.assertEqual(self.request('POST', '/v1/pair', {'code':code, 'op':'clients.list'}, token=None)[0], 400)
        self.assertEqual(self.request('GET', '/v1/outbox', token=result[1]['token'])[0], 200)

    def test_anonymous_flood_does_not_spend_authenticated_budgets_or_write_state(self):
        with self.q._connect() as db: before = '\n'.join(db.iterdump())
        for _ in range(65): self.request('GET', '/v1/outbox', token=secrets.token_urlsafe(32))
        with self.q._connect() as db: self.assertEqual('\n'.join(db.iterdump()), before)
        self.assertEqual(self.request('GET', '/v1/outbox')[0], 200)
        self.assertEqual(self.request('POST', '/v1/inbox', {'body':'accepted', 'idempotency_key':'one'})[0], 202)

    def test_limits_and_client_isolation(self):
        other = self.a.redeem(self.a.issue('human:owner', 'ag_poc', 'Other')['code'])
        self.q.outbound(other['client_id'], 'core_out', 'delivery', 'private to other')
        self.assertEqual(self.request('GET', '/v1/outbox')[1]['items'], [])
        self.assertEqual(self.request('POST', '/v1/outbox/ack', {'up_to':1})[0], 400)
        self.q.max_items = 1
        body = {'body':'one', 'idempotency_key':'one'}
        self.assertEqual(self.request('POST', '/v1/inbox', body)[0], 202)
        self.assertEqual(self.request('POST', '/v1/inbox', {**body,'idempotency_key':'two'})[0], 429)
        self.assertEqual(self.request('POST', '/v1/inbox', body)[0], 202)

    def test_rotation_and_pause_interrupt_waiting_polls(self):
        for operation, expected in (('rotate',401), ('pause',503), ('global',503)):
            self.q.install_token(self.client, self.token)
            self.q.control(self.client, paused=False)
            self.q.control(paused=False)
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                started = time.monotonic()
                result = pool.submit(self.request, 'GET', '/v1/outbox?timeout=30')
                deadline = time.monotonic()+2
                while self.app.poll_counts[self.client] < 1 and time.monotonic() < deadline: time.sleep(.01)
                if operation == 'rotate': self.q.install_token(self.client, secrets.token_urlsafe(32))
                elif operation == 'pause': self.q.control(self.client, paused=True)
                else: self.q.control(paused=True)
                self.assertEqual(result.result(timeout=3)[0], expected)
                self.assertLess(time.monotonic()-started, 3)
            self.assertEqual(sum(self.app.poll_counts.values()), 0)

    def test_long_poll_does_not_hold_a_database_transaction_and_returns_new_data(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            result = pool.submit(self.request, 'GET', '/v1/outbox?timeout=30')
            deadline = time.monotonic()+2
            while self.app.poll_counts[self.client] < 1 and time.monotonic() < deadline: time.sleep(.01)
            with self.q._connect() as db:
                db.execute('BEGIN EXCLUSIVE')
                db.execute("INSERT INTO settings VALUES ('fixture','ok')")
            self.q.outbound(self.client, 'core_out', 'delivery', 'reply')
            response = result.result(timeout=3)
            self.assertEqual(response[0], 200)
            self.assertEqual(response[1]['items'][0]['body'], 'reply')
            self.assertTrue(response[1]['items'][0]['created_at'].endswith('Z'))

    def test_malformed_and_oversized_body_and_query(self):
        self.assertEqual(self.request('POST','/v1/inbox',{'body':'a'*66000,'idempotency_key':'one'})[0], 413)
        self.assertEqual(self.request('POST','/v1/inbox',{'body':'one','idempotency_key':'one'}, headers={'Content-Encoding':'gzip'})[0], 415)
        for query in ('after=-1','timeout=31','timeout=1&timeout=2','target=someone','after='):
            self.assertEqual(self.request('GET','/v1/outbox?'+query)[0], 400)

    def test_long_poll_capacity_is_only_for_authenticated_callers(self):
        with self.app.polling(self.client, 30):
            with self.app.polling(self.client, 30):
                self.assertEqual(self.request('GET','/v1/outbox?timeout=30')[0], 429)
                self.assertEqual(self.request('GET','/v1/outbox?timeout=0')[0], 200)
                self.assertEqual(self.request('GET','/v1/outbox?timeout=30',token=None)[0], 401)

    def test_raw_ambiguous_headers_and_json_are_rejected(self):
        auth = 'Authorization: Bearer ' + self.token + '\r\n'
        def raw(headers, body):
            with socket.create_connection(self.server.server_address, timeout=7) as s:
                s.sendall(('POST /v1/inbox HTTP/1.1\r\nHost: fixture\r\n' + auth + headers + '\r\n').encode() + body)
                return s.recv(4096).split(b'\r\n')[0]
        for headers in ('Content-Length: 2\r\nContent-Length: 2\r\n',
                        'Content-Length: 2\r\nTransfer-Encoding: chunked\r\n'):
            self.assertIn(b'400', raw(headers,b'{}'))
        for body in (b'{"body":"one","body":"two","idempotency_key":"one"}',
                     b'{"body":"one","idempotency_key":NaN}', b'\xff'):
            self.assertIn(b'400', raw('Content-Type: application/json\r\nContent-Length: '+str(len(body))+'\r\n',body))

    def test_rotation_between_auth_and_mutation_rejects_stale_generation(self):
        self.q.outbound(self.client,'core-race','delivery-race','reply')
        self.q.poll(self.client,0)
        original=self.app.authenticate
        def rotate_after_auth(headers):
            result=original(headers)
            self.q.install_token(self.client,secrets.token_urlsafe(32))
            return result
        self.app.authenticate=rotate_after_auth
        self.assertEqual(self.request('POST','/v1/inbox',{'body':'stale','idempotency_key':'stale'})[0],401)
        self.assertIsNone(self.q.lease())
        self.q.install_token(self.client,self.token)
        self.assertEqual(self.request('POST','/v1/outbox/ack',{'up_to':1})[0],401)
        self.assertEqual(len(self.q.poll(self.client,0)['items']),1)

    def test_public_facade_cannot_call_management_methods(self):
        import ast
        from http_app.store import PublicQueues
        facade=PublicQueues(self.q)
        for name in ('control','prepare_pair','install_token','_connect','cleanup','summaries'):
            self.assertFalse(hasattr(facade,name))
        tree=ast.parse(inspect.getsource(public))
        methods={node.attr for node in ast.walk(tree) if isinstance(node,ast.Attribute)
                 and isinstance(node.value,ast.Attribute) and node.value.attr=='queues'}
        self.assertLessEqual(methods,{'authenticate','enqueue','poll','ack','available'})

    def test_long_poll_connections_are_closed_on_every_iteration(self):
        import sqlite3
        from unittest.mock import patch
        original=sqlite3.connect;connections=[]
        class Tracked(sqlite3.Connection):
            closed=False
            def close(self):
                self.closed=True
                super().close()
        def connect(*args,**kwargs):
            connection=original(*args,**kwargs,factory=Tracked);connections.append(connection);return connection
        with patch('http_app.store.sqlite3.connect',connect):
            self.assertEqual(self.request('GET','/v1/outbox?timeout=1')[0],200)
        self.assertGreater(len(connections),4)
        self.assertTrue(all(c.closed for c in connections))
