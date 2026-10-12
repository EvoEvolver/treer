"""Hermetic HTTP fixture; never accesses production Treer identity or directories."""
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from soul import SoulHTTPServer, SoulStore

PRINCIPALS = [{'id': 'ag_owner', 'kind': 'agent'}, {'id': 'ag_other', 'kind': 'agent'},
              {'id': 'u_human', 'kind': 'human', 'role': 'member'}]
TOKENS = {'owner': PRINCIPALS[0], 'other': PRINCIPALS[1], 'human': PRINCIPALS[2]}


class Proxy(BaseHTTPRequestHandler):
    def reply(self, value, status=200):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        if self.path == '/api/apps/oauth/token':
            from datetime import datetime, timezone, timedelta
            self.reply({'access_token':'human','expires_at':(datetime.now(timezone.utc)+timedelta(hours=1)).isoformat()})
            return
        body = json.loads(raw)
        token = body.get('token')
        if self.path == '/.treer/apps/identity/verify':
            principal = TOKENS.get(token)
            claims = dict(sub=principal['id'], principal_kind=principal['kind'], workspace_id='ws_test', service_id='svc_soul') if principal else {}
            if token == 'wrong-audience':
                claims = dict(sub='ag_owner', principal_kind='agent', workspace_id='ws_test', service_id='svc_other')
            self.reply({'active': bool(principal) or token == 'wrong-audience', 'claims': claims})
        else:
            self.reply({'error': {'code': 'not_found'}}, 404)

    def do_GET(self):
        if self.path == '/api/apps/svc_soul/directory' and self.headers.get('Authorization', '').removeprefix('Bearer ') in TOKENS:
            self.reply({'principals': PRINCIPALS})
        else:
            self.reply({'error': {'code': 'identity_invalid'}}, 401)

    def log_message(self, *_):
        pass


def sealed_environment(extra=None):
    # Allowlist instead of inheriting managed-Agent credentials, hooks or proxy variables.
    env = {'PATH': os.defpath, 'HOME': '/nonexistent', 'PYTHONUTF8': '1', 'LANG': 'en_US.UTF-8'}
    env.update(extra or {})
    return env


def servers(root, max_count=10000, max_bytes=10*1024**3):
    proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    app_root = Path(__file__).resolve().parents[1]
    server = SoulHTTPServer(('127.0.0.1', 0), SoulStore(root, max_count, max_bytes),
                           'http://127.0.0.1', app_root / 'client.py', '/never-invoke-treer', 64*1024*1024)
    from soul_auth import Auth
    url = 'http://127.0.0.1:' + str(server.server_port)
    server.public_url = url
    from soul import bundled_client
    import hashlib
    server.client_bytes = bundled_client(app_root / 'client.py', url)
    server.client_sha256 = hashlib.sha256(server.client_bytes).hexdigest()
    server.auth = Auth(server.store, f'http://127.0.0.1:{proxy.server_port}', 'svc_soul', 'ws_test', url)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, proxy


if __name__ == '__main__':
    import http.client
    import signal
    import tempfile
    from urllib.parse import urlsplit
    environment = sealed_environment()
    os.environ.clear()
    os.environ.update(environment)
    temporary = tempfile.TemporaryDirectory()
    server, proxy = servers(Path(temporary.name) / 'data')
    prefix = '/api/workspaces/ws_test/apps/app_soul/ui/'

    class Gateway(BaseHTTPRequestHandler):
        def do_GET(self):
            self.forward()
        def do_DELETE(self):
            self.forward()
        def forward(self):
            if self.path == '/control':
                body = f'<!doctype html><meta charset="utf-8"><iframe title="Soul" src="{prefix}" style="width:100%;height:95vh;border:0"></iframe>'.encode()
                self.send_response(200); self.send_header('Content-Type','text/html; charset=utf-8'); self.end_headers(); self.wfile.write(body)
                return
            path = self.path[len(prefix)-1:] if self.path.startswith(prefix) else self.path
            headers = {k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('cookie','authorization','host')}
            headers['X-Treer-Identity-Token'] = 'human'
            headers['Host'] = 'svc_soul.internal'
            connection = http.client.HTTPConnection('127.0.0.1', server.server_port)
            connection.request(self.command, path, headers=headers)
            response = connection.getresponse()
            body=response.read()
            self.send_response(response.status)
            for key,value in response.getheaders():
                if key.lower() not in ('server','date','connection'):
                    self.send_header(key,value)
            self.end_headers(); self.wfile.write(body)
            connection.close()
        def log_message(self,*_): pass

    gateway = ThreadingHTTPServer(('127.0.0.1',0),Gateway)
    threading.Thread(target=gateway.serve_forever,daemon=True).start()
    gateway_url = f'http://127.0.0.1:{gateway.server_port}'
    server.auth.proxy_url = gateway_url + '/'
    # Identity verification remains on the fake Proxy, browser Origin uses gateway.
    from soul_auth import Auth
    identity_auth = Auth(server.store,f'http://127.0.0.1:{proxy.server_port}','svc_soul','ws_test',gateway_url)
    server.auth.request = identity_auth.request
    server.auth.public_url = gateway_url + '/'
    raw_manifest = {'schema_version':1,'name':'<img src=x onerror=alert(1)>','environment':{}}
    import io, tarfile
    stream=io.BytesIO()
    with tarfile.open(fileobj=stream,mode='w') as archive:
        raw=json.dumps(raw_manifest).encode()
        info=tarfile.TarInfo('manifest.json'); info.size=len(raw)
        archive.addfile(info,io.BytesIO(raw))
    descriptor,path=server.store.temporary_upload()
    with os.fdopen(descriptor,'wb') as output: output.write(stream.getvalue())
    record=server.store.save(Path(path),TOKENS['human'],{})
    Path(path).unlink()
    print(json.dumps({'url':gateway_url,'prefix':prefix,'soul_id':record['soul_id']}),flush=True)
    signal.pause()
