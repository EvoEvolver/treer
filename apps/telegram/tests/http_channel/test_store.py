import concurrent.futures
import json
from pathlib import Path
import secrets
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parents[2]))
from http_app.common import Error, strict_json
from http_app.store import AdminStore, QueueStore


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.now = 10000.0
        self.q = QueueStore(self.root / 'data.sqlite', clock=lambda: self.now)
        self.a = AdminStore(self.root / 'admin.sqlite', self.q, clock=lambda: self.now)
        self.pair = self.a.issue('human:owner', 'ag_poc', 'Fixture')
        self.credentials = self.a.redeem(self.pair['code'])
        self.client = self.credentials['client_id']
        self.token = self.credentials['token']

    def tearDown(self):
        self.tmp.cleanup()

    def test_token_hash_only_and_private_files(self):
        self.assertEqual(self.q.authenticate(self.token)['id'], self.client)
        with self.q._connect() as db:
            content = '\n'.join(db.iterdump())
            self.assertNotIn(self.token, content)
            self.assertNotIn(self.pair['code'], content)
            self.assertNotIn(self.pair['code'].split('.')[0], content)
            self.assertNotIn('code_hash', content)
        with self.a._connect() as db:
            content = '\n'.join(db.iterdump())
            self.assertNotIn(self.pair['code'], content)
            self.assertNotIn(self.pair['code'].split('.')[0], content)
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
        for path in self.root.iterdir():
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_revoked_expired_and_forged_tokens_have_same_failure(self):
        failures = []
        for action in ('forged', 'revoked', 'expired'):
            if action == 'revoked': self.q.control(self.client, revoke=True)
            if action == 'expired': self.q.install_token(self.client, self.token, self.now - 1)
            with self.assertRaises(Error) as error:
                self.q.authenticate(secrets.token_urlsafe(32) if action == 'forged' else self.token)
            failures.append((error.exception.status, error.exception.code, error.exception.message))
        self.assertEqual(len(set(failures)), 1)

    def test_pairing_is_atomic_and_replay_fails(self):
        code = self.a.issue('human:owner', 'ag_poc', 'Second')['code']
        def attempt():
            try: return self.a.redeem(code)
            except Error: return None
        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
            self.assertEqual(sum(value is not None for value in pool.map(lambda _: attempt(), range(5))), 1)

    def test_anonymous_failures_do_not_invalidate_issued_codes_or_write_database(self):
        code = self.a.issue('human:owner', 'ag_poc', 'Second')['code']
        with self.a._connect() as db: before = '\n'.join(db.iterdump())
        for _ in range(30):
            with self.assertRaises(Error): self.a.redeem('f' * 32 + '.XXXXXXXXXX')
        with self.a._connect() as db: self.assertEqual('\n'.join(db.iterdump()), before)
        self.now += 61
        self.assertIn('token', self.a.redeem(code))

    def test_known_unguessable_handle_failures_block_only_that_code(self):
        code = self.a.issue('human:owner', 'ag_poc', 'Second')['code']
        other = self.a.issue('human:owner', 'ag_poc', 'Third')['code']
        self.now += 61
        for _ in range(10):
            with self.assertRaises(Error): self.a.redeem(code[:33] + 'X' * 10)
        self.now += 61
        with self.assertRaises(Error): self.a.redeem(code)
        self.assertIn('token', self.a.redeem(other))

    def test_enqueue_idempotency_and_cross_client_context(self):
        item = {'body': 'hello', 'idempotency_key': 'key'}
        one = self.q.enqueue(self.client, item)
        self.assertEqual(one, self.q.enqueue(self.client, item))
        with self.assertRaises(Error) as error:
            self.q.enqueue(self.client, {**item, 'body': 'changed'})
        self.assertEqual(error.exception.status, 409)
        self.q.delivered(one, 'msg_core')
        other = self.a.redeem(self.a.issue('human:owner', 'ag_poc', 'Other')['code'])['client_id']
        with self.assertRaises(Error) as error:
            self.q.enqueue(other, {**item, 'context': one})
        self.assertEqual(error.exception.status, 404)
        for field in ('source', 'target', 'external_source'):
            with self.assertRaises(Error): self.q.enqueue(self.client, {**item, field: 'forged'})

    def test_idempotent_retries_do_not_consume_admission_budget(self):
        calls = []
        item = {'body': 'hello', 'idempotency_key': 'key'}
        for _ in range(2):
            self.q.enqueue(self.client, item, admit=lambda: calls.append(1))
        self.assertEqual(calls, [1])
        with self.assertRaises(Error):
            self.q.enqueue(self.client, {'body': '\ud800', 'idempotency_key': 'bad'})

    def test_lease_expiry_recovers_without_losing_message(self):
        message = self.q.enqueue(self.client, {'body': 'hello', 'idempotency_key': 'key'})
        self.assertEqual(self.q.lease()['message_id'], message)
        self.assertIsNone(self.q.lease())
        self.now += 61
        self.assertEqual(self.q.lease()['message_id'], message)
        self.q.delivered(message, 'msg_core')
        self.assertIsNone(self.q.lease())
        self.assertEqual(self.q.mapped(message, self.client), 'msg_core')

    def test_outbound_mapping_route_ack_and_cross_client_dedup(self):
        message = self.q.enqueue(self.client, {'body': 'hello', 'idempotency_key': 'key'})
        self.q.delivered(message, 'msg_in')
        self.assertEqual(self.q.route(['msg_in']), self.client)
        self.assertIsNone(self.q.route(['msg_in', 'msg_unknown']))
        seq = self.q.outbound(self.client, 'msg_out', 'delivery', 'reply')
        self.assertEqual(seq, self.q.outbound(self.client, 'msg_out', 'delivery', 'reply'))
        with self.assertRaises(Error): self.q.ack(self.client, seq)
        result = self.q.poll(self.client, 0)
        self.assertEqual([i['body'] for i in result['items']], ['reply'])
        self.q.ack(self.client, seq)
        self.assertEqual(self.q.poll(self.client, 0)['items'], [])
        other = self.a.redeem(self.a.issue('human:owner', 'ag_poc', 'Other')['code'])['client_id']
        with self.assertRaises(Error): self.q.outbound(other, 'msg_out', 'delivery', 'reply')
        self.assertEqual(self.q.poll(other, 0)['items'], [])

    def test_capacity_preserves_oldest_undelivered_and_retention(self):
        self.q.max_items = 1
        message = self.q.enqueue(self.client, {'body': 'hello', 'idempotency_key': 'one'})
        with self.assertRaises(Error) as error:
            self.q.enqueue(self.client, {'body': 'next', 'idempotency_key': 'two'})
        self.assertEqual(error.exception.status, 429)
        self.now += 40 * 86400
        self.q.cleanup()
        self.assertEqual(self.q.lease()['message_id'], message)
        self.q.delivered(message, 'msg_in')
        self.q.cleanup()
        self.assertIsNone(self.q.lease())

    def test_unroutable_messages_are_bounded_without_bodies(self):
        self.assertFalse(self.a.unroutable('delivery', 99, 'unmapped'))
        self.assertFalse(self.a.unroutable('delivery', 99, 'unmapped'))
        self.assertTrue(self.a.unroutable('delivery', 99, 'unmapped'))
        self.assertTrue(self.a.unroutable('delivery', 99, 'unmapped'))
        with self.a._connect() as db:
            self.assertEqual(db.execute('SELECT count(*) FROM http_undeliverable').fetchone()[0], 0)
        rows = self.a.audit_list('http', self.a.ACCOUNT)
        self.assertEqual(rows[0]['outcome'], 'unmapped')

    def test_rotation_rejects_a_stale_inflight_pair_exchange(self):
        old = self.a.issue('human:owner', 'ag_poc', 'Rotation', client=self.client)
        # Simulate the old exchange having consumed its code before an owner
        # rotates it through another process, then trying to install its token.
        new = self.a.issue('human:owner', 'ag_poc', 'Rotation', client=self.client)
        with self.assertRaises(Error):
            self.q.install_token(self.client, secrets.token_urlsafe(32), expected_handle=old['code'][:32])
        with self.assertRaises(Error): self.q.authenticate(self.token)
        credentials = self.a.redeem(new['code'])
        self.assertEqual(self.q.authenticate(credentials['token'])['id'], self.client)

    def test_duplicate_json_keys_and_nonfinite_rejected(self):
        for raw in (b'{"body":"a","body":"b"}', b'{"value":NaN}', b'\xff'):
            with self.assertRaises(Error): strict_json(raw)
