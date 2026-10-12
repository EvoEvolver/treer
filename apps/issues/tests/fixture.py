"""Hermetic real HTTP App plus Treer contract double. Test-only, never runtime."""
import json
import os
import sys
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from auth import Auth
from store import Store
from issues import Application, Server
from gateway import PREFIX, Ingress, ControlPlane, forward, issue_human_token, verify_human_token, FORWARDED

PEOPLE = [{'kind':'agent','id':'ag_a','name':'Builder'}, {'kind':'agent','id':'ag_b','name':'Reviewer'},
          {'kind':'human','id':'usr_a','name':'Alice','role':'member'},
          {'kind':'human','id':'usr_admin','name':'Admin','role':'admin'}]
TOKENS = {'agent-a':PEOPLE[0], 'agent-b':PEOPLE[1], 'human':PEOPLE[2], 'admin':PEOPLE[3]}


class Proxy(BaseHTTPRequestHandler):
    disabled = True
    messages = []
    def log_message(self, *_): pass
    def do_GET(self): self.dispatch()
    def do_POST(self): self.dispatch()
    def do_PATCH(self): self.dispatch()
    def dispatch(self):
        parsed = urllib.parse.urlsplit(self.path)
        if parsed.path.startswith(PREFIX):
            forward(self, self.path[len(PREFIX):], True); return
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        data = json.loads(body or '{}') if self.headers.get('Content-Type') == 'application/json' else {}
        token = self.headers.get('Authorization','')[7:]
        principal = TOKENS.get(token)
        human = verify_human_token(token, 'svc_issues')
        if human: principal = next(p for p in PEOPLE if p['id'] == human['sub'])
        status, result = 200, {}
        if parsed.path == '/.treer/apps/identity/verify':
            principal = TOKENS.get(data.get('token'))
            claims = {'service_id':'svc_issues', 'workspace_id':'ws_test', 'sub':principal['id'],
                      'principal_kind':principal['kind'], 'name':principal['name']} if principal else None
            if data.get('token') == 'wrong-workspace':
                claims = {'service_id':'svc_issues', 'workspace_id':'ws_other', 'sub':'ag_a','principal_kind':'agent'}
            if data.get('token') == 'wrong-audience':
                claims = {'service_id':'svc_other','workspace_id':'ws_test','sub':'ag_a','principal_kind':'agent'}
            if data.get('token') == 'removed-agent':
                claims = {'service_id':'svc_issues','workspace_id':'ws_test','sub':'ag_removed','principal_kind':'agent'}
            if claims is None:
                claims = verify_human_token(data.get('token', ''), data.get('audience'))
            result = {'active':bool(claims), 'claims':claims}
        elif parsed.path == '/api/apps/svc_issues/directory':
            if principal or token == 'removed-agent': result = {'principals':PEOPLE}
            else: status, result = 401, {'error':{'code':'app_authentication_required'}}
        elif parsed.path == '/api/apps/svc_issues/messages':
            if not principal: status,result=401,{'error':{'code':'app_authentication_required'}}
            elif self.disabled: status,result=503,{'error':{'code':'core_messages_disabled'}}
            else:
                self.messages.append(data);result={'message':{'message_id':'msg_'+str(len(self.messages))}}
        elif parsed.path == '/api/apps/oauth/authorize':
            query = urllib.parse.parse_qs(parsed.query)
            target = query['redirect_uri'][0] + '?' + urllib.parse.urlencode({'code':'human-code','state':query['state'][0]})
            self.send_response(302); self.send_header('Location',target);self.end_headers();return
        elif parsed.path == '/api/apps/oauth/token':
            result={'access_token':'human','expires_at':'2099-01-01T00:00:00Z'}
        elif parsed.path == '/test/human-token':
            query=urllib.parse.parse_qs(parsed.query)
            result={'access_token':issue_human_token(query.get('audience',['svc_issues'])[0], int(query.get('seconds',['60'])[0]))}
        elif parsed.path == '/test/forwarded': result={'requests':FORWARDED}
        elif parsed.path == '/test/messages': result={'messages':self.messages}
        elif parsed.path == '/test/enable': Proxy.disabled=False
        elif parsed.path == '/test/disable': Proxy.disabled=True
        else: status,result=404,{'error':{'code':'not_found'}}
        raw=json.dumps(result).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)


def main():
    assert not any(k.startswith(('TREER_', 'AGENT_UI_', 'CODEX_UI_')) for k in os.environ), 'Non-hermetic fixture'
    with tempfile.TemporaryDirectory(prefix='issues-test-') as directory:
        proxy = ThreadingHTTPServer(('127.0.0.1',0), Proxy)
        threading.Thread(target=proxy.serve_forever,daemon=True).start()
        store = Store(Path(directory)/'issues.sqlite3')
        app = Application(store)
        server = Server(('127.0.0.1',0),app)
        base=f'http://127.0.0.1:{server.server_port}/';proxy_url=f'http://127.0.0.1:{proxy.server_port}/'
        ingress=ThreadingHTTPServer(('127.0.0.1',0),Ingress);ingress.app_url=base;proxy.app_url=base
        ingress_url=f'http://127.0.0.1:{ingress.server_port}/'
        control_plane=ThreadingHTTPServer(('127.0.0.1',0),ControlPlane)
        control_url=f'http://127.0.0.1:{control_plane.server_port}/'
        control_plane.embedded_url=proxy_url.rstrip('/')+PREFIX
        proxy.browser_origin=control_url.rstrip('/')
        threading.Thread(target=control_plane.serve_forever,daemon=True).start()
        threading.Thread(target=ingress.serve_forever,daemon=True).start()
        app.auth=Auth(store,proxy_url,'svc_issues','ws_test',ingress_url if '--browser-identity' in sys.argv else base)
        print(json.dumps({'url':base,'proxy':proxy_url,'ingress':ingress_url,'embedded':proxy_url.rstrip('/')+PREFIX,'control_plane':control_url}),flush=True)
        server.serve_forever()


if __name__ == '__main__': main()
