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
        if self.path == '/redirect':
            self.send_response(302);self.send_header('Location','/trap');self.send_header('Content-Length','0');self.end_headers();return
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))) or b'{}') if self.headers.get('Content-Type') != 'application/x-www-form-urlencoded' else self.rfile.read(int(self.headers['Content-Length']))
        self.server.calls.append((self.command,self.path,self.headers.get('Authorization'),body))
        token = self.headers.get('Authorization','').removeprefix('Bearer ')
        status=200
        if self.path == '/.treer/apps/identity/verify':
            token=body['token'];valid=token in ('human-owner','human-member','expired','wrong-audience','revoked','agent-valid')
            result={'active':valid,'claims':{'service_id':'other' if token=='wrong-audience' else 'svc_policy',
                'workspace_id':'ws_test','principal_kind':'agent' if token=='agent-valid' else 'human','sub':'delegate' if token=='agent-valid' else 'owner' if token!='human-member' else 'member',
                'exp':time.time()+(-60 if token=='expired' else 60)}}
        elif self.path == '/api/apps/oauth/token':
            from datetime import datetime, timezone
            result={'access_token':'human-owner','expires_at':datetime.fromtimestamp(time.time()+300,timezone.utc).isoformat()}
        elif self.path.endswith('/directory'):
            result={'principals':[] if token=='revoked' else [{'id':'owner','kind':'human','name':'Owner'},{'id':'member','kind':'human','name':'Member'},{'id':'delegate','kind':'agent','name':'Delegate'}]}
        elif self.path.endswith('/policy/publish-authority') or '/policy/operator/' in self.path:
            runtime_ok = self.headers.get('X-Treer-Agent-Id')=='runtime_policy' and self.headers.get('X-Treer-Workload-Credential')=='synthetic-runtime-credential'
            fault = getattr(self.server, 'authority_fault', None)
            if not runtime_ok: status=403;result={'error':{'code':'policy_provider_required'}}
            elif fault == 'authority404' or fault == 'operator404' and '/operator/' in self.path:
                status=404;result={'error':{'code':'route_not_found'}}
            elif self.path.endswith('/publish-authority'):
                result={'publish_authority':getattr(self.server,'authority','owner_session'),
                    'capabilities':[] if fault=='missingcap' else ['policy.publish-authority.v1'], 'provider':{'app_id':'app_policy'}}
                if fault == 'flip': self.server.authority='owner_session'
            elif getattr(self.server,'authority','owner_session')!='provider_operator':
                status=403;result={'error':{'code':'provider_operator_required'}}
            elif self.path.endswith('/validate'):
                result={'valid':True,'schema_version':body['schema_version'],'recovery_guaranteed':getattr(self.server,'recovery',True),'capabilities':[] if fault=='operator-missingcap' else ['policy.path-rules.v2','policy.publish-authority.v1']}
            else:
                result={'explanation':{'computed_effect':'deny','effective_decision':'allow','mode':'monitor','matched_rule_id':'operator-rule','policy_revision':2},'capabilities':['policy.publish-authority.v1']}
        elif '/policy/' in self.path:
            token=self.headers.get('X-Treer-Identity-Token')
            if self.headers.get('X-Treer-Agent-Id')!='runtime_policy' or self.headers.get('X-Treer-Workload-Credential')!='synthetic-runtime-credential':
                status=403;result={'error':{'code':'policy_provider_required'}}
            elif token!='human-owner': status=403;result={'error':{'code':'workspace_owner_required'}}
            elif self.path.endswith('/validate'): result={'valid':True,'schema_version':body['schema_version'],'recovery_guaranteed':getattr(self.server,'recovery',True),'capabilities':['policy.path-rules.v2']}
            elif self.path.endswith('/explain'): result={'explanation':{'computed_effect':'deny','effective_decision':'allow','mode':'monitor','matched_rule_id':'test-rule','policy_revision':2}}
            else: result={'decisions':[{'action':'agent.prompt','explanation':{'computed_effect':'deny','effective_decision':'allow'}}], 'scope':'this_proxy'}
        else: status=404;result={'error':{'code':'not_found'}}
        encoded=json.dumps(result).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(encoded)));self.end_headers();self.wfile.write(encoded)
    def log_message(self,*args): pass

class IdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.environment=patch.dict(os.environ, {'PATH':os.defpath,'TREER_AGENT_ID':'runtime_policy','TREER_WORKLOAD_CREDENTIAL':'synthetic-runtime-credential'}, clear=True);self.environment.start()
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
    def test_operator_mode_requires_local_capability_and_verified_injected_identity(self):
        document=policy.template_document('team-isolation',{})['document']
        publish={'expected_revision':1,'mode':'monitor','document':document}
        self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity='agent-valid')[0],403)
        self.assertEqual(self.app.RequestHandlerClass.store.bundle()['revision'],1)
        self.proxy.authority='provider_operator'
        self.assertEqual(self.request('GET','/v1/policy/authority')[1]['publish_authority'],'provider_operator')
        for identity in ('forged','expired','wrong-audience',''):
            self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity=identity)[0],401)
        self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity='agent-valid',extra={'Authorization':'Bearer forged'})[0],401)
        self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity='agent-valid')[0],200)
        self.assertEqual(self.app.RequestHandlerClass.store.bundle()['revision'],2)
        simulation={'subject':{'kind':'agent','path':'/team/a'},'action':'agent.prompt','resource':{'kind':'agent','path':'/team/b'}}
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity=None)[0],200)
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity='human-member')[0],200)
        self.assertEqual(self.request('GET','/v1/policy/decisions',identity='agent-valid')[0],401)
        self.assertNotIn(ADMIN,json.dumps(self.proxy.calls))
        self.assertTrue(any('/operator/validate' in call[1] for call in self.proxy.calls))
        self.assertTrue(all(call[2] is None for call in self.proxy.calls if '/operator/' in call[1]))
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity=None,extra={'Origin':'https://evil.test'})[0],403)
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity=None,extra={'Sec-Fetch-Site':'cross-site'})[0],403)
        self.proxy.authority='owner_session'
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity='agent-valid')[0],401)

    def test_mixed_proxy_versions_and_mode_switch_fail_without_publication(self):
        document=policy.template_document('team-isolation',{})['document']
        publish={'expected_revision':1,'mode':'monitor','document':document}
        for fault,expected in [('authority404',409),('missingcap',409),('operator404',409),('operator-missingcap',409),('flip',403)]:
            self.proxy.authority='provider_operator';self.proxy.authority_fault=fault
            self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity=None)[0],expected,fault)
            self.assertEqual(self.app.RequestHandlerClass.store.bundle()['revision'],1)
        self.assertFalse(any(call[1].endswith('/policy/validate') for call in self.proxy.calls),'No legacy owner fallback')
        self.proxy.authority_fault=None;self.proxy.authority='owner_session'
        self.assertEqual(self.request('POST','/v1/policy/publish',publish,identity=None)[0],401)
        self.assertEqual(self.request('POST','/v1/policy/publish',publish)[0],200)

    def test_operator_client_against_app_and_file_capability_agent_path(self):
        import subprocess, sys
        self.proxy.authority='provider_operator'
        client_config=Path(self.temp.name).resolve()/'client.json'
        client_config.write_text(json.dumps({'url':self.origin,'token':ADMIN}));client_config.chmod(0o600)
        payload=Path(self.temp.name)/'publication.json'
        payload.write_text(json.dumps({'expected_revision':1,'mode':'monitor','document':policy.template_document('team-isolation',{})['document']}))
        client=Path(__file__).resolve().parents[1]/'treer-policy'
        for command in (['read'],['submit',str(payload)],['preview','-']):
            result=subprocess.run([sys.executable,str(client),'--config',str(client_config),*command],input='{}',capture_output=True,text=True,
                env={'PATH':os.defpath,'HOME':self.temp.name},timeout=5)
            self.assertEqual(result.returncode,0,result.stdout);self.assertEqual(result.stderr,'');self.assertNotIn(ADMIN,result.stdout)
        source=policy.AdminTokenFile(Path(self.temp.name).resolve()/'file-admin');source.initialize();self.app.admin_token_file=source
        simulation={'action':'agent.prompt'}
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity='agent-valid',extra={'Authorization':'Bearer '+source.read()})[0],200)
        source.path.unlink()
        self.assertEqual(self.request('POST','/v1/policy/explain',simulation,identity='agent-valid',extra={'Authorization':'Bearer '+ADMIN})[0],503)
        self.assertNotIn(ADMIN,json.dumps(self.proxy.calls))

    def test_embedded_operator_header_survives_core_consumed_bearer(self):
        payload={'expected_revision':1,'mode':'monitor','document':policy.template_document('team-isolation',{})['document']}
        status, _, _=self.request('POST','/v1/policy/publish',payload,extra={'Authorization':'','X-Policy-Admin-Token':ADMIN})
        self.assertEqual(status,200)
        for value in ('','forged'):
            self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'X-Policy-Admin-Token':value})[0],401,'Invalid custom capability cannot fall back to valid Bearer')
        self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'Authorization':'','X-Policy-Admin-Token':ADMIN,'Origin':'https://evil.test'})[0],403)
        self.assertNotIn(ADMIN,json.dumps(self.proxy.calls))

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
    def test_file_capability_rotation_loss_and_no_forwarding(self):
        source=policy.AdminTokenFile(Path(self.temp.name).resolve()/'admin-token')
        source.initialize();old=source.read();self.app.admin_token_file=source
        self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'Authorization':'Bearer '+old})[0],200)
        source.rotate();new=source.read()
        self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'Authorization':'Bearer '+old})[0],401)
        self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'Authorization':'Bearer '+new})[0],200)
        source.path.unlink()
        self.assertEqual(self.request('POST','/v1/policy/invalidate',{},extra={'Authorization':'Bearer '+new})[0],503)
        self.assertEqual(self.request('GET','/v1/session',extra={'Authorization':'Bearer '+old})[0],200)
        forwarded=json.dumps(self.proxy.calls)
        self.assertNotIn(old,forwarded);self.assertNotIn(new,forwarded)

    def test_authenticated_proxy_requests_never_follow_redirects(self):
        with self.assertRaises(policy.AuthError) as caught:
            self.app.auth.request('GET','/redirect','human-owner')
        self.assertEqual(caught.exception.status,502)
        self.assertFalse(any(call[1]=='/trap' for call in self.proxy.calls))
        with self.assertRaises(policy.AuthError): self.app.auth.diagnostic('unrestricted-route','human-owner')

    def test_diagnostics_need_both_bound_runtime_and_human(self):
        with patch.dict(os.environ, {'TREER_WORKLOAD_CREDENTIAL':''}):
            self.assertEqual(self.request('GET','/v1/policy/decisions')[0],503)
        with patch.dict(os.environ, {'TREER_WORKLOAD_CREDENTIAL':'forged'}):
            self.assertEqual(self.request('GET','/v1/policy/decisions')[0],403)
        self.assertEqual(self.request('GET','/v1/policy/decisions',extra={'Origin':'https://evil.test'})[0],403)
        self.assertEqual(self.request('GET','/v1/policy/decisions',extra={'Sec-Fetch-Site':'cross-site'})[0],403)

        self.assertEqual(self.request('GET','/v1/policy/decisions',extra={'X-Treer-Agent-Id':'forged','X-Treer-Workload-Credential':'forged'})[0],200)

    def test_unconfirmed_recovery_cannot_publish_or_restore_path_policy(self):
        document=policy.template_document('team-isolation',{})['document']
        self.proxy.recovery=False
        self.assertEqual(self.request('POST','/v1/policy/publish',{'expected_revision':1,'mode':'enforce','document':document})[0],409)
        self.assertEqual(self.app.RequestHandlerClass.store.bundle()['revision'],1)
        self.proxy.recovery=True
        self.assertEqual(self.request('POST','/v1/policy/publish',{'expected_revision':1,'mode':'monitor','document':document})[0],200)
        self.assertEqual(self.request('POST','/v1/policy/rollback',{'expected_revision':2,'revision':1})[0],200)
        self.proxy.recovery=False
        self.assertEqual(self.request('POST','/v1/policy/rollback',{'expected_revision':3,'revision':2})[0],409)
        self.assertEqual(self.app.RequestHandlerClass.store.bundle()['revision'],3)

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
        url,browser=auth.start();query=parse_qs(urlsplit(url).query)
        raw,age=auth.finish('synthetic-code',query['state'][0],browser)
        self.assertGreater(age,0);self.assertEqual(auth.session_token(raw),'human-owner')
        with self.assertRaises(policy.AuthError): auth.finish('synthetic-code',query['state'][0],browser)
        auth.logout(raw);self.assertIsNone(auth.session_token(raw))
        self.assertEqual(auth.store.path.stat().st_mode & 0o777,0o600)
    def test_schema_errors_are_bounded_client_errors(self):
        for field in ('effect','priority'):
            document=policy.template_document('team-isolation',{})['document'];document['rules'][0][field]={}
            self.assertEqual(self.request('POST','/v1/policy/validate',{'document':document})[0],400)

if __name__=='__main__': unittest.main()
