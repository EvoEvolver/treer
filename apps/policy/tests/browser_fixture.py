"""Hermetic loopback ingress/tunnel which injects synthetic verified identity."""
import http.client
import json
import os
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
from test_identity import FakeProxy, policy, ADMIN

PREFIX='/api/workspaces/ws_test/apps/app_policy/ui/'
class Gateway(BaseHTTPRequestHandler):
    def do_GET(self): self.forward()
    def do_POST(self): self.forward()
    def forward(self):
        if self.path=='/control':
            body=f'<!doctype html><style>body{{margin:0}}iframe{{width:100%;height:100vh;border:0}}</style><iframe src="{PREFIX}"></iframe>'.encode()
            self.send_response(200);self.send_header('Content-Length',str(len(body)));self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(body);return
        path=self.path
        if path.startswith(PREFIX): path='/'+path[len(PREFIX):]
        headers={k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('host','connection','cookie')}
        # Direct fallback route deliberately has no injected identity.
        if path.startswith('/fallback/'):
            path='/'+path[len('/fallback/'):]
        else: headers['X-Treer-Identity-Token']='human-owner'
        length=int(self.headers.get('Content-Length','0'));body=self.rfile.read(length) if length else None
        connection=http.client.HTTPConnection('127.0.0.1',self.server.app.server_port,timeout=10)
        connection.request(self.command,path,body,headers);response=connection.getresponse();data=response.read()
        self.send_response(response.status)
        for key,val in response.getheaders():
            if key.lower() not in ('server','date','connection','content-length'): self.send_header(key,val)
        self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data);connection.close()
    def log_message(self,*args): pass

with tempfile.TemporaryDirectory() as root:
    proxy=ThreadingHTTPServer(('127.0.0.1',0),FakeProxy);proxy.calls=[]
    threading.Thread(target=proxy.serve_forever,daemon=True).start()
    policy.PolicyHandler.store=policy.PolicyStore(Path(root)/'policy.json','ws_test')
    app=policy.PolicyServer(('127.0.0.1',0),policy.PolicyHandler);app.admin_token=ADMIN
    gateway=ThreadingHTTPServer(('127.0.0.1',0),Gateway);gateway.app=app
    origin=f'http://127.0.0.1:{gateway.server_port}'
    os.environ['TREER_AGENT_ID']='runtime_policy'
    os.environ['TREER_WORKLOAD_CREDENTIAL']='synthetic-runtime-credential'
    os.environ['POLICY_PROXY_URL']=f'http://127.0.0.1:{proxy.server_port}'
    app.auth=policy.Auth(policy.AuthStore(Path(root)/'auth.sqlite3'),os.environ['POLICY_PROXY_URL'],'svc_policy','ws_test',origin+PREFIX)
    threading.Thread(target=app.serve_forever,daemon=True).start()
    print(json.dumps({'url':origin+'/','prefix':PREFIX}),flush=True)
    gateway.serve_forever()
