"""Local fake identity/ingress fixture; never invokes managed Treer or OAuth."""
import http.client
import json
import sys
import tempfile
import threading
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

APPS = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(APPS / 'mail/tests'))
sys.path.insert(0, str(APPS / 'mail'))
import test_mail
import mail

PREFIX = '/api/workspaces/workspace-a/apps/mail/ui/'

with tempfile.TemporaryDirectory() as temporary:
    proxy = test_mail.FakeProxy()
    proxy.messages[0]['body'] = '<img src=x onerror=alert(1)>'
    config = mail.Config('127.0.0.1', 0, 'svc_mail', 'http://app.invalid/',
                         f'http://127.0.0.1:{proxy.server_port}/',
                         APPS / 'mail/web/dist', Path(temporary) / 'state/mail.sqlite3')
    app = mail.MailApplication(config, mail.TreerAppClient(config.proxy_public_url, config.service_id),
                               mail.SessionStore(config.database_path))
    backend = mail.MailServer(('127.0.0.1', 0), app)

    class Gateway(BaseHTTPRequestHandler):
        def do_GET(self): self.forward()
        def do_POST(self): self.forward()
        def log_message(self, *_): pass
        def forward(self):
            if self.path == '/control':
                raw = f'<iframe title="Mail" src="{PREFIX}" style="border:0;width:100%;height:900px"></iframe>'.encode()
                self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(raw)
                return
            path = '/' + self.path[len(PREFIX):] if self.path.startswith(PREFIX) else self.path
            headers = {k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('cookie','authorization','host')}
            headers['X-Treer-Identity-Token'] = 'app-access-token'
            raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            upstream = http.client.HTTPConnection('127.0.0.1', backend.server_port, timeout=5)
            upstream.request(self.command, path, raw, headers)
            response = upstream.getresponse();body = response.read()
            self.send_response(response.status)
            for key,value in response.getheaders():
                if key.lower() not in ('content-length','connection','server','date','x-frame-options','content-security-policy'):
                    self.send_header(key,value)
            self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            upstream.close()

    gateway = ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    url = f'http://127.0.0.1:{gateway.server_port}/'
    app.config = replace(config, public_url=url)
    for server in (proxy,backend,gateway):
        threading.Thread(target=server.serve_forever,daemon=True).start()
    print(json.dumps({'url':url}),flush=True)
    threading.Event().wait()
