"""Local-only security regressions: fake identities, ephemeral files/listeners."""
import os
for key in list(os.environ):
    if key.startswith(('TREER_','AGENT_UI_','CODEX_UI_','MAIL_','ISSUES_','SOUL_','POLICY_','EMAIL_ENDPOINT_')) or key.lower() in ('http_proxy','https_proxy','all_proxy','no_proxy'):
        del os.environ[key]
import importlib.util
import http.client
import io
import json
import sqlite3
import sys
import tarfile
import tempfile
import threading
import unittest
import urllib.parse
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

APPS=Path(__file__).resolve().parents[2]
def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec);sys.modules[name]=module;spec.loader.exec_module(module)
    return module
mail=load('audit_mail',APPS/'mail/mail.py')
policy=load('audit_policy',APPS/'policy/policy.py')
issues_auth=load('audit_issues_auth',APPS/'issues/auth.py')
secrets=load('audit_soul_secrets',APPS/'soul/soul_secrets.py')
archive_bounds=load('audit_soul_archive',APPS/'soul/soul_archive.py')
sys.modules['auth']=issues_auth
issues_store=load('audit_issues_store',APPS/'issues/store.py')
tower=load('audit_tower',APPS/'tower/tower.py')
sys.path.insert(0,str(APPS/'telegram'))
from telegram_app.state import StateStore as TelegramStore

class FakeMailAPI:
    def __init__(self): self.exchanges=0;self.writes=0
    def request(self,method,path,access_token=None,body=None,form=None):
        if path.endswith('/oauth/token'):
            self.exchanges+=1
            return {'access_token':'synthetic-human','expires_at':'2099-01-01T00:00:00Z'}
        if path.endswith('/identity/verify'):
            return {'active':body['token']=='synthetic-human','claims':{'workspace_id':'ws_test','service_id':'svc_test','principal_kind':'human','sub':'u_test','name':'Synthetic','role':'member','exp':4070908800}}
        if path.endswith('/directory'): return {'principals':[{'kind':'human','id':'u_test','name':'Synthetic','role':'member'}]}
        self.writes+=1
        return {'message':{'message_id':'msg_test'}}

class Boundaries(unittest.TestCase):
    def mail_app(self,root):
        c=mail.Config('127.0.0.1',0,'svc_test','http://app.invalid/','http://proxy.invalid/',root,root/'mail.sqlite3')
        return mail.MailApplication(c,FakeMailAPI(),mail.SessionStore(c.database_path))

    def test_mail_callback_requires_same_browser(self):
        with tempfile.TemporaryDirectory() as tmp:
            app=self.mail_app(Path(tmp))
            url=app.start_oauth('/',browser='browser-owner')
            state=urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)['state'][0]
            for browser in ('','browser-victim'):
                with self.assertRaises(mail.MailError):app.finish_oauth('synthetic-code',state,browser=browser)
            self.assertEqual(app.api.exchanges,0)
            raw,_,_=app.finish_oauth('synthetic-code',state,browser='browser-owner')
            self.assertEqual(app.authenticated_session(raw).user_id,'u_test')
            with self.assertRaises(mail.MailError):app.finish_oauth('synthetic-code',state,browser='browser-owner')

    def test_policy_default_read_only_and_no_identity_header_bypass(self):
        with tempfile.TemporaryDirectory() as tmp:
            policy.PolicyHandler.store=policy.PolicyStore(Path(tmp)/'policy.json','ws_test')
            server=ThreadingHTTPServer(('127.0.0.1',0),policy.PolicyHandler)
            threading.Thread(target=server.serve_forever,daemon=True).start()
            body={'expected_revision':1,'mode':'enforce','document':{'schema_version':1,'defaults':{},'groups':{},'rules':[]}}
            try:
                conn=http.client.HTTPConnection(*server.server_address,timeout=5)
                conn.request('POST','/v1/policy/publish',json.dumps(body),{'Content-Type':'text/plain','X-Treer-Role':'owner'})
                response=conn.getresponse();response.read();conn.close()
                self.assertIn(response.status,(401,403,503))
                self.assertEqual(policy.PolicyHandler.store.bundle()['revision'],1)
            finally:server.shutdown();server.server_close()

    def test_identity_header_never_falls_back_to_mail_cookie_and_csrf(self):
        from dataclasses import replace
        with tempfile.TemporaryDirectory() as tmp:
            app=self.mail_app(Path(tmp));claims=app.api.request('POST','/.treer/apps/identity/verify',body={'token':'synthetic-human'})['claims']
            app.sessions.save_session('synthetic-cookie','synthetic-human',claims,4070908800)
            server=mail.MailServer(('127.0.0.1',0),app)
            app.config=replace(app.config,public_url=f'http://127.0.0.1:{server.server_address[1]}/')
            threading.Thread(target=server.serve_forever,daemon=True).start()
            def request(path,token=None,method='GET',extra=None):
                headers={'Cookie':mail.SESSION_COOKIE+'=synthetic-cookie',**(extra or {})}
                if token is not None:headers['X-Treer-Identity-Token']=token
                conn=http.client.HTTPConnection(*server.server_address,timeout=5)
                conn.request(method,path,'{}' if method=='POST' else None,headers)
                r=conn.getresponse();r.read();status=r.status;conn.close();return status
            try:
                for token in ('','forged-token'):
                    self.assertEqual(request('/api/auth/session',token),401)
                self.assertEqual(request('/api/auth/session','synthetic-human'),200)
                for origin,site in [('null','same-origin'),('https://evil.invalid','same-site'),(app.config.public_url.rstrip('/'),'cross-site')]:
                    self.assertEqual(request('/api/auth/logout','synthetic-human','POST',{'Origin':origin,'Sec-Fetch-Site':site}),403)
            finally:server.shutdown();server.server_close()

    def test_app_http_clients_do_not_forward_synthetic_bearer_on_redirect(self):
        leaked=[]
        class Target(BaseHTTPRequestHandler):
            def do_GET(self):
                leaked.append(self.headers.get('Authorization'));self.send_response(200);self.end_headers();self.wfile.write(b'{}')
            def log_message(self,*_):pass
        target=ThreadingHTTPServer(('127.0.0.1',0),Target)
        class Redirect(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(302);self.send_header('Location',f'http://127.0.0.1:{target.server_address[1]}/sink');self.end_headers()
            def log_message(self,*_):pass
        source=ThreadingHTTPServer(('127.0.0.1',0),Redirect)
        for server in (source,target):threading.Thread(target=server.serve_forever,daemon=True).start()
        class Store:pass
        try:
            url=f'http://127.0.0.1:{source.server_address[1]}/'
            clients=[(mail.TreerAppClient(url,'svc_test'),mail.AppApiError),(issues_auth.Auth(Store(),url,'svc_test','ws_test','http://app.invalid/'),issues_auth.Error)]
            for client,error in clients:
                with self.assertRaises(error):
                    if isinstance(client,mail.TreerAppClient):client.request('GET','/resource',access_token='synthetic-bearer')
                    else:client.request('GET','/resource',token='synthetic-bearer')
            self.assertEqual(leaked,[])
        finally:
            for server in (source,target):server.shutdown();server.server_close()

    def test_secret_scanning_is_not_an_encryption_or_encoding_boundary(self):
        fake=b'sk-syntheticToken12345'
        import base64
        self.assertTrue(secrets.scan_secrets(fake))
        self.assertEqual(secrets.scan_secrets(base64.b64encode(fake)),{})
        self.assertEqual(secrets.scan_secrets(json.dumps({'token':'sk-'+'\\u0073yntheticToken12345'}).encode()),{})

    def test_issues_oauth_browser_state_and_legacy_migration(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'issues.db'
            with sqlite3.connect(path) as db:
                db.execute('CREATE TABLE oauth(state TEXT PRIMARY KEY,verifier TEXT,expires REAL)')
                db.execute("INSERT INTO oauth VALUES('old','verifier',4070908800)")
            store=issues_store.Store(path)
            auth=issues_auth.Auth(store,'http://proxy.invalid/','svc_test','ws_test','http://app.invalid/')
            url,browser=auth.start()
            state=urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)['state'][0]
            for wrong in ('','wrong-browser'):
                with self.assertRaises(issues_auth.Error):auth.finish('code',state,wrong)
            with store.connect() as db:
                self.assertEqual(db.execute('SELECT count(*) FROM oauth').fetchone()[0],1)
            exchanged=[]
            def request(*args,**kwargs):
                exchanged.append(True)
                return {'access_token':'synthetic','expires_at':'2099-01-01T00:00:00Z'}
            auth.request=request
            auth.verify=lambda token: ({'kind':'human'},[])
            auth.finish('code',state,browser)
            with self.assertRaises(issues_auth.Error):auth.finish('code',state,browser)
            self.assertEqual(len(exchanged),1)

    def test_hidden_tar_metadata_is_in_expansion_budget(self):
        import gzip,bz2,lzma
        stream=io.BytesIO()
        with tarfile.open(fileobj=stream,mode='w',format=tarfile.PAX_FORMAT) as archive:
            member=tarfile.TarInfo('manifest.json')
            member.pax_headers={'comment':'x'*32768}
            member.size=2
            archive.addfile(member,io.BytesIO(b'{}'))
        raw=stream.getvalue()
        for encode,mode in [(lambda x:x,'r:'),(gzip.compress,'r:gz'),(bz2.compress,'r:bz2'),(lzma.compress,'r:xz')]:
            compressed=encode(raw)
            with self.assertRaises(ValueError):archive_bounds.archive_mode(compressed,1024)
            self.assertEqual(archive_bounds.archive_mode(compressed,len(raw)),mode)

    def test_state_is_private_even_with_permissive_umask(self):
        constructors=[mail.SessionStore,issues_store.Store,tower.TowerStore,TelegramStore]
        with tempfile.TemporaryDirectory() as tmp:
            previous=os.umask(0o022)
            try:
                for index,constructor in enumerate(constructors):
                    path=Path(tmp)/str(index)/'state.sqlite3'
                    path.parent.mkdir(mode=0o755)
                    store=constructor(path)
                    self.assertEqual(path.parent.stat().st_mode & 0o777,0o700)
                    self.assertEqual(path.stat().st_mode & 0o777,0o600)
                    for sidecar in path.parent.glob('state.sqlite3-*'):
                        self.assertEqual(sidecar.stat().st_mode & 0o077,0)
                    if hasattr(store,'connection'):store.connection.close()
            finally:os.umask(previous)

if __name__=='__main__':unittest.main()

