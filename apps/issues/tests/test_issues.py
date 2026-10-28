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

    def test_note_status_is_memory_not_work(self):
        status,result=self.req('v1/issues','POST',{'title':'Decision record','status':'note'});self.assertEqual(status,201,result)
        note=result['issue']['id'];self.assertEqual(result['issue']['status'],'note')
        task=self.create();parent=self.create()
        self.assertEqual(self.req(f'v1/issues/{task}','PATCH',{'parent_id':parent})[0],200)
        self.assertEqual(self.req(f'v1/issues/{note}','PATCH',{'parent_id':parent})[0],200)
        progress=self.req(f'v1/issues/{parent}')[1]['issue']['progress']
        self.assertEqual((progress['total'],progress['completed']),(1,0),'a note does not count in the rollup')
        self.assertEqual(progress['by_status']['note'],0)
        listed=lambda q:[i['id'] for i in self.req('v1/issues?'+q)[1]['issues']]
        self.assertIn(note,listed('status=note'));self.assertNotIn(note,listed('unresolved=1'));self.assertIn(task,listed('unresolved=1'))
        blocked=self.create(blocked_by=[note])
        self.assertEqual(self.req(f'v1/issues/{blocked}','PATCH',{'status':'in_progress'})[0],200,'a note is never an unresolved dependency')
        for to in ('open','note','in_progress','note','closed','note'):          # any status <-> note, no extra rules
            self.assertEqual(self.req(f'v1/issues/{task}','PATCH',{'status':to})[0],200,to)
        changes=[e['changes'] for e in self.req(f'v1/issues/{task}')[1]['activity'] if e['kind']=='update']
        self.assertIn({'status':'note'},changes,'every conversion is an activity entry (who, when)')
    def test_status_filter_and_cli_accept_note(self):
        id=self.create(status='note');self.assertEqual(self.req(f'v1/issues/{id}')[1]['issue']['status'],'note')
        self.assertEqual([i['id'] for i in self.req('v1/issues?status=note')[1]['issues'] if i['id']==id],[id])
    def test_search_route_validation_and_attribution(self):
        id=self.req('v1/issues','POST',{'title':'Searchable pelican report'})[1]['issue']['id'];self.req(f'v1/issues/{id}/comment','POST',{'body':'the pelican landed','title':'[decision] pelican'},'agent-b')
        status,result=self.req('v1/search?q=pelican');self.assertEqual(status,200,result)
        hit=next(r for r in result['results'] if r['issue']['id']==id);self.assertIn(hit['match']['author'],('ag_a','ag_b'));self.assertTrue(hit['snippet_untrusted'])
        self.assertEqual(self.req('v1/search?q=pelican&author=ag_b')[1]['results'][0]['match']['author'],'ag_b')
        for query in ('','q=','q=x&limit=99','q=x&from=never','q=x&bogus=1','q=x&status=bogus'):
            self.assertEqual(self.req('v1/search'+('?'+query if query else ''))[0],400,query)
        self.assertEqual(self.req('v1/search?q=pelican',token=None)[0],401)
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
        status,result=self.req(f'v1/issues/{id}','PATCH',{'title':'Edited by another Agent'},'agent-b')
        self.assertEqual(status,200,'any workspace principal may edit any issue')
        self.assertEqual(next(e for e in result['activity'] if e['kind']=='update')['actor'],'ag_b','the change is attributed in activity')
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'status':'closed'},'human')[0],200,'a plain human member too')
        self.assertEqual(self.req('v1/issues','POST',{'title':'Forged','creator':'ag_b'})[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}/comment','POST',{'body':'Collaborative'},'agent-b')[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'assignee':'made-up'})[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'title':'Admin edit'},'admin')[0],200)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'title':'Stale','version':1})[0],409)
        # Forged identity headers never override the verified principal.
        _,result=self.req('v1/issues','POST',{'title':'Verified creator'},extra={'X-Treer-Agent-Id':'ag_b'})
        self.assertEqual(result['issue']['creator'],'ag_a')
    def prompts(self):
        return self.req('test/prompts',base=self.proxy)[1]['prompts']
    def wake_of(self,issue,target):
        events=self.req(f'v1/issues/{issue}')[1]['activity']
        return [e['notification'].get('wake',{}).get(target) for e in events if e['notification'].get('wake',{}).get(target)]
    def test_assignment_and_mentions_message_and_wake_agents(self):
        self.req('test/enable',base=self.proxy)
        try:
            before=len(self.prompts())
            status,result=self.req('v1/issues','POST',{'title':'Wake test','assignee':'ag_b','description':'SECRET DESCRIPTION'})
            self.assertEqual(status,201,result);id=result['issue']['id']
            self.assertEqual(result['notification']['wake'],{'ag_b':'prompted'})
            prompt=self.prompts()[before]
            self.assertEqual(prompt[:3],['agent','prompt','ag_b'])
            self.assertIn(f'Issues #{id}: assigned to you by Builder.',prompt[3]);self.assertIn('Message msg_',prompt[3])
            self.assertIn(f'issues show {id} --titles',prompt[3]);self.assertNotIn('SECRET',prompt[3]);self.assertNotIn('Wake test',prompt[3],'no author text in a prompt')
            self.assertEqual(self.wake_of(id,'ag_b'),['prompted'],'the outcome is recorded on the activity entry')
            # a second event about the same issue inside the window is deduplicated; the Message still goes out
            status,result=self.req(f'v1/issues/{id}/comment','POST',{'body':'more for @Builder and @/engineering/review/Reviewer'},'admin')
            self.assertEqual(status,200,result);self.assertEqual(result['notification']['status'],'sent')
            self.assertEqual(result['notification']['wake'],{'ag_a':'prompted','ag_b':'deduped'})
            # humans get a Message, never a prompt
            count=len(self.prompts())
            status,result=self.req(f'v1/issues/{id}','PATCH',{'assignee':'usr_a'});self.assertEqual(status,200)
            self.assertNotIn('wake',result['notification']);self.assertEqual(len(self.prompts()),count)
            self.assertIn('usr_a',self.req('test/messages',base=self.proxy)[1]['messages'][-1]['recipients'])
            # the actor is never woken by their own action
            other=self.create(assignee='ag_a');self.assertNotIn('wake',self.req(f'v1/issues/{other}')[1]['activity'][0]['notification'])
            # a Policy denial is recorded and the Message stands
            self.req('test/prompt-answer?code=policy_denied',base=self.proxy)
            status,result=self.req('v1/issues','POST',{'title':'Denied wake','assignee':'ag_c'})
            self.assertEqual((result['notification']['status'],result['notification']['wake']),('sent',{'ag_c':'denied'}))
            # a transient failure does not hold the dedupe slot: the next event tries again
            self.req('test/prompt-answer?code=agent_unreachable',base=self.proxy)
            denied=result['issue']['id'];third=self.create(assignee='ag_no_path')
            self.assertEqual(self.req(f'v1/issues/{third}')[1]['activity'][0]['notification']['wake'],{'ag_no_path':'failed:agent_unreachable'})
            self.req('test/prompt-answer',base=self.proxy)
            status,result=self.req(f'v1/issues/{third}/comment','POST',{'body':'retry @ag_no_path'})
            self.assertEqual(result['notification']['wake'],{'ag_no_path':'prompted'})
        finally:
            self.req('test/prompt-answer',base=self.proxy);self.req('test/disable',base=self.proxy)
    def test_no_wake_without_an_accepted_message(self):
        self.req('test/disable',base=self.proxy)
        count=len(self.prompts())
        status,result=self.req('v1/issues','POST',{'title':'Messages off','assignee':'ag_b'})
        self.assertEqual(result['notification']['status'],'disabled');self.assertNotIn('wake',result['notification'])
        self.assertEqual(len(self.prompts()),count,'the App never prompts someone the actor could not message')
    def test_hide_and_restore_issues_and_comments(self):
        id=self.create(labels=['hide-test']);self.req(f'v1/issues/{id}/comment','POST',{'title':'Visible','body':'keep me'})
        status,result=self.req(f'v1/issues/{id}/comment','POST',{'title':'Oops','body':'zebracorn secret'},'agent-b');comment=result['activity'][0]['id']
        self.assertEqual(self.req('v1/search?q=zebracorn')[1]['count'],1)
        status,result=self.req(f'v1/issues/{id}/activity/{comment}','DELETE',token='human');self.assertEqual(status,200,result)
        listed=self.req(f'v1/issues/{id}')[1]['activity']
        stub=next(e for e in listed if e['id']==comment);self.assertEqual(stub['hidden']['by'],'usr_a');self.assertNotIn('body',stub);self.assertNotIn('title',stub)
        self.assertEqual(next(e for e in listed if e['kind']=='hide')['changes'],{'comment':comment},'hiding is itself history')
        full=next(e for e in self.req(f'v1/issues/{id}?hidden=1')[1]['activity'] if e['id']==comment);self.assertEqual(full['body'],'zebracorn secret')
        self.assertEqual(self.req(f'v1/issues/{id}/activity/{comment}')[1]['event']['body'],'zebracorn secret','nothing is deleted')
        self.assertEqual(self.req('v1/search?q=zebracorn')[1]['count'],0,'hidden comments are not searched')
        self.assertEqual(self.req(f'v1/issues/{id}/activity/{comment}/restore','POST')[0],200)
        self.assertEqual(next(e for e in self.req(f'v1/issues/{id}')[1]['activity'] if e['id']==comment)['body'],'zebracorn secret')
        created=self.req(f'v1/issues/{id}?events=all')[1]['activity'][-1]['id']
        self.assertEqual(self.req(f'v1/issues/{id}/activity/{created}','DELETE')[0],409,'only comments can be hidden')
        comments=self.req(f'v1/issues/{id}?events=comments')[1]['activity'];self.assertTrue(comments and all(e['kind']=='comment' for e in comments))
        self.assertEqual(self.req(f'v1/issues/{id}?events=bogus')[0],400)
        # issues
        ids=lambda q:[i['id'] for i in self.req('v1/issues?label=hide-test'+q)[1]['issues']]
        self.assertEqual(self.req(f'v1/issues/{id}','DELETE',token='agent-b')[0],200,'any principal may hide')
        self.assertEqual((ids(''),ids('&hidden=1'),ids('&hidden=only')),([],[id],[id]))
        self.assertEqual(self.req(f'v1/issues/{id}')[1]['issue']['hidden']['by'],'ag_b','a hidden issue is still readable')
        self.assertEqual(self.req('v1/search?q=keep')[1]['count'],0)
        self.assertEqual(self.req(f'v1/issues/{id}/restore','POST')[0],200);self.assertEqual(ids(''),[id])
        self.assertIsNone(self.req(f'v1/issues/{id}')[1]['issue']['hidden'])
        kinds=[e['kind'] for e in self.req(f'v1/issues/{id}')[1]['activity']][:2];self.assertEqual(kinds,['restore','hide'])
        self.assertEqual(self.req('v1/issues?hidden=maybe')[0],400)
        self.assertEqual(self.req(f'v1/issues/{id}/activity/999999','DELETE')[0],404)
        parent=self.create();child=self.create(parent_id=parent);self.req(f'v1/issues/{child}','DELETE')
        shown=self.req(f'v1/issues/{parent}')[1];self.assertEqual((shown['children_total'],shown['issue']['progress']['total']),(0,0),'hidden children leave rollups')
    def test_retired_routes_and_fields_are_gone(self):
        id=self.create()
        for path,method,body in (('v1/stale','GET',None),('v1/notify-policy','GET',None),(f'v1/issues/{id}/claim','POST',{})):
            self.assertEqual(self.req(path,method,body)[0],404,path)
        self.assertEqual(self.req(f'v1/issues/{id}','PATCH',{'status':'blocked','waiting_on':'ag_b'})[0],400)
    def test_hierarchy_and_dependencies(self):
        parent=self.create();child=self.create(parent_id=parent);leaf=self.create(parent_id=child,status='done')
        progress=self.req(f'v1/issues/{parent}')[1]['issue']['progress']
        self.assertEqual((progress['total'],progress['completed']),(2,1))
        self.assertEqual(self.req(f'v1/issues/{parent}','PATCH',{'parent_id':leaf})[0],400)
        dependent=self.create(blocked_by=[child])
        self.assertEqual(self.req(f'v1/issues/{dependent}','PATCH',{'status':'done'})[0],409)
        self.assertEqual(self.req(f'v1/issues/{child}','PATCH',{'blocked_by':[dependent]})[0],400)
        self.req(f'v1/issues/{child}','PATCH',{'status':'done'})
        self.assertEqual(self.req(f'v1/issues/{dependent}','PATCH',{'status':'in_progress'})[0],200)
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
        self.assertEqual(message['recipients'],['ag_b']);self.assertIn(f'issues show {id} --titles',message['body']);self.assertNotIn('http',message['body'],'Agents get a CLI reference, not a URL')
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
            def do_DELETE(self):self.forward()
            def forward(self):
                path=urllib.parse.urlsplit(self.path).path
                query=urllib.parse.urlsplit(self.path).query
                body=self.rfile.read(int(self.headers.get('Content-Length',0)))
                request=urllib.request.Request(target+path.lstrip('/')+('?' + query if query else ''),data=body if body else None,method=self.command,headers={'X-Treer-Identity-Token':'agent-a','Content-Type':'application/json'})
                try:
                    with urllib.request.urlopen(request) as r: content=r.read();self.send_response(r.status);self.end_headers();self.wfile.write(content)
                except urllib.error.HTTPError as error:
                    content=error.read();self.send_response(error.code);self.end_headers();self.wfile.write(content)       # relay errors like a proxy
        proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),Inject);threading.Thread(target=proxy.serve_forever,daemon=True).start()
        try:
            env=hermetic_env({'http_proxy':f'http://127.0.0.1:{proxy.server_port}'})
            for command in (['list','--label',label],['list','--assignee','me','--unresolved','--label',label],['show',str(id)],['children',str(id)],['comment',str(id),'CLI comment'],['assign',str(id),'ag_a'],['status',str(id),'in_progress'],['create','CLI issue','--label','cli'],['create','CLI note','--status','note'],['show',str(id),'--comments-only','--hidden'],['hide',str(id)],['list','--hidden','only'],['restore',str(id)],['status',str(id),'blocked'],['search','pelican','--limit','3']):
                r=subprocess.run([sys.executable,str(ROOT/'issues'),*command],env=env,capture_output=True,text=True,timeout=10)
                self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertIsInstance(json.loads(r.stdout),dict)
            cli=lambda *a,stdin=None:subprocess.run([sys.executable,str(ROOT/'issues'),*a],env=env,input=stdin,capture_output=True,text=True,timeout=10)
            dep=self.create();epic=self.create()
            r=cli('edit',str(id),'--title','Edited by CLI','--priority','high','--label','x','--label','y','--blocked-by',str(dep),'--parent',str(epic))
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            issue=self.req(f'v1/issues/{id}')[1]['issue']
            self.assertEqual((issue['title'],issue['priority'],issue['labels'],issue['blocked_by'],issue['parent_ids']),('Edited by CLI','high',['x','y'],[dep],[epic]))
            untouched=(issue['status'],issue['assignee'],issue['description'])
            r=cli('edit',str(id),'--description-stdin',stdin='Line one\nLine two with "quotes" and \u4e2d\u6587')
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            after=self.req(f'v1/issues/{id}')[1]['issue']
            self.assertEqual(after['description'],'Line one\nLine two with "quotes" and \u4e2d\u6587')
            self.assertEqual((after['title'],after['priority'],after['labels'],after['blocked_by'],after['parent_ids']),('Edited by CLI','high',['x','y'],[dep],[epic]),'only the options given are sent')
            r=cli('edit',str(id),'--clear-labels','--clear-blocked-by','--clear-parents')
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            cleared=self.req(f'v1/issues/{id}')[1]['issue'];self.assertEqual((cleared['labels'],cleared['blocked_by'],cleared['parent_ids']),([],[],[]))
            for bad in (['edit',str(id)],['edit',str(id),'--label','a','--clear-labels'],['edit',str(id),'--description','x','--description-stdin'],['edit',str(id),'--version','1'],['edit','abc','--title','x']):
                self.assertNotEqual(cli(*bad).returncode,0,bad)
            self.assertNotEqual(cli('edit',str(id),'--title','stale write','--version','1').returncode,0,'optimistic concurrency conflict (409) is reported')
            self.assertEqual(self.req(f'v1/issues/{id}')[1]['issue']['title'],'Edited by CLI')
            r=cli('edit',str(id),'--title','x'*400);self.assertNotEqual(r.returncode,0);self.assertIn('invalid_field',r.stdout)
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
