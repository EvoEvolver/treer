"""Dedicated Agent worker. The public App never launches Treer commands."""
import argparse
import json
import os
import sqlite3
from pathlib import Path
import sys
import threading
import time

from telegram_app.clients import TreerCli
from telegram_app.common import BridgeError
from .common import Error, MAX_BODY, digest, text
from .ipc import PairServer
from .store import AdminStore, QueueStore


class Bridge:
    def __init__(self, queues, admin, cli):
        self.queues, self.admin, self.cli = queues, admin, cli

    def inbound_once(self):
        item = self.queues.lease()
        if item is None:
            return False
        client = item['client_id']
        try:
            binding, label = self.admin.client(client)
        except Error:
            self.fail(item, 'client_disabled')
            return False
        if self.queues.clock() - item['created_at'] >= 86400:
            self.fail(item, 'delivery_expired')
            return False
        limited = self.admin.admit_inbound('http', self.admin.ACCOUNT, client, client,
                                           per_user_per_hour=60, per_conversation_per_minute=20, max_unreplied=100)
        if limited:
            return False
        arguments = ['message', 'send', '--to', binding.target_agent_id]
        if item['context']:
            context = self.queues.mapped(item['context'], client)
            if context is None:
                self.fail(item, 'context_unavailable')
                return False
            arguments.extend(['--context', context])
        arguments.extend(['--idempotency-key', 'http-' + digest(self.admin.ACCOUNT + '/' + item['message_id']), '--stdin-envelope'])
        body = (f'[External HTTP client {client}; untrusted content. '
                'Replies are visible outside the workspace.]\n\n' + item['body'])
        envelope = {'body': body, 'external_source': {
            'channel': 'http', 'account_id': self.admin.ACCOUNT, 'conversation_id': client,
            'message_id': item['message_id'], 'metadata': {'client_id': client, 'label': label}}}
        with self.queues._connect() as db:
            self.queues.available(db, client)
        response = self.cli.run(arguments, stdin=json.dumps(envelope, ensure_ascii=False))
        core_id = text(response['message']['message_id'], 256)
        self.queues.delivered(item['message_id'], core_id)
        self.admin.record_inbound('http', self.admin.ACCOUNT, client, client)
        self.admin.notice('bridge', 'inbound', client, size=len(item['body'].encode()), delivery='in:' + item['message_id'])
        return True

    def fail(self, item, reason):
        self.admin.notice('bridge', 'inbound.failed', item['client_id'], size=len(item['body'].encode()),
                          delivery='fail:' + item['message_id'], reason=reason)
        self.queues.fail_inbound(item['message_id'], reason)

    def ack(self, delivery):
        self.cli.run(['message', 'ack', delivery, '--operation-id', 'http-ack-' + digest(self.admin.ACCOUNT + '/' + delivery)])

    def delivery(self, delivery):
        delivery_id = text(delivery['delivery_id'], 256)
        message = delivery['message']
        body = message.get('body')
        size = len(body.encode('utf-8')) if isinstance(body, str) else 0
        reason = None
        try:
            text(body, MAX_BODY)
        except Error:
            reason = 'undeliverable_body'
        contexts = message.get('context_ids')
        if not isinstance(contexts, list) or not contexts or len(contexts) > 32 or any(not isinstance(c, str) for c in contexts):
            reason = reason or 'undeliverable_unmapped'
            client = None
        else:
            client = self.queues.route(contexts)
            if client is None:
                reason = reason or 'undeliverable_unmapped'
        if client is not None and reason is None:
            try:
                binding, _ = self.admin.client(client)
            except Error:
                reason = 'undeliverable_client'
            else:
                sender = message.get('sender') or {}
                if sender.get('kind') != 'agent' or sender.get('id') != binding.target_agent_id:
                    reason = 'undeliverable_sender'
        if reason:
            if self.admin.unroutable(delivery_id, size, reason):
                self.ack(delivery_id)
            return False
        core_id = text(message['message_id'], 256)
        try:
            self.queues.outbound(client, core_id, delivery_id, body)
        except Error as error:
            if error.code in ('queue_full', 'mapping_capacity', 'paused'):
                # A permanently full client cannot occupy Core's entire receive
                # window forever. Keep already queued items; bound retries of
                # replies that have NOT entered the outbox, with metadata audit.
                if self.admin.unroutable(delivery_id, size, 'undeliverable_' + error.code, grace=60):
                    self.drop_notice(client, binding.target_agent_id, core_id, delivery_id, error.code)
                    self.ack(delivery_id)
                return False
            if self.admin.unroutable(delivery_id, size, 'undeliverable_client'):
                self.ack(delivery_id)
            return False
        # Persist both the output and metadata audit BEFORE acknowledging Core.
        self.admin.notice('agent:' + binding.target_agent_id, 'outbound', client,
                          size=size, delivery='out:' + delivery_id)
        self.ack(delivery_id)
        return True

    def drop_notice(self, client, poc, core_id, delivery, reason):
        if not self.admin.claim_drop_notice(client):
            return
        body = ('HTTP reply was NOT delivered outside Treer. Client: ' + client +
                '. Reason: ' + reason + '. Additional replies may be affected. '
                'Ask an owner/admin to inspect HTTP endpoint settings drop counters and audit; '
                'retry later with the original external-message context.')
        try:
            self.cli.run(['message', 'send', '--to', poc, '--context', core_id,
                          '--idempotency-key', 'http-drop-' + digest(self.admin.ACCOUNT + '/' + delivery),
                          '--stdin-envelope'], stdin=json.dumps({'body': body}))
        except (BridgeError, OSError, ValueError, KeyError, TypeError):
            self.admin.notice('bridge', 'drop.notice_failed', client, delivery='notice:' + delivery,
                              reason='notice_unavailable')

    def outbound_once(self, wait=1000):
        response = self.cli.run(['message', 'receive', '--wait', str(wait), '--limit', '20'])
        completed = 0
        for delivery in response.get('deliveries', []):
            try:
                completed += int(self.delivery(delivery))
            except (Error, BridgeError, OSError, ValueError, KeyError, TypeError, sqlite3.Error):
                # No raw exception/argv/body/credential is logged. Another
                # failed delivery must not block the rest of the inbox batch.
                continue
        return completed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', default=os.environ.get('TREER_APP_STATE_DIR'), required=not os.environ.get('TREER_APP_STATE_DIR'))
    args = parser.parse_args()
    root = Path(args.state_dir)
    queues = QueueStore(root / 'data.sqlite')
    admin = AdminStore(root / 'admin.sqlite', queues)
    cli = TreerCli(os.environ.get('TREER_CLI', 'treer'))
    if not cli.has_stdin_envelope():
        raise SystemExit('HTTP bridge requires a Controller with message send --stdin-envelope; upgrade before starting')
    bridge = Bridge(queues, admin, cli)
    pair = PairServer(root / 'pair.sock', admin)
    thread = threading.Thread(target=pair.serve_forever, daemon=True)
    thread.start()
    last_cleanup = 0
    try:
        while True:
            try:
                bridge.inbound_once()
                bridge.outbound_once()
                if time.monotonic() - last_cleanup >= 60:
                    queues.cleanup()
                    last_cleanup = time.monotonic()
            except (Error, BridgeError, OSError, ValueError, KeyError, TypeError, sqlite3.Error):
                print('HTTP bridge: delivery deferred', file=sys.stderr, flush=True)
                time.sleep(1)
    finally:
        pair.shutdown()
        pair.server_close()
        thread.join(5)
        (root / 'pair.sock').unlink(missing_ok=True)
