#!/usr/bin/env python3
"""Runs the real executable against loopback Controller/Proxy fixtures and a fake `treer` CLI.
No live credentials. Pass --browser to also drive the settings UI in Chromium (Playwright).
"""
import argparse
import contextlib
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

OWN = 'ag_' + '1' * 32
AGENT = 'ag_' + '3' * 32
CREDENTIAL = 'fixture-workload-no-live-credentials'
STATE = {'owner': True, 'verifies': 0, 'calls': 0}
FAKE_CLI = '''#!/usr/bin/python3
import sys, json
if sys.argv[1:] == ['--version']:
    print('treer 0.2.0 (smoke)'); sys.exit(0)
if sys.argv[1:2] == ['deny']:
    sys.stderr.write('policy_denied'); sys.exit(1)
print(json.dumps({'argv': sys.argv[1:], 'stdin': sys.stdin.read()}))
'''

def exchange(port, method, path, body=None, headers=None):
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=12)
    payload = None if body is None else json.dumps(body).encode()
    all_headers = dict(headers or {})
    if payload is not None:
        all_headers['Content-Type'] = 'application/json'
    conn.request(method, path, payload, all_headers)
    response = conn.getresponse()
    data = response.read()
    result = response.status, dict(response.getheaders()), data
    conn.close()
    return result

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *_):
        pass
    def reply(self, status, value):
        raw = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(raw)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(raw)
        self.close_connection = True
    def body(self):
        length = int(self.headers.get('Content-Length', '0'))
        assert length <= 65536
        return json.loads(self.rfile.read(length)) if length else {}
    def do_GET(self):
        self.dispatch()
    def do_POST(self):
        self.dispatch()

class Controller(Handler):
    def dispatch(self):
        assert self.headers.get('x-treer-agent-id') == OWN
        assert self.headers.get('x-treer-workload-credential') == CREDENTIAL
        assert self.headers.get('Authorization') is None
        if self.path == '/api/health':
            return self.reply(200, dict(service='treer-agent-server', workspace_id='ws_fixture', server_id='machine_fixture', cli_capabilities=['agent.prompt-delivery.v1']))  # no message.endpoint-inbox.v1
        if self.path == '/api/agents/' + OWN:
            return self.reply(200, dict(agent_id=OWN, server_id='machine_fixture'))
        STATE['calls'] += 1
        raise AssertionError('Unexpected Controller operation ' + self.path)

class Proxy(Handler):
    def dispatch(self):
        assert self.command == 'POST' and self.path == '/.treer/apps/identity/verify'
        value = self.body()
        assert value['audience'] == 'svc_fixture'
        STATE['verifies'] += 1
        now = int(time.time())
        claims = dict(sub='user_owner', aud='svc_fixture', workspace_id='ws_fixture', service_id='svc_fixture', principal_kind='human', role='owner' if STATE['owner'] else 'member', iat=now, exp=now+60)
        if value['token'] == 'fixture-agent-assertion':
            claims.update(sub=AGENT, principal_kind='agent')
            del claims['role']
        else:
            assert value['token'] == 'fixture-owner-assertion'
        self.reply(200, dict(active=True, claims=claims))

class Gateway(Handler):
    def dispatch(self):
        # Dedicated fixture origin: strip caller identities and emulate the Console's fresh assertion injection.
        headers = {k: v for k, v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('host', 'content-length', 'connection', 'content-type')}
        headers['x-treer-identity-token'] = 'fixture-owner-assertion'
        body = self.body() if self.command == 'POST' else None
        status, returned, raw = exchange(self.server.app_port, self.command, self.path, body, headers)
        self.send_response(status)
        for k, v in returned.items():
            if k.lower() not in ('connection', 'content-length'):
                self.send_header(k, v)
        self.send_header('Content-Length', str(len(raw)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(raw)
        self.close_connection = True

@contextlib.contextmanager
def server(handler):
    srv = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
    srv.daemon_threads = True
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    try:
        yield srv
    finally:
        srv.shutdown()
        srv.server_close()
        thread.join(timeout=2)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--browser', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='external-agent-smoke-') as temp, server(Controller) as controller, server(Proxy) as proxy, server(Gateway) as gateway:
        root = Path(temp).resolve()
        os.chmod(root, 0o700)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            app_port = sock.getsockname()[1]
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            owner_port = sock.getsockname()[1]
        gateway.app_port = owner_port
        origin = f'http://127.0.0.1:{gateway.server_port}'
        config = dict(workspace='ws_fixture', app='app_fixture', service='svc_fixture', agent=OWN, machine='machine_fixture', controller=f'http://127.0.0.1:{controller.server_port}/', proxy=f'http://127.0.0.1:{proxy.server_port}/', public_origin=f'http://127.0.0.1:{app_port}/', owner_origin=origin+'/', console_origin=origin+'/', tunnel_origin=origin+'/', console_url=origin+'/', listen=f'127.0.0.1:{app_port}', owner_listen=f'127.0.0.1:{owner_port}', state=str(root/'state'), treer_cli=str(root/'treer'), policy_acceptance_sha256=None, prompt_review_sha256=None)
        (root/'treer').write_text(FAKE_CLI)
        os.chmod(root/'treer', 0o700)
        path = root/'config.json'
        path.write_text(json.dumps(config))
        os.chmod(path, 0o600)
        env = dict(PATH='/usr/bin:/bin', HOME=str(root), TREER_WORKSPACE_ID='ws_fixture', TREER_AGENT_ID=OWN, TREER_SERVER_ID='machine_fixture', TREER_AGENT_SERVER_URL=config['controller'], TREER_WORKLOAD_CREDENTIAL=CREDENTIAL)
        with (root/'process.log').open('wb') as log:
            process = subprocess.Popen([str(args.binary.resolve()), str(path)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            try:
                deadline = time.monotonic()+12
                while True:
                    assert process.poll() is None, 'App startup failed'
                    try:
                        status, _, _ = exchange(app_port, 'GET', '/health')
                        if status == 200:
                            break
                    except OSError:
                        pass
                    assert time.monotonic() < deadline, 'App startup deadline'
                    time.sleep(.03)
                owner_headers = {'x-treer-identity-token': 'fixture-owner-assertion', 'Origin': origin, 'Sec-Fetch-Site': 'same-origin', 'X-External-Agent-CSRF': '1'}
                def owner(path, body=None):
                    status, headers, raw = exchange(owner_port, 'POST' if body is not None else 'GET', path, body, owner_headers)
                    assert headers['Cache-Control'] == 'no-store'
                    return status, json.loads(raw)
                status, value = owner('/owner/tokens', dict(name='Executable fixture', days=30, authority_confirmed=True))
                assert status == 201
                token = value['plaintext']
                bearer = {'Authorization': 'Bearer '+token, 'X-Treer-Client-IP': '192.0.2.1'}
                assert exchange(app_port, 'GET', '/owner/state', headers=owner_headers)[0] == 404
                cli = lambda argv: exchange(app_port, 'POST', '/v1/cli', {'argv': argv}, bearer)
                assert exchange(owner_port, 'POST', '/v1/cli', {'argv': ['agent', 'ls']}, bearer)[0] == 404
                assert cli(['agent', 'ls'])[0] == 503
                _, state = owner('/owner/state')
                assert state['readiness']['inbox']['available'] is False
                assert state['readiness']['cli'] == {'available': True, 'version': '0.2.0'}
                assert owner('/owner/activation', dict(enabled=True, generation=state['generation'], agent=OWN, policy_reviewed=True))[0] == 200
                status, _, raw = cli(['message', 'send', '--to', 'x', '--body', 'PROMPT-PRIVATE-SENTINEL'])
                result = json.loads(raw)['result']
                assert status == 200 and result['exit_code'] == 0, raw
                assert json.loads(result['stdout'])['argv'][-1] == 'PROMPT-PRIVATE-SENTINEL'
                status, _, raw = cli(['deny'])
                assert status == 200 and json.loads(raw)['result']['exit_code'] == 1
                # A workspace Agent manages through the same API, without browser headers.
                agent_headers = {'x-treer-identity-token': 'fixture-agent-assertion'}
                status, _, raw = exchange(owner_port, 'POST', '/owner/external-agents', dict(key='robot', name='Robot', days=30, authority_confirmed=True), agent_headers)
                assert status == 201, raw
                scoped = {'Authorization': 'Bearer ' + json.loads(raw)['plaintext']}
                status, _, raw = exchange(app_port, 'GET', '/v1/inbox', headers=scoped)
                assert status == 503 and json.loads(raw)['error']['code'] == 'inbox_unsupported'
                assert owner('/owner/revoke', {'id': token[5:37]})[0] == 200
                assert cli(['agent', 'ls'])[0] == 401
                STATE['owner'] = False
                assert owner('/owner/state')[0] == 401
                STATE['owner'] = True
                assert STATE['calls'] == 0
                if args.browser:
                    script = Path(__file__).with_name('browser.mjs')
                    subprocess.run(['node', str(script), origin], check=True, env=dict(os.environ))
                print('Executable smoke PASS: owner + agent management, activation, CLI gateway, inbox capability report, revoke')
            finally:
                process.terminate()
                try:
                    process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=2)
            assert token.encode() not in (root/'process.log').read_bytes()
            assert b'PROMPT-PRIVATE-SENTINEL' not in (root/'process.log').read_bytes()
            for file in (root/'state').iterdir():
                if file.is_file():
                    raw=file.read_bytes()
                    assert token.encode() not in raw and b'PROMPT-PRIVATE-SENTINEL' not in raw

if __name__ == '__main__':
    main()
