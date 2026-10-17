import concurrent.futures
import json
import os
import subprocess
import sys
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from hermetic import hermetic_env

ROOT = Path(__file__).resolve().parents[1]
# In-process imports, CLI subprocesses and fixture all receive a sealed environment.
clean = hermetic_env(); os.environ.clear(); os.environ.update(clean)
sys.path.insert(0,str(ROOT))
from store import Store
from auth import Error
import tempfile


class IssuesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.process = subprocess.Popen([sys.executable,str(ROOT/'tests/fixture.py')],env=hermetic_env(),
                                       stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        config=json.loads(cls.process.stdout.readline());cls.base=config['url'];cls.proxy=config['proxy']
    @classmethod
    def tearDownClass(cls):
        cls.process.terminate();cls.process.communicate(timeout=5)
    def req(self,path,method='GET',body=None,token='agent-a',extra=None,base=None):
        headers={'Content-Type':'application/json',**({'X-Treer-Identity-Token':token} if token else {}),**({'X-Issues-Request':'1'} if token in ('human','admin') else {}),**(extra or {})}
        r=urllib.request.Request((base or self.base)+path,method=method,headers=headers,data=json.dumps(body).encode() if body is not None else None)
        try:
            with urllib.request.urlopen(r,timeout=10) as response:return response.status,json.load(response)
        except urllib.error.HTTPError as exc:return exc.code,json.load(exc)
    def create(self,**values):
        status,result=self.req('v1/issues','POST',dict(title='Test issue',**values));self.assertEqual(status,201,result);return result['issue']['id']
    def test_comment_titles_titles_only_reads_and_single_activity(self):
        id=self.create()
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'body':'Plain comment'})[0],200)
        status,result=self.req(f'v1/issues/{id}/comment','POST',{'title':'Decision: use SQLite','body':'Long reasoning\nline two'});self.assertEqual(status,200,result)
        full=self.req(f'v1/issues/{id}')[1]['activity']
        titled=next(e for e in full if e.get('title'));self.assertEqual(titled['body'],'Long reasoning\nline two')
        short=self.req(f'v1/issues/{id}?comments=titles')[1]['activity']
        short_titled=next(e for e in short if e.get('title'));plain=next(e for e in short if e['kind']=='comment' and not e.get('title'))
        self.assertNotIn('body',short_titled);self.assertEqual(short_titled['title'],'Decision: use SQLite');self.assertEqual(short_titled['body_bytes'],len(titled['body'].encode()))
        self.assertNotIn('body',plain);self.assertEqual(plain['body_bytes'],len('Plain comment'))
        self.assertEqual(plain['excerpt'],'Plain comment');self.assertNotIn('excerpt',short_titled,'a title replaces the excerpt')
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'body':'  line one\n\n\tline   two '+'y'*400})[0],200)
        long_one=next(e for e in self.req(f'v1/issues/{id}?comments=titles')[1]['activity'] if e.get('excerpt','').startswith('line one'))
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'body':'a\x00b\x1b[0m\u2028c'})[0],200)
        ctl=next(e for e in self.req(f'v1/issues/{id}?comments=titles')[1]['activity'] if e.get('excerpt','').startswith('a b'))
        self.assertEqual(ctl['excerpt'],'a b [0m c','control characters and line separators become single spaces')
        self.assertTrue(long_one['excerpt'].startswith('line one line two yyy'));self.assertLessEqual(len(long_one['excerpt']),160);self.assertTrue(long_one['excerpt'].endswith('\u2026'));self.assertNotIn('\n',long_one['excerpt'])
        self.assertTrue(all('body' not in e for e in short),'no comment text in a titles-only read')
        status,one=self.req(f"v1/issues/{id}/activity/{titled['id']}");self.assertEqual(status,200);self.assertEqual(one['event']['body'],titled['body'])
        other=self.create()
        self.assertEqual(self.req(f"v1/issues/{other}/activity/{titled['id']}")[0],404,'an activity id from another issue is not readable here')
        self.assertEqual(self.req(f'v1/issues/{id}/activity/999999')[0],404)
        self.assertEqual(self.req(f'v1/issues/{id}?comments=bogus')[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}/children?comments=titles')[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}/activity/1','POST',{})[0],404)
    def test_comment_titles_are_validated(self):
        id=self.create()
        for title in ('', ' padded ', 'two\nlines', 'x'*201, 5, 'nul\x00byte', 'esc\x1b[31m', 'tab\there', 'sep\u2028x', 'para\u2029x', 'del\x7fx'):
            self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'title':title,'body':'b'})[0],400,repr(title))
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'title':'t'})[0],400,'a title never replaces the text')
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'title':'标题'*30,'body':'b'})[0],200,'200 bytes, not characters')

    def test_directory_reports_paths_without_guessing_missing_ones(self):
        status,result=self.req('v1/directory');self.assertEqual(status,200)
        self.assertEqual(result['paths'],'supported');self.assertEqual(result['capabilities'],['app.directory-paths.v1'])
        paths={p['id']:p.get('path') for p in result['principals'] if p['kind']=='agent'}
        self.assertEqual(paths['ag_a'],'/engineering/backend/Builder');self.assertIsNone(paths['ag_no_path'])
    def test_directory_from_a_proxy_without_the_capability_is_marked_unsupported(self):
        from auth import Auth, Directory
        auth=Auth.__new__(Auth);auth.service_id='svc_issues'
        auth.request=lambda method,path,token,body=None:{'principals':[{'kind':'agent','id':'ag_a','name':'A'}]}
        old=auth.directory('t');self.assertIsInstance(old,Directory);self.assertEqual(old.capabilities,frozenset())
        auth.request=lambda method,path,token,body=None:{'principals':[],'capabilities':['app.directory-paths.v1',7]}
        self.assertEqual(auth.directory('t').capabilities,frozenset({'app.directory-paths.v1'}))
    def test_identity_and_permissions(self):
        for token in (None,'forged','wrong-audience','wrong-workspace','removed-agent'):
            status,_=self.req('v1/issues',token=token);self.assertIn(status,(401,403))
        id=self.create()
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'title':'Attack'},'agent-b')[0],403)
        self.assertEqual(self.req('v1/issues','POST',{'title':'Forged','creator':'ag_b'})[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'body':'Collaborative'},'agent-b')[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'assignee':'made-up'})[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'title':'Admin edit'},'admin')[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'title':'Stale','version':1})[0],409)
        # Forged identity headers never override the verified principal.
        _,result=self.req('v1/issues','POST',{'title':'Verified creator'},extra={'X-Treer-Agent-Id':'ag_b'})
        self.assertEqual(result['issue']['creator'],'ag_a')
    def test_claim_concurrency_expiry_and_ownership(self):
        id=self.create()
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            results=list(pool.map(lambda token:self.req(f'v1/issues/{id}/claim','POST',{'seconds':1},token), ['agent-a','agent-b']))
        self.assertEqual(sorted(r[0] for r in results),[200,409])
        holder=next(r[1]['issue']['claim']['holder'] for r in results if r[0]==200)
        token='agent-a' if holder=='ag_a' else 'agent-b';other='agent-b' if token=='agent-a' else 'agent-a'
        self.assertEqual(self.req(f'v1/issues/{id}/claim','POST',{'action':'renew','seconds':1},token)[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'status':'in_progress'},other)[0],409)
        self.assertEqual(self.req(f'v1/issues/{id}/claim','POST',{'action':'release'},other)[0],409)
        time.sleep(1.1)
        self.assertIsNone(self.req(f'v1/issues/{id}')[1]['issue']['claim'])
        self.assertEqual(self.req(f'v1/issues/{id}/claim','POST',{},other)[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'status':'in_progress'},other)[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}/claim','POST',{'action':'release'},other)[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}/claim','POST',{'action':'renew'},other)[0],409)
        assigned=self.create(assignee='ag_a')
        self.assertEqual(self.req(f'v1/issues/{assigned}/claim','POST',{},'agent-b')[0],409)
    def test_hierarchy_and_dependencies(self):
        parent=self.create();child=self.create(parent_id=parent);leaf=self.create(parent_id=child,status='done')
        progress=self.req(f'v1/issues/{parent}')[1]['issue']['progress']
        self.assertEqual((progress['total'],progress['completed']),(2,1))
        self.assertEqual(self.req(f'v1/issues/{parent}','PATCH',{'parent_id':leaf})[0],400)
        dependent=self.create(blocked_by=[child])
        self.assertEqual(self.req(f'v1/issues/{dependent}/claim','POST',{})[0],409)
        self.assertEqual(self.req(f'v1/issues/{dependent}','PATCH',{'status':'done'})[0],409)
        self.assertEqual(self.req(f'v1/issues/{child}','PATCH',{'blocked_by':[dependent]})[0],400)
        self.req(f'v1/issues/{child}','PATCH',{'status':'done'})
        self.assertEqual(self.req(f'v1/issues/{dependent}/claim','POST',{})[0],200)
        self.assertEqual(self.req(f'v1/issues/{parent}/children')[1]['issues'][0]['id'],child)
    def test_notification_degradation_and_summary(self):
        self.req('test/disable',base=self.proxy)
        id=self.create(assignee='ag_b',description='SECRET FULL BODY')
        _,result=self.req(f'v1/issues/{id}/comment','POST',{'body':'SECRET COMMENT'})
        self.assertEqual(result['notification']['status'],'disabled')
        self.assertEqual(result['activity'][0]['notification']['status'],'disabled')
        self.req('test/enable',base=self.proxy)
        _,result=self.req(f'v1/issues/{id}','PATCH',{'status':'in_progress'})
        self.assertEqual(result['notification']['status'],'sent')
        message=self.req('test/messages',base=self.proxy)[1]['messages'][-1]
        self.assertEqual(message['recipients'],['ag_b']);self.assertIn(f'#issue={id}',message['body'])
        self.assertNotIn('SECRET',message['body']);self.assertTrue(message['idempotency_key'].startswith('issues-event-'))
        self.req('test/disable',base=self.proxy)
    def test_needs_me_view_filters(self):
        label='needs-me-label'
        mine=self.create(assignee='ag_a',labels=[label]);self.create(assignee='ag_a',labels=[label],status='done')
        self.create(assignee='ag_a',labels=[label],status='closed');theirs=self.create(assignee='ag_b',labels=[label]);self.create(labels=[label])
        ids=lambda path,token='agent-a':[i['id'] for i in self.req(path,token=token)[1]['issues']]
        self.assertEqual(ids(f'v1/issues?assignee=me&unresolved=1&label={label}'),[mine],'me is the caller; done/closed are resolved')
        self.assertEqual(ids(f'v1/issues?assignee=me&unresolved=1&label={label}','agent-b'),[theirs])
        self.assertEqual(self.req(f'v1/issues?assignee=me&label={label}')[1]['total'],3,'without unresolved every status is listed')
        self.assertEqual(self.req(f'v1/issues?assignee=me&unresolved=0&label={label}')[1]['total'],3)
        self.assertEqual(self.req('v1/issues?unresolved=maybe')[0],400)
        self.assertEqual(self.req(f'v1/issues?assignee=me&unresolved=1&label={label}',token='human')[1]['total'],0,'a human with no assignments sees nothing')
    def test_validation_pagination_and_cli(self):
        for body in ({'title':'x','labels':'bad'},{'title':'x','status':'wat'},{'title':'x','parent_id':999999},{'title':'x','blocked_by':[True]}):
            self.assertIn(self.req('v1/issues','POST',body)[0],(400,404))
        self.assertEqual(self.req('v1/issues?limit=101')[0],400)
        self.assertEqual(self.req('v1/issues?evil=1')[0],400)
        label='unique-label';id=self.create(labels=[label])
        self.assertEqual(self.req('v1/issues?label='+label)[1]['total'],1)
        # Use a local test proxy that injects service identity, as Treer's managed entry does.
        import http.server,threading
        target=self.base
        class Inject(http.server.BaseHTTPRequestHandler):
            def log_message(self,*_):pass
            def do_GET(self): self.forward()
            def do_POST(self):self.forward()
            def do_PATCH(self):self.forward()
            def forward(self):
                path=urllib.parse.urlsplit(self.path).path
                query=urllib.parse.urlsplit(self.path).query
                body=self.rfile.read(int(self.headers.get('Content-Length',0)))
                request=urllib.request.Request(target+path.lstrip('/')+('?' + query if query else ''),data=body if body else None,method=self.command,headers={'X-Treer-Identity-Token':'agent-a','Content-Type':'application/json'})
                with urllib.request.urlopen(request) as r: content=r.read();self.send_response(r.status);self.end_headers();self.wfile.write(content)
        proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),Inject);threading.Thread(target=proxy.serve_forever,daemon=True).start()
        try:
            env=hermetic_env({'http_proxy':f'http://127.0.0.1:{proxy.server_port}'})
            for command in (['list','--label',label],['list','--assignee','me','--unresolved','--label',label],['show',str(id)],['children',str(id)],['comment',str(id),'CLI comment'],['assign',str(id),'ag_a'],['status',str(id),'in_progress'],['claim',str(id),'--seconds','10'],['claim',str(id),'--action','release'],['create','CLI issue','--label','cli']):
                r=subprocess.run([sys.executable,str(ROOT/'issues'),*command],env=env,capture_output=True,text=True,timeout=10)
                self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertIsInstance(json.loads(r.stdout),dict)
        finally:proxy.shutdown();proxy.server_close()
    def test_forwarded_human_identity_and_browser_mutation_security(self):
        token=self.req('test/human-token',base=self.proxy)[1]['access_token']
        expired=self.req('test/human-token?seconds=-1',base=self.proxy)[1]['access_token']
        other=self.req('test/human-token?audience=svc_other',base=self.proxy)[1]['access_token']
        self.assertEqual(self.req('v1/me',token=token,extra={'Authorization':'Bearer agent-b'})[1]['principal']['id'],'usr_a')
        import http.cookiejar
        jar=http.cookiejar.CookieJar();opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        opener.open(self.base+'api/auth/start').close()
        cookie='; '.join(f'{c.name}={c.value}' for c in jar)
        for invalid in ('forged',expired,other,''):
            status,_=self.req('v1/me',token=None,extra={'X-Treer-Identity-Token':invalid,'Cookie':cookie})
            self.assertEqual(status,401)
        # An injected human token is never treated as an Agent CSRF exemption.
        self.assertEqual(self.req('v1/issues','POST',{'title':'unsafe'},token=token)[0],403)
        for headers in ({'Origin':'https://evil.test','Sec-Fetch-Site':'same-site'},
                        {'Origin':self.base.rstrip('/'),'Sec-Fetch-Site':'cross-site'},
                        {'Origin':'null'}, {'Sec-Fetch-Site':'none'}):
            self.assertEqual(self.req('v1/issues','POST',{'title':'unsafe'},token=token,
                extra={'X-Issues-Request':'1',**headers})[0],403)
        for origin in (self.base.rstrip('/'),self.proxy.rstrip('/')):
            status,result=self.req('v1/issues','POST',{'title':'Human verified'},token=token,
                extra={'X-Issues-Request':'1','Origin':origin,'Sec-Fetch-Site':'same-origin'})
            self.assertEqual(status,201,result);self.assertEqual(result['issue']['creator'],'usr_a')

    def test_embedded_identity_discovery_without_wildcard_ingress(self):
        from unittest.mock import patch
        from types import SimpleNamespace
        from issues import Application
        with tempfile.TemporaryDirectory() as directory:
            app=Application(Store(Path(directory)/'issues.sqlite3'))
            with patch.dict(os.environ,{'TREER_AGENT_ID':'app_test','ISSUES_PROXY_URL':'https://proxy.test/'}), patch('issues.subprocess.run',return_value=SimpleNamespace(stdout=json.dumps({'apps':[{
                'runtime_agent_id':'app_test','app_id':'app_test','service_id':'svc_test','workspace_id':'ws_test','public_url':None}]}))):
                auth=app.identity_provider()
                self.assertEqual(auth.public_url,'https://proxy.test/api/workspaces/ws_test/apps/app_test/ui/')

    def test_representations_oauth_csrf(self):
        for headers,media in (({},'text/markdown'),({'Accept':'text/html'},'text/html'),({'User-Agent':'Mozilla/test'},'text/html'),({'Accept':'text/html;q=.1,text/markdown;q=.9'},'text/markdown')):
            with urllib.request.urlopen(urllib.request.Request(self.base,headers=headers)) as r:
                self.assertTrue(r.headers['Content-Type'].startswith(media));self.assertEqual(r.headers['Vary'],'Accept, User-Agent')
        with self.assertRaises(urllib.error.HTTPError) as denied:
            urllib.request.urlopen(self.base+'api/auth/start')
        self.assertEqual(denied.exception.code,401)
        import http.cookiejar
        jar=http.cookiejar.CookieJar();opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        opener.open(self.base+'api/auth/start').close()
        cookie='; '.join(f'{c.name}={c.value}' for c in jar)
        self.assertTrue(cookie)
        self.assertEqual(self.req('v1/me',token=None,extra={'Cookie':cookie})[1]['principal']['id'],'usr_a')
        self.assertEqual(self.req('v1/issues','POST',{'title':'CSRF'},token=None,extra={'Cookie':cookie})[0],403)
        self.assertEqual(self.req('v1/issues','POST',{'title':'Browser'},token=None,extra={'Cookie':cookie,'X-Issues-Request':'1'})[0],201)
        self.assertEqual(self.req('v1/issues','POST',{'title':'CSRF'},token=None,extra={'Cookie':cookie,'X-Issues-Request':'1','Origin':'https://evil.test'})[0],403)
        self.assertEqual(self.req('api/auth/callback?code=x&state=forged')[0],401)
        self.assertEqual(self.req('v1/missing')[0],404)
        self.assertEqual(self.req('missing')[0],404)


if __name__ == '__main__':unittest.main()
