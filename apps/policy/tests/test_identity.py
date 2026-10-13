"""Loopback synthetic Proxy + App tests. Never use runtime credentials/config."""
import http.client
import importlib.util
import json
import os
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('identity_policy', Path(__file__).resolve().parents[1] / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
ADMIN = 'synthetic-operator-secret-at-least-32-chars'

class FakeProxy(BaseHTTPRequestHandler):
    calls = []
    def do_GET(self): self.dispatch()
    def do_POST(self): self.dispatch()
    def dispatch(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))) or b'{}') if self.headers.get('Content-Type') != 'application/x-www-form-urlencoded' else self.rfile.read(int(self.headers['Content-Length']))
        self.server.calls.append((self.command,self.path,self.headers.get('Authorization'),body))
        token = self.headers.get('Authorization','').removeprefix('Bearer ')
        status=200
        if self.path == '/.treer/apps/identity/verify':
            token=body['token'];valid=token in ('human-owner','human-member','expired','wrong-audience','revoked')
            result={'active':valid,'claims':{'service_id':'other' if token=='wrong-audience' else 'svc_policy',
                'workspace_id':'ws_test','principal_kind':'human','sub':'owner' if token!='human-member' else 'member',
                'exp':time.time()+(-60 if token=='expired' else 60)}}
        elif self.path.endswith('/directory'):
            result={'principals':[] if token=='revoked' else [{'id':'owner','kind':'human','name':'Owner'},{'id':'member','kind':'human','name':'Member'}]}
        elif '/policy/' in self.path:
            if token!='human-owner': status=403;result={'error':{'code':'workspace_owner_required'}}
            elif self.path.endswith('/validate'): result={'valid':True,'schema_version':body['schema_version']}
            elif self.path.endswith('/explain'): result={'explanation':{'computed_effect':'deny','effective_decision':'allow','mode':'monitor','matched_rule_id':'test-rule','policy_revision':2}}
            else: result={'decisions':[{'computed_effect':'deny','effective_decision':'allow','action':'agent.prompt'}], 'scope':'this_proxy'}
        else: status=404;result={'error':{'code':'not_found'}}
        encoded=json.dumps(result).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(encoded)));self.end_headers();self.wfile.write(encoded)
    def log_message(self,*args): pass

class IdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.environment=patch.dict(os.environ, {'PATH':os.defpath}, clear=True);self.environment.start()
        self.proxy=ThreadingHTTPServer(('127.0.0.1',0),FakeProxy);self.proxy.calls=[]
        self.proxy_thread=threading.Thread(target=self.proxy.serve_forever,daemon=True);self.proxy_thread.start()
        class Handler(policy.PolicyHandler):
            def log_message(self,*args): pass
        Handler.store=policy.PolicyStore(Path(self.temp.name)/'policy.json','ws_test')
        self.app=policy.PolicyServer(('127.0.0.1',0),Handler);self.app.admin_token=ADMIN
        self.origin=f'http://127.0.0.1:{self.app.server_port}'
        self.app.auth=policy.Auth(policy.AuthStore(Path(self.temp.name)/'auth.sqlite3'),f'http://127.0.0.1:{self.proxy.server_port}','svc_policy','ws_test',self.origin+'/')
        self.thread=threading.Thread(target=self.app.serve_forever,daemon=True);self.thread.start()
    def tearDown(self):
        for server,thread in ((self.app,self.thread),(self.proxy,self.proxy_thread)):
            server.shutdown();server.server_close();thread.join(2)
        self.environment.stop();self.temp.cleanup()
    def request(self,method,path,body=None,identity='human-owner',extra=None):
        headers={'Authorization':'Bearer '+ADMIN,'X-Policy-Request':'1','Origin':self.origin,'Sec-Fetch-Site':'same-origin'}
        if identity is not None: headers['X-Treer-Identity-Token']=identity
        headers.update(extra or {})
        data=None if body is None else json.dumps(body)
        if data is not None: headers['Content-Type']='application/json'
        connection=http.client.HTTPConnection('127.0.0.1',self.app.server_port,timeout=5)
        connection.request(method,path,data,headers);response=connection.getresponse();raw=response.read();result=json.loads(raw) if response.getheader('Content-Type','').startswith('application/json') else raw.decode()
        response_headers=response.getheaders();connection.close();return response.status,result,response_headers
    def test_injected_identity_precedes_cookie_and_operator_capability(self):
        with self.app.auth.store.connect() as db: db.execute('INSERT INTO sessions VALUES(?,?,?)',(policy.digest('session'),'human-owner',time.time()+100))
        for token,expected in [('forged',401),('',401),('expired',401),('wrong-audience',401),('revoked',403)]:
            status,_,_=self.request('GET','/v1/session',identity=token,extra={'Cookie':'policy_session=session'})
            self.assertEqual(status,expected,token)
            status,_,_=self.request('POST','/v1/policy/invalidate',{},identity=token)
            self.assertEqual(status,expected,token)
        status,body,_=self.request('GET','/v1/session');self.assertEqual(status,200);self.assertEqual(body['principal']['id'],'owner')
        verifies=[c for c in self.proxy.calls if c[1].endswith('/verify')]
        self.assertTrue(verifies);self.assertNotIn(ADMIN,json.dumps(self.proxy.calls))
    def test_owner_diagnostics_no_service_deputy_and_csrf(self):
        request={'subject':{'kind':'agent','id':'a'},'action':'agent.prompt','resource':{'kind':'agent','id':'b'}}
        for path in ('/v1/policy/explain','/v1/policy/decisions'):
            method='GET' if path.endswith('decisions') else 'POST'
            for identity,expected in [('human-owner',200),('human-member',403),('forged',401)]:
                status,_,_=self.request(method,path,None if method=='GET' else request,identity)
                self.assertEqual(status,expected)
        for extra in ({'Origin':'https://evil.test'}, {'Sec-Fetch-Site':'cross-site'},{'X-Policy-Request':''}):
            self.assertEqual(self.request('POST','/v1/policy/explain',request,extra=extra)[0],403)
        self.assertNotIn(ADMIN,json.dumps(self.proxy.calls))
    def test_path_publish_requires_owner_core_validation_and_negotiation(self):
        document=policy.template_document('team-isolation',{})['document']
        body={'expected_revision':1,'mode':'monitor','document':document}
        self.assertEqual(self.request('POST','/v1/policy/publish',body,'human-member')[0],403)
        self.assertEqual(self.request('POST','/v1/policy/publish',body,extra={'Authorization':'Bearer wrong'})[0],401)
        self.assertEqual(self.request('POST','/v1/policy/publish',body)[0],200)
        self.assertEqual(self.request('GET','/v1/policy/bundle?workspace_id=ws_test')[0],409)
        status,bundle,_=self.request('GET','/v1/policy/bundle?workspace_id=ws_test&max_schema_version=2')
        self.assertEqual(status,200);self.assertEqual(bundle['document'],document)
        self.assertEqual(self.request('POST','/v1/policy/rollback',{'expected_revision':2,'revision':1})[0],200)
        self.assertEqual(self.request('POST','/v1/policy/rollback',{'expected_revision':3,'revision':2},'human-member')[0],403)
        self.assertEqual(self.request('POST','/v1/policy/rollback',{'expected_revision':3,'revision':2})[0],200)
    def test_oauth_pkce_browser_binding_consumed_once_and_private_storage(self):
        auth=self.app.auth
        url,browser=auth.start();query=parse_qs(urlsplit(url).query)
        self.assertEqual(query['code_challenge_method'],['S256'])
        self.assertEqual(query['redirect_uri'],[self.origin+'/api/auth/callback'])
        with self.assertRaises(policy.AuthError): auth.finish('code',query['state'][0],'wrong-browser')
        with auth.store.connect() as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM oauth').fetchone()[0],1)
            db.execute('UPDATE oauth SET expires=?',(time.time()-1,))
        with self.assertRaises(policy.AuthError): auth.finish('code',query['state'][0],browser)
        with auth.store.connect() as db: self.assertEqual(db.execute('SELECT COUNT(*) FROM oauth').fetchone()[0],0)
        self.assertEqual(auth.store.path.stat().st_mode & 0o777,0o600)
    def test_schema_errors_are_bounded_client_errors(self):
        for field in ('effect','priority'):
            document=policy.template_document('team-isolation',{})['document'];document['rules'][0][field]={}
            self.assertEqual(self.request('POST','/v1/policy/validate',{'document':document})[0],400)

if __name__=='__main__': unittest.main()
