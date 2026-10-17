"""Local forwarding fixture; synthetic Core identity/CLI, never a running Treer."""
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import threading
import time

sys.path.insert(0,str(Path(__file__).parents[2]))
sys.path.insert(0,str(Path(__file__).parent))
from test_settings import IdentityProxy
from test_bridge import FakeCli
from http_app import public, settings
from http_app.bridge import Bridge
from http_app.ipc import PairServer
from http_app.store import AdminStore, QueueStore
from settings_app.auth import Auth

PREFIX='/api/workspaces/W/apps/ID/ui/'

class Gateway(BaseHTTPRequestHandler):
    def log_message(self,*_):pass
    def do_GET(self):self.forward()
    def do_POST(self):self.forward()
    def forward(self):
        path=self.path
        if path=='/console':
            raw=('<!doctype html><iframe title="HTTP settings" src="'+PREFIX+'" width="100%" height="1600"></iframe>').encode()
            self.send_response(200);self.send_header('Content-Length',str(len(raw)));self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(raw);return
        if path.startswith('/public/'):
            target=self.server.data;path=path[len('/public'):] ;human=False
        elif path.startswith(PREFIX):target=self.server.settings;path='/'+path[len(PREFIX):];human=True
        elif path.startswith('/workspace/'):
            target=self.server.settings;path=path[len('/workspace'):];human=True
        else:self.send_error(404);return
        # Match the security-relevant Proxy shape: sanitize untrusted identity
        # and cookies, rewrite Host, inject human identity only on workspace UI.
        headers={k:v for k,v in self.headers.items() if not k.lower().startswith('x-treer-') and k.lower() not in ('host','cookie','connection')}
        if human:headers['X-Treer-Identity-Token']='owner'
        body=self.rfile.read(int(self.headers.get('Content-Length','0')))
        c=http.client.HTTPConnection(*target.server_address,timeout=35)
        try:
            c.request(self.command,path,body=body,headers=headers);r=c.getresponse();raw=r.read()
            # Test-only seed: the long-poll regression starts with an OLD failure,
            # before its token response reaches the caller. No extra API route.
            if not human and path=='/v1/pair' and r.status==200 and self.server.seed_failure:
                client=json.loads(raw)['client_id']
                with self.server.queues._lock:
                    failed=self.server.queues.enqueue(client,{'body':'synthetic expired context','idempotency_key':'old-failure'})
                    self.server.queues.fail_inbound(failed,'context_unavailable')
            self.send_response(r.status)
            for k,v in r.headers.items():
                if k.lower()=='content-security-policy' and human and self.path.startswith(PREFIX):v=v.replace("frame-ancestors 'none'","frame-ancestors 'self'")
                if k.lower() not in ('connection','server','date'):self.send_header(k,v)
            self.end_headers();self.wfile.write(raw)
        finally:c.close()


def main():
    stop=threading.Event()
    signal.signal(signal.SIGTERM,lambda *_:stop.set())
    with tempfile.TemporaryDirectory(prefix='http142-',dir='/tmp') as tmp:
        root=Path(tmp);q=QueueStore(root/'data.sqlite');a=AdminStore(root/'admin.sqlite',q)
        proxy=ThreadingHTTPServer(('127.0.0.1',0),IdentityProxy);proxy.calls=[];proxy.roles={}
        gateway=ThreadingHTTPServer(('127.0.0.1',0),Gateway)
        origin='http://127.0.0.1:'+str(gateway.server_port)
        core='http://127.0.0.1:'+str(proxy.server_port)
        app=settings.Settings(q,a,origin+'/public/',Auth(core,'settings','workspace',origin))
        control=public.Server(('127.0.0.1',0),settings.Handler,app)
        pair=PairServer(root/'pair.sock',a)
        data=public.Server(('127.0.0.1',0),public.Handler,public.Application(q,root/'pair.sock'))
        gateway.settings,gateway.data=control,data
        gateway.queues=q;gateway.seed_failure=os.environ.get("HTTP_FIXTURE_OLD_FAILURE")=="1"
        servers=(proxy,control,pair,data,gateway)
        threads=[]
        for s in servers:
            t=threading.Thread(target=s.serve_forever,kwargs={'poll_interval':.01},daemon=True);t.start();threads.append(t)
        cli=FakeCli();bridge=Bridge(q,a,cli)
        def worker():
            while not stop.wait(.05):
                if bridge.inbound_once():
                    cli.deliveries=[{'delivery_id':'fixture-reply','message':{'message_id':'msg_reply','body':'Hello from POC',
                        'context_ids':['msg_in'],'sender':{'kind':'agent','id':'ag_poc'}}}]
                bridge.outbound_once(wait=0)
        t=threading.Thread(target=worker,daemon=True);t.start()
        print(json.dumps({'url':origin,'prefix':PREFIX}),flush=True)
        stop.wait()
        for s in reversed(servers):s.shutdown();s.server_close()
        t.join(2)
        for t in threads:t.join(2)

if __name__=='__main__':main()
