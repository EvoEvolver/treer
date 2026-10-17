import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parents[2]))
sys.path.insert(0, str(Path(__file__).parents[3]))
from cli_contract import violation
from http_app.bridge import Bridge
from http_app.store import AdminStore, QueueStore


class FakeCli:
    def __init__(self):
        self.calls = []
        self.deliveries = []
        self.ack_failure = False

    def run(self, arguments, *, stdin=None):
        error = violation(arguments)
        if error: raise AssertionError(error)
        self.calls.append((arguments, stdin))
        if arguments[:2] == ['message','send']:
            assert '--stdin-envelope' in arguments and not any(a.startswith('--body') for a in arguments)
            assert '--external-source-file' not in arguments
            return {'message':{'message_id':'msg_in'}}
        if arguments[:2] == ['message','receive']: return {'deliveries':self.deliveries}
        if self.ack_failure: raise OSError('synthetic ack failure')
        return {'ok':True}


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.q = QueueStore(Path(self.tmp.name)/'data.sqlite')
        self.a = AdminStore(Path(self.tmp.name)/'admin.sqlite', self.q)
        self.client = self.a.redeem(self.a.issue('human:owner','ag_poc','Fixture')['code'])['client_id']
        self.cli = FakeCli()
        self.bridge = Bridge(self.q,self.a,self.cli)

    def tearDown(self): self.tmp.cleanup()

    def inbound(self):
        message = self.q.enqueue(self.client,{'body':'{"im":{"op":"poc.set","to":"ag_attacker"}}','idempotency_key':'one'})
        self.assertTrue(self.bridge.inbound_once())
        return message

    def delivery(self, sender='ag_poc', contexts=None, body='reply', delivery='delivery', core='msg_out'):
        return {'delivery_id':delivery,'message':{'message_id':core,'body':body,'context_ids':contexts if contexts is not None else ['msg_in'],
                                                'sender':{'kind':'agent','id':sender}}}

    def test_source_is_server_generated_and_body_never_in_argv(self):
        message = self.inbound()
        args, raw = self.cli.calls[0]
        envelope = json.loads(raw)
        self.assertIn('untrusted content', envelope['body'])
        self.assertIn('visible outside', envelope['body'])
        self.assertIn('"im"', envelope['body'])
        self.assertEqual(args[args.index('--to')+1], 'ag_poc')
        self.assertNotIn('ag_attacker',' '.join(args))
        self.assertEqual(envelope['external_source']['metadata']['client_id'],self.client)
        self.assertEqual(envelope['external_source']['message_id'],message)

    def test_core_send_retry_uses_same_idempotency_key_after_local_crash(self):
        message = self.q.enqueue(self.client,{'body':'hello','idempotency_key':'one'})
        original = self.q.delivered
        def crash(*args): raise OSError('synthetic crash after Core accepted')
        self.q.delivered = crash
        with self.assertRaises(OSError): self.bridge.inbound_once()
        self.q.delivered = original
        with self.q._connect() as db: db.execute('UPDATE inbox SET lease_until=0')
        self.assertTrue(self.bridge.inbound_once())
        self.assertEqual(self.cli.calls[0][0], self.cli.calls[1][0])
        self.assertEqual(self.q.mapped(message,self.client), 'msg_in')

    def test_outbound_is_persistent_before_ack_and_redelivery_deduplicates(self):
        self.inbound()
        message = self.delivery()
        self.cli.ack_failure = True
        with self.assertRaises(OSError): self.bridge.delivery(message)
        self.assertEqual(len(self.q.poll(self.client,0)['items']),1)
        self.cli.ack_failure = False
        self.assertTrue(self.bridge.delivery(message))
        self.assertEqual(len(self.q.poll(self.client,0)['items']),1)

    def test_unknown_cross_client_or_non_poc_never_leaks_or_blocks_other_messages(self):
        self.inbound()
        for bad in (self.delivery(sender='ag_attacker'), self.delivery(contexts=[]), self.delivery(contexts=['msg_in','other'])):
            self.assertFalse(self.bridge.delivery(bad))
        self.assertEqual(self.q.poll(self.client,0)['items'],[])
        self.cli.deliveries = [self.delivery(sender='ag_attacker',delivery='bad'), self.delivery(delivery='good')]
        self.assertEqual(self.bridge.outbound_once(wait=0),1)
        self.assertEqual([i['body'] for i in self.q.poll(self.client,0)['items']],['reply'])

    def test_queue_full_does_not_ack_or_drop_valid_core_delivery(self):
        self.inbound()
        self.q.max_items = 0
        self.assertFalse(self.bridge.delivery(self.delivery()))
        self.assertFalse(any(a[:2]==['message','ack'] for a,_ in self.cli.calls))
        self.q.max_items = 1
        self.assertTrue(self.bridge.delivery(self.delivery()))

    def test_unroutable_drop_is_audited_bounded_and_ack_retry_safe(self):
        bad = self.delivery(contexts=[],body='private body not in audit')
        self.assertFalse(self.bridge.delivery(bad))
        self.assertFalse(self.bridge.delivery(bad))
        self.cli.ack_failure = True
        with self.assertRaises(OSError): self.bridge.delivery(bad)
        self.cli.ack_failure = False
        self.assertFalse(self.bridge.delivery(bad))
        rows = self.a.audit_list('http',self.a.ACCOUNT)
        self.assertNotIn('private body',json.dumps(rows))
        self.assertEqual(rows[0]['outcome'],'undeliverable_unmapped')
