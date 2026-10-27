#!/usr/bin/env python3
"""App-owned recursive agent tree and durable message routing example (stdlib only)."""
import argparse
import contextlib
import fcntl
import hashlib
import html
import json
import os
import re
import signal
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler, ProxyHandler

MAX_NODES = 256
MAX_CHILDREN = 100
MAX_DEPTH = 8
MAX_PENDING = 1000
MAX_BODY = 16384
MAX_RECEIPTS = 10000
RECENT_TERMINAL = 128
MAX_SENDERS = 128
SENDER_WINDOW = 60
SENDER_RATE = 32
SENDER_PENDING = 32
REPLY_MARKER = '[app-hub:reply:v1] '

KEY_PART = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}\Z")


def key(value, empty=False):
    if not isinstance(value, str) or (not value and not empty):
        raise ValueError('invalid_key')
    parts = value.split('/') if value else []
    if len(parts) > MAX_DEPTH or any(not KEY_PART.fullmatch(p) for p in parts):
        raise ValueError('invalid_key')
    return value


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def root_representation(accept, user_agent):
    # Same representation contract as existing standalone Treer Apps.
    choices = []
    for order, raw in enumerate(accept.split(',')):
        parts = [part.strip().lower() for part in raw.split(';')]
        if parts[0] not in ('text/html', 'text/markdown'):
            continue
        quality = 1.0
        for parameter in parts[1:]:
            if parameter.startswith('q='):
                try:
                    quality = float(parameter[2:])
                except ValueError:
                    quality = 0.0
        if 0 < quality <= 1:
            choices.append((-quality, order, parts[0]))
    return min(choices)[2] if choices else ('text/html' if 'mozilla/' in user_agent.lower() else 'text/markdown')


class Store:
    def __init__(self, root, workspace, app_id, worker=True, clock=time.time):
        root = Path(root)
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if root.is_symlink() or root.stat().st_mode & 0o077:
            raise ValueError('state_directory_must_be_private')
        self.lock = None
        if worker:
            fd = os.open(root / 'worker.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            self.lock = os.fdopen(fd, 'a')
            try:
                fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BaseException:
                self.lock.close()
                raise
        db_path = root / 'state.sqlite'
        if db_path.is_symlink():
            raise ValueError('state_symlink')
        self.db = sqlite3.connect(db_path, check_same_thread=False)
        os.chmod(db_path, 0o600)
        self.mutex = threading.RLock()
        self.clock = clock
        self.db.executescript('''
          PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), workspace TEXT, app_id TEXT);
          CREATE TABLE IF NOT EXISTS nodes (key TEXT PRIMARY KEY, parent TEXT NOT NULL, kind TEXT NOT NULL,
            name TEXT NOT NULL, handler TEXT, enabled INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS jobs (message_id TEXT NOT NULL, endpoint TEXT NOT NULL, fingerprint TEXT NOT NULL,
            request TEXT, response TEXT, state TEXT NOT NULL, turn INTEGER NOT NULL DEFAULT 0,
            subject TEXT NOT NULL, reply_id TEXT, PRIMARY KEY(message_id,endpoint));
          CREATE TABLE IF NOT EXISTS deliveries (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, endpoint TEXT NOT NULL,
            acked INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS quotas (subject TEXT PRIMARY KEY, window INTEGER NOT NULL, admitted INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS counters (code TEXT PRIMARY KEY, attempts INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS poison (singleton INTEGER PRIMARY KEY CHECK(singleton=1), attempts INTEGER NOT NULL);
        ''')
        with self.db:
            saved = self.db.execute('SELECT workspace,app_id FROM identity').fetchone()
            if saved and saved != (workspace, app_id):
                raise ValueError('state_identity_mismatch')
            self.db.execute('INSERT OR IGNORE INTO identity VALUES(1,?,?)', (workspace, app_id))
        self.workspace, self.app_id = workspace, app_id

    def close(self):
        self.db.close()
        if self.lock:
            self.lock.close()

    def register(self, node_key, kind, name, handler=None):
        node_key = key(node_key)
        parent = node_key.rpartition('/')[0]
        if kind not in ('app', 'agent') or not isinstance(name, str) or not 0 < len(name.encode()) <= 128:
            raise ValueError('invalid_node')
        if (kind == 'agent' and handler not in ('echo', 'reverse')) or (kind == 'app' and handler is not None):
            raise ValueError('invalid_handler')
        with self.mutex, self.db:
            self.db.execute('BEGIN IMMEDIATE')
            if parent and self.db.execute('SELECT kind,enabled FROM nodes WHERE key=?', (parent,)).fetchone() != ('app', 1):
                raise ValueError('missing_parent_app')
            old = self.db.execute('SELECT kind FROM nodes WHERE key=?', (node_key,)).fetchone()
            if old and old[0] != kind:
                raise ValueError('kind_is_immutable')
            if not old and (self.db.execute('SELECT count(*) FROM nodes').fetchone()[0] >= MAX_NODES or self.db.execute('SELECT count(*) FROM nodes WHERE parent=?', (parent,)).fetchone()[0] >= MAX_CHILDREN):
                raise ValueError('tree_capacity')
            self.db.execute('INSERT INTO nodes VALUES(?,?,?,?,?,1) ON CONFLICT(key) DO UPDATE SET name=excluded.name,handler=excluded.handler,enabled=1',
                            (node_key, parent, kind, name, handler))

    def remove(self, node_key):
        node_key = key(node_key)
        # Disabled identities remain stable tombstones; accepted jobs retain their handler snapshot.
        with self.mutex, self.db:
            self.db.execute('BEGIN IMMEDIATE')
            self.db.execute('UPDATE nodes SET enabled=0 WHERE key=? OR substr(key,1,?)=?',
                            (node_key, len(node_key) + 1, node_key + '/'))

    def tree(self, parent=''):
        parent = key(parent, empty=True)
        with self.mutex:
            if parent and self.db.execute('SELECT kind,enabled FROM nodes WHERE key=?', (parent,)).fetchone() != ('app', 1):
                raise ValueError('app_not_found')
            rows = self.db.execute('SELECT key,kind,name FROM nodes WHERE parent=? AND enabled=1 ORDER BY key LIMIT ?', (parent, MAX_CHILDREN)).fetchall()
        return {'schema_version': 1, 'path': parent.split('/') if parent else [],
                'nodes': [{'key': k.rsplit('/', 1)[-1], 'kind': kind, 'name': name,
                           'capabilities': ['tree.read'] if kind == 'app' else ['message.send', 'message.read']} for k, kind, name in rows]}

    def count(self, code, amount=1):
        self.db.execute('INSERT INTO counters VALUES(?,?) ON CONFLICT(code) DO UPDATE SET attempts=min(attempts+excluded.attempts,9223372036854775807)', (code, amount))

    def retire(self):
        # ACK is Core's irreversible responsibility transfer. Keep a recent
        # terminal window; only retire rows with every observed ACK confirmed.
        # Pure handler replay also uses the same Core send idempotency key.
        total = self.db.execute("SELECT count(*) FROM jobs WHERE state IN ('sent','terminal')").fetchone()[0]
        count = max(0, min(20, total - RECENT_TERMINAL))
        rows = self.db.execute("""SELECT message_id,endpoint FROM jobs j
          WHERE state IN ('sent','terminal') AND NOT EXISTS
          (SELECT 1 FROM deliveries d WHERE d.message_id=j.message_id AND d.endpoint=j.endpoint AND d.acked=0)
          ORDER BY rowid LIMIT ?""", (count,)).fetchall()
        for mid, endpoint in rows:
            self.db.execute('DELETE FROM deliveries WHERE message_id=? AND endpoint=?', (mid, endpoint))
            self.db.execute('DELETE FROM jobs WHERE message_id=? AND endpoint=?', (mid, endpoint))
        if rows:
            self.count('retired', len(rows))

    def quota(self, subject):
        now = int(self.clock())
        self.db.execute('DELETE FROM quotas WHERE window<=?', (now - SENDER_WINDOW,))
        row = self.db.execute('SELECT window,admitted FROM quotas WHERE subject=?', (subject,)).fetchone()
        if (row and (now < row[0] or row[1] >= SENDER_RATE)) or (not row and self.db.execute('SELECT count(*) FROM quotas').fetchone()[0] >= MAX_SENDERS):
            return False
        pending = self.db.execute("SELECT count(*) FROM jobs WHERE subject=? AND state NOT IN ('sent','terminal')", (subject,)).fetchone()[0]
        if pending >= SENDER_PENDING:
            return False
        self.db.execute('INSERT INTO quotas VALUES(?,?,1) ON CONFLICT(subject) DO UPDATE SET admitted=admitted+1', (subject, now))
        return True

    def stats(self):
        with self.mutex:
            return {'jobs': self.db.execute('SELECT count(*) FROM jobs').fetchone()[0],
                    'deliveries': self.db.execute('SELECT count(*) FROM deliveries').fetchone()[0],
                    'counters': dict(self.db.execute('SELECT code,attempts FROM counters'))}

    def admit(self, delivery, normalize):
        """Persist before ACK; invalid deliveries use a bounded body-free poison counter."""
        delivery_id = delivery.get('delivery_id') if isinstance(delivery, dict) else None
        if not isinstance(delivery_id, str) or not 0 < len(delivery_id) <= 200:
            raise ValueError('invalid_delivery_id')  # Cannot acknowledge an untrusted selector.
        try:
            request = normalize(delivery, self.workspace, self.app_id)
        except (ValueError, KeyError, TypeError):
            with self.mutex, self.db:
                self.db.execute('INSERT INTO poison VALUES(1,1) ON CONFLICT(singleton) DO UPDATE SET attempts=min(attempts+1,9223372036854775807)')
            return delivery_id
        mid, endpoint = request['message_id'], request['endpoint']
        fingerprint = digest(encoded(request))
        with self.mutex, self.db:
            self.db.execute('BEGIN IMMEDIATE')
            self.retire()
            old = self.db.execute('SELECT fingerprint FROM jobs WHERE message_id=? AND endpoint=?', (mid, endpoint)).fetchone()
            if old and old[0] != fingerprint:
                raise ValueError('immutable_message_conflict')
            if not old:
                if not self.quota(request['subject']):
                    self.count('sender_limited')
                    return delivery_id
                if (self.db.execute('SELECT count(*) FROM jobs').fetchone()[0] >= MAX_RECEIPTS
                        or self.db.execute("SELECT count(*) FROM jobs WHERE state NOT IN ('sent','terminal')").fetchone()[0] >= MAX_PENDING
                        or self.db.execute('SELECT count(*) FROM deliveries').fetchone()[0] >= MAX_RECEIPTS):
                    # Nothing was admitted/executed. Durable body-free refusal
                    # then ACK avoids turning a full queue into a poisoned page.
                    self.count('busy')
                    return delivery_id
                if request['contexts'] and request['sender_kind'] == 'agent':
                    marks = ','.join('?' for _ in request['contexts'])
                    if self.db.execute('SELECT 1 FROM jobs WHERE reply_id IN (' + marks + ') LIMIT 1', request['contexts']).fetchone():
                        request['no_reply'] = True
                node = self.db.execute('SELECT kind,handler,enabled FROM nodes WHERE key=?', (endpoint,)).fetchone()
                request['handler'] = node[1] if node and node[0] == 'agent' and node[2] else None
                self.db.execute('INSERT INTO jobs(message_id,endpoint,fingerprint,request,response,state,subject) VALUES(?,?,?,?,NULL,?,?)', (mid, endpoint, fingerprint, encoded(request), 'queued', request['subject']))
            existing = self.db.execute('SELECT message_id,endpoint FROM deliveries WHERE id=?', (delivery_id,)).fetchone()
            if not existing and self.db.execute('SELECT count(*) FROM deliveries').fetchone()[0] >= MAX_RECEIPTS:
                self.count('busy')
                return delivery_id
            if existing and existing != (mid, endpoint):
                raise ValueError('delivery_identity_conflict')
            self.db.execute('INSERT OR IGNORE INTO deliveries(id,message_id,endpoint) VALUES(?,?,?)', (delivery_id, mid, endpoint))
        return delivery_id

    def pending_acks(self):
        with self.mutex:
            return [row[0] for row in self.db.execute('SELECT id FROM deliveries WHERE acked=0 ORDER BY rowid LIMIT 20')]

    def acked(self, delivery_id):
        with self.mutex, self.db:
            self.db.execute('UPDATE deliveries SET acked=1 WHERE id=?', (delivery_id,))

    def step(self, send):
        with self.mutex, self.db:
            self.db.execute('BEGIN IMMEDIATE')
            row = self.db.execute("SELECT message_id,endpoint,request,response FROM jobs WHERE state NOT IN ('sent','terminal') ORDER BY turn,rowid LIMIT 1").fetchone()
            if not row:
                return False
            mid, endpoint, raw, response = row
            request = json.loads(raw)
            self.db.execute('UPDATE jobs SET turn=(SELECT max(turn)+1 FROM jobs) WHERE message_id=? AND endpoint=?', (mid, endpoint))
            if response is None:
                handler = request['handler']
                text = request['body']
                body = ('[app_endpoint_not_found] ' + endpoint) if handler is None else ('[' + endpoint + '] ' + (text[::-1] if handler == 'reverse' else text))
                response = encoded({'body': REPLY_MARKER + body, 'recipient': request['sender'], 'sender_endpoint': endpoint,
                                    'context': mid, 'idempotency_key': 'app-hub-reply-' + digest(self.workspace + '/' + self.app_id + '/' + mid + '/' + endpoint)})
                self.db.execute("UPDATE jobs SET response=?,state='outbox' WHERE message_id=? AND endpoint=?", (response, mid, endpoint))
        # No lock held over Core CLI. On timeout, identical immutable request/key is retried,
        # relying on Core send's durable idempotency; handlers have no external effects.
        receipt = None if request['no_reply'] else send(json.loads(response))
        reply_id = receipt.get('message', {}).get('message_id') if isinstance(receipt, dict) else None
        with self.mutex, self.db:
            if request['no_reply']:
                self.count('reply_suppressed')
            self.db.execute("UPDATE jobs SET state=?,request=NULL,response=NULL,reply_id=? WHERE message_id=? AND endpoint=?",
                            ('terminal' if request['no_reply'] else 'sent', reply_id, mid, endpoint))
        return True


# Wire adapter is pinned to Core's #230 contract below before integration.
def normalize(delivery, workspace, app_id):
    message = delivery['message']
    recipient = delivery['recipient']
    if message['workspace_id'] != workspace or recipient['id'] != app_id or recipient['kind'] != 'agent':
        raise ValueError('wrong_recipient')
    endpoint = key(recipient['app_endpoint'])
    mid = message['message_id']
    body = message['body']
    sender = message['sender']
    if not isinstance(mid, str) or not 0 < len(mid) <= 200 or not isinstance(body, str) or len(body.encode()) > MAX_BODY:
        raise ValueError('invalid_message')
    if sender.get('kind') not in ('agent', 'human') or not isinstance(sender.get('id'), str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,200}', sender['id']):
        raise ValueError('unsupported_sender')
    target = sender['id']
    if sender.get('app_endpoint'):
        if sender['kind'] != 'agent':
            raise ValueError('invalid_sender_endpoint')
        target += '/' + key(sender['app_endpoint'])
    contexts = message.get('context_ids', [])
    if not isinstance(contexts, list) or len(contexts) > 64 or any(not isinstance(c, str) or len(c) > 200 for c in contexts):
        raise ValueError('invalid_contexts')
    return {'message_id': mid, 'endpoint': endpoint, 'sender': target, 'subject': sender['id'],
            'contexts': contexts, 'body': body, 'sender_kind': sender['kind'],
            'no_reply': sender['id'] == app_id or body.startswith(REPLY_MARKER)}


class Cli:
    def __init__(self, executable):
        self.executable = str(Path(executable).resolve(strict=True))

    def run(self, args, body=None):
        result = subprocess.run([self.executable] + args, input=body, text=True, capture_output=True, timeout=20)
        if result.returncode:
            raise RuntimeError('core_cli_failed')
        return json.loads(result.stdout)

    def send(self, response):
        result = self.run(['message', 'send', '--to', response['recipient'], '--from-endpoint', response['sender_endpoint'],
                         '--context', response['context'], '--idempotency-key', response['idempotency_key'], '--stdin-envelope'], encoded({'body': response['body']}))
        if not isinstance(result.get('message', {}).get('message_id'), str):
            raise RuntimeError('missing_core_send_receipt')
        return result


def poll_once(store, cli, stopped=lambda: False):
    if stopped():
        return
    # An ACK may have committed in Core while its response was lost. Core then
    # no longer redelivers it: retry the saved ACK selector independently.
    for did in store.pending_acks():
        if stopped():
            return
        try:
            cli.run(['message', 'ack', did, '--operation-id', 'app-hub-ack-' + digest(did)])
            store.acked(did)
        except (RuntimeError, sqlite3.Error, subprocess.TimeoutExpired):
            continue
    page = cli.run(['message', 'receive', '--limit', '20', '--wait', '1000'])
    for delivery in page['deliveries']:
        if stopped():
            break
        try:
            did = store.admit(delivery, normalize)
            cli.run(['message', 'ack', did, '--operation-id', 'app-hub-ack-' + digest(did)])
            store.acked(did)
        except (ValueError, KeyError, TypeError, RuntimeError, sqlite3.Error, subprocess.TimeoutExpired):
            # A blocked delivery must not prevent handling the remainder of this bounded page.
            continue
    for _ in range(20):
        if stopped():
            break
        if not store.step(cli.send):
            break


class Identity:
    """Verify the supplied assertion with Core; never decode or fabricate claims."""
    def __init__(self, proxy_url, service_id, workspace):
        url = urlsplit(proxy_url)
        if url.scheme not in ('http', 'https') or not url.netloc or url.username or url.password or url.path not in ('', '/') or url.query or url.fragment:
            raise ValueError('proxy_origin_required')
        self.url = proxy_url.rstrip('/') + '/.treer/apps/identity/verify'
        self.service_id, self.workspace = service_id, workspace

    def __call__(self, headers):
        token = headers.get('X-Treer-Identity-Token')
        if not token or len(token) > 8192:
            return False
        class NoRedirect(HTTPRedirectHandler):
            def redirect_request(self, *_args, **_kwargs):
                return None
        request = Request(self.url, data=encoded({'token': token, 'audience': self.service_id}).encode(),
                          headers={'Content-Type': 'application/json'}, method='POST')
        try:
            with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=5) as response:
                raw = response.read(65537)
                if len(raw) > 65536:
                    return False
                result = json.loads(raw)
            claims = result.get('claims') or {}
            return (result.get('active') is True and claims.get('service_id') == self.service_id
                    and claims.get('workspace_id') == self.workspace and claims.get('principal_kind') == 'human'
                    and bool(claims.get('sub')))
        except (OSError, ValueError, TypeError, AttributeError):
            return False


def server(store, host, port, authorize):
    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(5)

        def do_GET(self):
            path = urlsplit(self.path)
            status, data = 200, None
            if path.path == '/health':
                data = {'ok': True}
            elif path.path == '/api/tree' and not authorize(self.headers):
                status, data = 401, {'error': 'authentication_required'}
            elif path.path == '/api/tree':
                try:
                    query = parse_qs(path.query, keep_blank_values=True, max_num_fields=2)
                    if set(query) - {'path'} or len(query.get('path', [''])) != 1:
                        raise ValueError('invalid_query')
                    data = store.tree(query.get('path', [''])[0])
                except ValueError:
                    status, data = 404, {'error': 'app_not_found'}
            elif path.path == '/':
                manual = '# Agent Hub\n\nApp-owned recursive internal agents.\n\nRead `GET api/tree?path=` to expand the tree. Send messages with the Treer CLI to the App path plus the returned key. Core authenticates and authorizes the App principal; internal handlers are App-owned. This service is workspace-only and must not be published publicly.\n'
                accept = self.headers.get('Accept', '')
                wants_html = root_representation(accept, self.headers.get('User-Agent', '')) == 'text/html'
                raw = (('<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Agent Hub</title><style>:root{color-scheme:light dark}body{font:1rem system-ui;max-width:50rem;margin:2rem auto;padding:0 1rem}pre{white-space:pre-wrap;overflow-wrap:anywhere}</style><main><pre>' + html.escape(manual) + '</pre></main></html>') if wants_html else manual).encode()
                self.send_response(200)
                self.send_header('Vary', 'Accept, User-Agent')
                self.send_header('Content-Type', ('text/html' if wants_html else 'text/markdown') + '; charset=utf-8')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
                return
            else:
                status, data = 404, {'error': 'not_found'}
            raw = encoded(data).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(raw)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *_):
            pass
    class Server(ThreadingHTTPServer):
        daemon_threads = True
    return Server((host, port), Handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--workspace', required=True)
    parser.add_argument('--app-agent-id', required=True)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('seed')
    commands.add_parser('stats')
    commands.add_parser('tree').add_argument('--parent', default='')
    add = commands.add_parser('register')
    add.add_argument('key'); add.add_argument('--kind', choices=['app', 'agent'], required=True)
    add.add_argument('--name', required=True); add.add_argument('--handler', choices=['echo', 'reverse'])
    commands.add_parser('remove').add_argument('key')
    serve = commands.add_parser('serve')
    serve.add_argument('--service-id', required=True); serve.add_argument('--proxy-url', required=True)
    serve.add_argument('--treer', required=True); serve.add_argument('--host', default='127.0.0.1'); serve.add_argument('--port', type=int, required=True)
    args = parser.parse_args()
    with contextlib.closing(Store(args.state_dir, args.workspace, args.app_agent_id, worker=args.command == 'serve')) as store:
        if args.command == 'seed':
            for k, kind, name, handler in [('team', 'app', 'Team', None), ('team/tools', 'app', 'Tools', None),
                                           ('team/tools/agent1', 'agent', 'Echo', 'echo'), ('team/tools/agent2', 'agent', 'Reverse', 'reverse')]:
                store.register(k, kind, name, handler)
        elif args.command == 'stats':
            print(encoded(store.stats()))
        elif args.command == 'register':
            store.register(args.key, args.kind, args.name, args.handler)
        elif args.command == 'remove':
            store.remove(args.key)
        elif args.command == 'tree':
            print(encoded(store.tree(args.parent)))
        else:
            cli = Cli(args.treer)
            http = server(store, args.host, args.port, Identity(args.proxy_url, args.service_id, args.workspace))
            thread = threading.Thread(target=http.serve_forever, daemon=True)
            thread.start()
            stopped = threading.Event()
            for sig in (signal.SIGTERM, signal.SIGINT):
                signal.signal(sig, lambda *_: stopped.set())
            try:
                while not stopped.is_set():
                    try:
                        poll_once(store, cli, stopped.is_set)
                    except (RuntimeError, OSError, ValueError, sqlite3.Error, subprocess.TimeoutExpired):
                        stopped.wait(1)
            finally:
                http.shutdown(); http.server_close(); thread.join()


if __name__ == '__main__':
    main()
