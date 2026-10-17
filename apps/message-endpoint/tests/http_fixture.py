#!/usr/bin/env python3
"""Owned loopback-only hub/Proxy/CLI fixture. Never uses installed Treer."""
import contextlib
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

TOKEN = 'fixture.owner.token'
PREFIX = '/api/workspaces/ws_test/apps/svc_hub/ui/'
LONG_PATH = '/team/' + 'Long same-named Agent ' * 7 + '<img src=x onerror=alert(1)>'

class Quiet(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def reply(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class Proxy(Quiet):
    def do_POST(self):
        size = int(self.headers.get('Content-Length', '0'))
        if size > 16384:
            return self.reply(400, {})
        body = json.loads(self.rfile.read(size))
        if self.path != '/.treer/apps/identity/verify' or body != {'token': TOKEN, 'audience': 'svc_hub'}:
            return self.reply(401, {'active': False})
        self.reply(200, {'active': True, 'claims': {'sub': 'user_owner', 'principal_kind': 'human', 'workspace_id': 'ws_test', 'service_id': 'svc_hub', 'exp': int(time.time()) + 300}})
    def do_GET(self):
        if self.path != '/api/apps/svc_hub/directory' or self.headers.get('Authorization') != 'Bearer ' + TOKEN:
            return self.reply(403, {})
        self.reply(200, {'principals': [{'id': 'user_owner', 'kind': 'human', 'role': 'owner'}]})

@contextlib.contextmanager
def server(handler):
    instance = ThreadingHTTPServer(('127.0.0.1', 0), handler)
    instance.daemon_threads = True
    thread = threading.Thread(target=instance.serve_forever, daemon=True)
    thread.start()
    try:
        yield instance
    finally:
        instance.shutdown()
        instance.server_close()
        thread.join(timeout=3)

@contextlib.contextmanager
def fixture(binary, editable=False):
    with tempfile.TemporaryDirectory(prefix='endpoint-http-fixture-') as temp, server(Proxy) as proxy:
        root = Path(temp).resolve()
        root.chmod(0o700)
        cli = root / 'fixture-cli'
        # Fixed argv, no environment credential access and no network calls.
        cli.write_text('#!/usr/bin/python3\nimport json,sys\nif sys.argv[1:3]==["message","receive"]: print(json.dumps({"deliveries":[],"remaining_unacknowledged":0}))\nelif sys.argv[1:4]==["agent","show","--"] and sys.argv[4] in ("ag_new","ag_recipient","ag_wrong_workspace","ag_unplaced"): print(json.dumps({"agent_id":sys.argv[4],"workspace_id":"ws_other" if sys.argv[4]=="ag_wrong_workspace" else "ws_test","server_id":"machine_fixture","kind":"command","cwd":"/private/fixture","status":"running","path":None if sys.argv[4]=="ag_unplaced" else "/team/"+sys.argv[4]}))\nelse: sys.exit(1)\n')
        cli.chmod(0o700)
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            port = reservation.getsockname()[1]
        config = root / 'config.json'
        config.write_text(json.dumps({'hub_id':'app_hub','workspace_id':'ws_test','service_id':'svc_hub','proxy_url':f'http://127.0.0.1:{proxy.server_port}/','public_url':f'http://127.0.0.1:{port}/' if editable else 'https://hub.example/','access':'workspace','cli':str(cli),'cli_sha256':hashlib.sha256(cli.read_bytes()).hexdigest(),'state_directory':str(root/'state'),'listen':f'127.0.0.1:{port}'}))
        config.chmod(0o600)
        environment = {'HOME':str(root),'TMPDIR':str(root),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin','TREER_AGENT_ID':'app_hub','TREER_WORKSPACE_ID':'ws_test'}
        process = subprocess.Popen([str(binary),'--config',str(config)],env=environment,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        def request(path, headers=None, method='GET', body=None):
            connection = http.client.HTTPConnection('127.0.0.1',port,timeout=3)
            try:
                connection.request(method,path,body=body,headers=headers or {})
                response=connection.getresponse()
                return response.status,dict(response.getheaders()),response.read()
            finally:
                connection.close()
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError('fixture hub exited before health')
                try:
                    if request('/health')[0]==200:
                        break
                except (OSError,http.client.HTTPException):
                    time.sleep(.05)
            else:
                raise AssertionError('fixture hub did not listen')
            db=sqlite3.connect(root/'state/hub.sqlite3')
            binding={'endpoint_id':'ag_demo','generation':1,'recipient':'ag_recipient','expected_path':LONG_PATH,'expected_cwd':'/private/fixture','expected_server_id':'machine_fixture'}
            db.execute('INSERT INTO endpoints(id,generation,recipient,paused) VALUES(?,?,?,1)',('ag_demo',1,'ag_recipient'))
            db.execute('INSERT INTO hub_bindings(endpoint,definition) VALUES(?,?)',('ag_demo',json.dumps(binding)))
            db.commit();db.close()
            def forward_handler(embedded):
                class Forward(Quiet):
                    def do_GET(self):
                        if embedded and self.path=='/console':
                            page=f'<!doctype html><title>Fixture Console</title><iframe title="Endpoint App" src="{PREFIX}" style="width:100%;height:1200px;border:0"></iframe>'.encode()
                            self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(page)));self.end_headers();self.wfile.write(page);return
                        if embedded and not self.path.startswith(PREFIX):
                            return self.reply(404,{})
                        path='/'+self.path[len(PREFIX):] if embedded else self.path
                        # Model only the documented forwarding contract: strip
                        # client identity, inject a test verified owner identity.
                        h={k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('host','cookie')}
                        h['X-Treer-Identity-Token']=TOKEN
                        status,headers,body=request(path,h)
                        self.send_response(status)
                        for k,v in headers.items():
                            if k.lower() in ('connection','server','date','transfer-encoding','content-length'):continue
                            if embedded and k.lower()=='content-security-policy':
                                v=';'.join(p for p in v.split(';') if not p.strip().startswith('frame-ancestors'))
                            self.send_header(k,v)
                        self.send_header('Content-Length',str(len(body)))
                        self.end_headers();self.wfile.write(body)
                return Forward
            with server(forward_handler(False)) as wildcard, server(forward_handler(True)) as embedded:
                yield {'url':f'http://127.0.0.1:{port}/','wildcard':f'http://127.0.0.1:{wildcard.server_port}/','embedded':f'http://127.0.0.1:{embedded.server_port}/console','prefix':PREFIX,'request':request,'pid':process.pid,'state':root/'state'}
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:process.wait(timeout=20)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
            if process.stderr:process.stderr.close()

if __name__=='__main__':
    signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
    with fixture(Path(sys.argv[1]).resolve(),editable='--editable' in sys.argv[2:]) as f:
        print(json.dumps({k:f[k] for k in ['url','wildcard','embedded','prefix']}),flush=True)
        while True:time.sleep(1)
