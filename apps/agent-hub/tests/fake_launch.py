#!/usr/bin/env python3
"""Synthetic `treer-launch --no-register` for the Agent Hub tests.

Serves a minimal Agent Interface (status, state, prompts, transcript, a UI page
and a WebSocket) on a loopback port, writes the port file and records what the
hub asked for under FAKE_LAUNCH_DIR/<job>/. FAKE_LAUNCH_DIR/<job>/control.json
steers it: {"status": "idle"|"working", "ready": bool, "exit": CODE}.
"""
import base64
import hashlib
import json
import os
import signal
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]
opts, flags, i = {}, set(), 0
while i < len(args):
    if args[i] == '--no-register':
        flags.add('no-register')
        i += 1
        continue
    opts[args[i]] = args[i + 1]
    i += 2
if 'no-register' not in flags or '--state-dir' not in opts:
    print('fake launcher: --no-register --state-dir required', file=sys.stderr)
    raise SystemExit(2)
job = os.environ['TREER_HUB_JOB']
record = os.path.join(os.environ['FAKE_LAUNCH_DIR'], job)
os.makedirs(record, exist_ok=True)
lock = threading.Lock()


def append(name, value):
    with lock, open(os.path.join(record, name), 'a') as f:
        f.write(json.dumps(value) + '\n')


def control():
    try:
        with open(os.path.join(record, 'control.json')) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


session_file = os.path.join(opts['--state-dir'], 'session')
session = open(session_file).read() if os.path.exists(session_file) else None
if session and opts.get('--session-id') not in (None, session):
    print('private_state_session_mismatch', file=sys.stderr)
    raise SystemExit(3)
session = session or opts.get('--session-id') or 'session-' + uuid.uuid4().hex[:8]
with open(session_file, 'w') as f:
    f.write(session)
prompts_file = os.path.join(opts['--state-dir'], 'prompts.json')
prompts = json.load(open(prompts_file)) if os.path.exists(prompts_file) else []
grandchild = subprocess.Popen(['sleep', '300'])   # same process group: must die with the worker


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def reply(self, status, value, kind='application/json'):
        raw = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(raw)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        append('requests.jsonl', {'method': self.command, 'path': self.path, 'headers': dict(self.headers.items())})
        c = control()
        path = self.path.split('?')[0]
        if path == '/ws' and self.headers.get('Upgrade', '').lower() == 'websocket':
            return self.websocket()
        if path == '/v1/manifest':
            return self.reply(200, {'instance_id': os.environ.get('AGENT_UI_INSTANCE_ID'), 'protocol': 'treer.agent-interface/v1'})
        if path == '/api/health':
            return self.reply(200, {'ok': c.get('ready', True), 'ready': c.get('ready', True)})
        if path == '/v1/status':
            return self.reply(200, {'agent_id': '', 'status': c.get('status', 'working'), 'queue_length': 0})
        if path == '/api/state':
            return self.reply(200, {'ready': True, 'runtime': {'state': c.get('status', 'working')}, 'thread': {'id': session}})
        if path == '/v1/transcript':
            entries = [{'role': 'user', 'kind': 'userMessage', 'content': p['text']} for p in prompts]
            return self.reply(200, {'agent_id': '', 'page': 0, 'entries': entries})
        if path == '/' or path.startswith('/index'):
            return self.reply(200, b'<!doctype html><base href="./"><title>fake worker</title><script src="ui.js"></script>',
                              'text/html; charset=utf-8')
        if path == '/ui.js':
            return self.reply(200, b'// fake', 'text/javascript')
        return self.reply(404, {'error': 'not found'})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length') or 0)) or b'{}')
        append('requests.jsonl', {'method': 'POST', 'path': self.path, 'headers': dict(self.headers.items())})
        if self.path == '/v1/prompts':
            if any(p['operation_id'] == body.get('operation_id') for p in prompts):
                return self.reply(202, {'accepted': True, 'duplicate': True})
            prompts.append(body)
            with open(prompts_file, 'w') as f:
                json.dump(prompts, f)
            append('prompts.jsonl', body)
            return self.reply(202, {'accepted': True, 'operation_id': body.get('operation_id')})
        if self.path == '/api/prompt':
            append('ui-prompts.jsonl', body)
            return self.reply(200, {'ok': True})
        return self.reply(404, {'error': 'not found'})

    def websocket(self):
        key = self.headers['Sec-WebSocket-Key']
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        self.close_connection = True
        sock = self.connection
        self.frame(sock, json.dumps({'type': 'state', 'session': session}).encode())
        while True:
            head = self.rfile.read(2)
            if len(head) < 2:
                return
            length = head[1] & 0x7f
            if length == 126:
                length = int.from_bytes(self.rfile.read(2), 'big')
            mask = self.rfile.read(4) if head[1] & 0x80 else b'\0\0\0\0'
            data = bytes(b ^ mask[n % 4] for n, b in enumerate(self.rfile.read(length)))
            if head[0] & 0x0f == 8:
                return
            self.frame(sock, b'echo:' + data)

    @staticmethod
    def frame(sock, data):
        sock.sendall(bytes([0x81, len(data)]) + data if len(data) < 126 else bytes([0x81, 126]) + len(data).to_bytes(2, 'big') + data)

    def log_message(self, *_):
        pass


class Server(ThreadingHTTPServer):
    daemon_threads = True


server = Server(('127.0.0.1', 0), Handler)
append('spawns.jsonl', {'argv': sys.argv, 'cwd': os.getcwd(), 'pid': os.getpid(), 'pgid': os.getpgid(0),
                        'grandchild': grandchild.pid, 'port': server.server_address[1], 'session': session,
                        'env': {k: os.environ.get(k) for k in ('TREER_HUB_URL', 'TREER_HUB_JOB', 'TREER_HUB_TOKEN',
                                                                'TREER_AGENT_ID', 'TREER_AGENT_PRIVATE_STATE_DIR',
                                                                'RUST_SPIKE_REGISTER', 'AGENT_UI_INSTANCE_ID',
                                                                'GIT_AUTHOR_NAME', 'TREER_WORKLOAD_CREDENTIAL')}})


def stop(*_):
    grandchild.kill()
    os._exit(0)


signal.signal(signal.SIGTERM, stop)
fd = os.open(opts['--port-file'], os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
os.write(fd, str(server.server_address[1]).encode())
os.close(fd)
threading.Thread(target=server.serve_forever, daemon=True).start()
while True:
    current = control()
    if current.get('exit') is not None:
        code = current.pop('exit')
        with open(os.path.join(record, 'control.json'), 'w') as f:
            json.dump(current, f)   # one crash per request
        os._exit(code)   # the grandchild is left for the hub to clean up
    time.sleep(0.05)
