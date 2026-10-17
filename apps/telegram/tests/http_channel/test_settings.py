import http.client
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, str(Path(__file__).parents[2]))
from http_app import ipc, settings
from http_app.public import Server
from http_app.store import AdminStore, QueueStore
from settings_app.auth import Auth


class IdentityProxy(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        token = body['token']
        self.server.calls.append((self.path, token))
        claims = {'sub': 'ag_poc' if token == 'agent' else token, 'service_id': 'settings', 'workspace_id': 'workspace', 'principal_kind': 'agent' if token == 'agent' else 'human'}
        if token == 'wrong-audience': claims['service_id'] = 'other'
        if token == 'wrong-workspace': claims['workspace_id'] = 'other'
        self.reply({'active': token not in ('forged','expired'), 'claims': claims})
    def do_GET(self):
        self.server.calls.append((self.path, self.headers.get('Authorization')))
        self.reply({'capabilities':['app.directory-paths.v1'], 'principals':[
            {'id': p, 'name':p, 'kind':'human', 'role':self.server.roles.get(p, p)} for p in ('owner','admin','member')
        ] + [{'id':'ag_poc','name':'Same name','kind':'agent','path':'/one/Same name'},
             {'id':'ag_other','name':'Same name','kind':'agent','path':'/two/Same name'}]})
    def reply(self, value):
        raw=json.dumps(value).encode(); self.send_response(200)
        self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)


class SettingsTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='http142-',dir='/tmp');self.root=Path(self.tmp.name)
        self.q=QueueStore(self.root/'data.sqlite');self.a=AdminStore(self.root/'admin.sqlite',self.q)
        self.proxy=ThreadingHTTPServer(('127.0.0.1',0),IdentityProxy);self.proxy.calls=[];self.proxy.roles={}
        self.origin='http://127.0.0.1:'+str(self.proxy.server_port)
        self.app=settings.Settings(self.q,self.a,'https://public.example/',Auth(self.origin,'settings','workspace',self.origin))
        self.server=Server(('127.0.0.1',0),settings.Handler,self.app)
        self.pair=ipc.PairServer(self.root/'pair.sock',self.a)
        self.threads=[]
        for server in (self.proxy,self.server,self.pair):
            t=threading.Thread(target=server.serve_forever,kwargs={'poll_interval':.01},daemon=True);t.start();self.threads.append(t)
    def tearDown(self):
        for server in (self.server,self.proxy,self.pair):server.shutdown();server.server_close()
        for t in self.threads:t.join(2)
        self.tmp.cleanup()
    def request(self, method='GET', path='/v1/state', body=None, token='owner', headers=None):
        headers=dict(headers or {})
        if token is not None:headers['X-Treer-Identity-Token']=token
        if body is not None:headers['Content-Type']='application/json'
        c=http.client.HTTPConnection(*self.server.server_address,timeout=5)
        try:
            c.request(method,path,body=None if body is None else json.dumps(body),headers=headers)
            r=c.getresponse();raw=r.read();return r.status,raw,dict(r.headers)
        finally:c.close()
    def write(self, path, body, **kwargs):
        return self.request('POST',path,body,headers={'Origin':self.origin,'X-Http-Settings':'1','Sec-Fetch-Site':'same-origin'},**kwargs)
    def test_identity_every_request_and_no_fallback(self):
        for token in ('forged','expired','wrong-audience','wrong-workspace',''):
            self.assertEqual(self.request(token=token,headers={'Cookie':'valid','Authorization':'Bearer owner'})[0],401)
        self.assertEqual(self.request(token=None,headers={'Authorization':'Bearer owner'})[0],401)
        for token in ('agent','member'):
            self.assertEqual(self.request(token=token)[0],403)
        for token in ('admin','owner'):
            self.assertEqual(self.request(token=token)[0],200)
        self.proxy.roles['owner']='member'
        self.assertEqual(self.request()[0],403,'directory changes must apply without cached identities')
        self.assertGreaterEqual(sum(p=='/.treer/apps/identity/verify' for p,_ in self.proxy.calls),9)
    def test_csrf_and_visible_stable_poc(self):
        body={'label':'client','poc':'ag_poc'}
        for headers in ({},{'Origin':'https://evil.example','X-Http-Settings':'1'}, {'Origin':self.origin},
                        {'Origin':self.origin,'X-Http-Settings':'1','Sec-Fetch-Site':'cross-site'},
                        {'Origin':'null','X-Http-Settings':'1'}):
            self.assertEqual(self.request('POST','/v1/pairings',body,headers=headers)[0],403)
        self.assertEqual(self.write('/v1/pairings',{**body,'poc':'hidden'})[0],403)
        status,raw,_=self.write('/v1/pairings',body);self.assertEqual(status,201)
        result=json.loads(raw); credentials=ipc.redeem(self.root/'pair.sock',result['code'])
        state=json.loads(self.request()[1]);self.assertEqual(len(state['agents']),2)
        self.assertNotIn(result['code'],str(state));self.assertNotIn(credentials['token'],str(state))
        self.assertNotIn('token_hash',str(state));self.assertNotIn('salt',str(state))
        client=result['client_id']
        pending=json.loads(self.write(f'/v1/clients/{client}/rotate',body)[1])
        self.assertEqual(self.write(f'/v1/clients/{client}/revoke',{})[0],200)
        with self.assertRaises(Exception):ipc.redeem(self.root/'pair.sock',pending['code'])
        self.assertEqual(self.write('/v1/pause',{'paused':True})[0],200)
        self.assertTrue(json.loads(self.request()[1])['paused'])
    def test_representation_assets_and_unknown_paths(self):
        for accept,media in (('text/html','text/html'),('text/markdown','text/markdown'),('text/html;q=.2,text/markdown','text/markdown')):
            status,raw,headers=self.request(path='/',headers={'Accept':accept})
            self.assertEqual(status,200);self.assertTrue(headers['Content-Type'].startswith(media))
            self.assertIn('Accept',headers['Vary']);self.assertIn('Content-Security-Policy',headers)
            self.assertNotIn(b'Bearer owner',raw)
        for path in ('/app.js','/app.css','/base.css'):
            self.assertEqual(self.request(path=path)[0],200)
            self.assertEqual(self.request(path=path,token=None)[0],401)
        self.assertEqual(self.request(path='/missing')[0],404)
    def test_pair_socket_rejects_management_and_other_uid(self):
        before=list(self.a.list_users('http',self.a.ACCOUNT))
        for value in ({'op':'revoke','client_id':'x'},{'code':'invalid','poc':'attacker'}, {'code':'x','code2':'y'}):
            with socket.socket(socket.AF_UNIX) as s:
                s.connect(str(self.root/'pair.sock'));s.sendall(json.dumps(value).encode()+b'\n')
                self.assertEqual(json.loads(s.makefile('rb').readline())['error']['status'],400)
        self.assertEqual(before,self.a.list_users('http',self.a.ACCOUNT))
        with socket.socket(socket.AF_UNIX) as s:
            with patch('http_app.ipc.peer_uid',return_value=os.getuid()+1):
                self.assertFalse(self.pair.verify_request(s,None))
            with patch('http_app.ipc.peer_uid',side_effect=OSError()):
                self.assertFalse(self.pair.verify_request(s,None))

    def test_pair_cooldown_diagnostic_is_visible_without_database_failure_writes(self):
        with self.a._connect() as db:before='\n'.join(db.iterdump())
        for _ in range(11):
            try:self.a.redeem('f'*32+'.XXXXXXXXXX')
            except Exception:pass
        with self.a._connect() as db:self.assertEqual('\n'.join(db.iterdump()),before)
        state=json.loads(self.request()[1])
        self.assertTrue(state['pair_backoff']['active'])
        self.assertTrue(state['pair_backoff']['expires_at'].endswith('Z'))
        self.assertNotIn('XXXXXXXXXX',str(state))

    def test_drop_counts_and_reasons_are_visible_without_message_bodies(self):
        for _ in range(3):self.a.unroutable('drop-fixture',99,'undeliverable_queue_full')
        state=json.loads(self.request()[1])
        self.assertEqual(state['drops']['total'],1)
        self.assertEqual(state['drops']['by_reason'][0]['reason'],'undeliverable_queue_full')
        self.assertTrue(state['drops']['by_reason'][0]['last_at'].endswith('Z'))
