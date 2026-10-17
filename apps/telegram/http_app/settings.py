"""Workspace-only human owner/admin page. No OAuth, cookies, or bearer fallback."""
import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import threading
import urllib.parse

from settings_app.auth import Auth, Error as IdentityError
from telegram_app.clients import TreerCli
from .common import Error, ID, Limiter, fields, read_backoff
from .public import Handler as DataHandler, Server
from .store import AdminStore, QueueStore

ROOT = Path(__file__).resolve().parent
ASSETS = {'/app.js': (ROOT / 'web/app.js', 'text/javascript'),
          '/app.css': (ROOT / 'web/app.css', 'text/css'),
          '/base.css': (ROOT.parent / 'ui/src/app.css', 'text/css')}
CSP = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"


def representation(accept, agent):
    choices = []
    for order, item in enumerate(accept.split(',')):
        parts = [part.strip().lower() for part in item.split(';')]
        if parts[0] not in ('text/html', 'text/markdown'):
            continue
        quality = 1
        for part in parts[1:]:
            if part.startswith('q='):
                try: quality = float(part[2:])
                except ValueError: quality = 0
        if 0 < quality <= 1: choices.append((-quality, order, parts[0]))
    return min(choices)[2] if choices else 'text/html' if 'mozilla/' in agent.lower() else 'text/markdown'


class Settings:
    def __init__(self, queues, admin, endpoint_url, auth=None, *, proxy_url=None, service_id=None, workspace=None, public_url=None, cli=None):
        self.queues, self.admin, self.endpoint_url, self.auth = queues, admin, endpoint_url, auth
        self.proxy_url, self.service_id, self.workspace, self.public_url = proxy_url, service_id, workspace, public_url
        self.cli = cli or TreerCli(os.environ.get('TREER_CLI', 'treer'))
        self.lock, self.limits = threading.Lock(), Limiter()

    def identity(self):
        with self.lock:
            if self.auth is None:
                if not self.service_id or not self.workspace or not self.public_url:
                    # Own-runtime discovery only: never select an audience from the request.
                    apps = self.cli.run(['app', 'list'])['apps']
                    app = next((item for item in apps if item['runtime_agent_id'] == os.environ.get('TREER_AGENT_ID')), None)
                    if app is None:
                        raise Error(503, 'app_not_ready', 'Managed App identity unavailable')
                    self.service_id, self.workspace = app['service_id'], app['workspace_id']
                    self.public_url = app.get('public_url') or (self.proxy_url.rstrip('/') + f"/api/workspaces/{self.workspace}/apps/{app['app_id']}/ui/")
                self.auth = Auth(self.proxy_url, self.service_id, self.workspace, self.public_url)
            return self.auth

    def authenticate(self, headers):
        # Even a valid explicit Bearer/cookie is not a fallback for a missing/bad header.
        values = headers.get_all('X-Treer-Identity-Token') or []
        if len(values) != 1 or not values[0]:
            raise Error(401, 'authentication_required', 'Open settings from the Treer workspace')
        try:
            principal, directory = self.identity().verify(values[0])
        except IdentityError as error:
            raise Error(error.status, error.code, error.message) from None
        if principal.get('kind') != 'human' or principal.get('role') not in ('owner', 'admin'):
            raise Error(403, 'forbidden', 'Workspace owner or admin required')
        return principal, directory

    def mutation(self, headers, principal):
        identity = self.identity()
        allowed = {Auth.origin(identity.public_url), Auth.origin(identity.proxy_url)}
        if (headers.get('Origin') not in allowed or headers.get('X-Http-Settings') != '1'
                or headers.get('Sec-Fetch-Site') not in (None, 'same-origin', 'same-site')):
            raise Error(403, 'csrf_rejected', 'Trusted origin and X-Http-Settings: 1 required')
        if not self.limits.take(principal['id'], 20, 3600):
            raise Error(429, 'rate_limited', 'Too many management changes', 60)

    def state(self, principal, directory):
        with self.queues._connect() as db:
            row = db.execute("SELECT value FROM settings WHERE key='paused'").fetchone()
        clients = {row['id']: row for row in self.queues.summaries()}
        for user in self.admin.list_users('http', self.admin.ACCOUNT):
            client = clients.setdefault(user['user_id'], {'id': user['user_id'], 'revoked': True, 'inbound': 0, 'outbound': 0})
            binding = self.admin.get_binding('http', self.admin.ACCOUNT, user['user_id'])
            client.update(label=user['label'], poc=binding.target_agent_id if binding else None)
        return {'me': {k: principal.get(k) for k in ('id','name','role')}, 'endpoint_url': self.endpoint_url,
                'paused': bool(row and row[0] == '1'), 'clients': list(clients.values()),
                'agents': [{k: p.get(k) for k in ('id','name','path')} for p in directory if p.get('kind') == 'agent'],
                'drops': self.admin.drop_summary(),
                'pair_backoff': read_backoff(self.admin.path.parent),
                'audit': self.admin.audit_list('http', self.admin.ACCOUNT, 50), 'per_ip_effective': False}

    def issue(self, principal, directory, body, client=None):
        fields(body, ('label','poc'), ('expires_at',))
        if not any(p.get('id') == body['poc'] and p.get('kind') == 'agent' for p in directory):
            raise Error(403, 'poc_unavailable', 'POC is not visible in the verified workspace directory')
        expires = body.get('expires_at')
        if expires is not None:
            try:
                if not isinstance(expires, str): raise ValueError()
                parsed = datetime.fromisoformat(expires.replace('Z', '+00:00'))
                if parsed.tzinfo is None: raise ValueError()
                expires = parsed.timestamp()
            except (ValueError, OverflowError):
                raise Error(400, 'invalid_expiry', 'Expiry requires an RFC 3339 timestamp with timezone') from None
        return self.admin.issue('human:' + principal['id'], body['poc'], body['label'], client=client, expires=expires)


class Handler(DataHandler):
    server_version = 'TreerHttpSettings'

    def content(self, raw, media):
        self.send_response(200)
        for key, value in (('Content-Type',media+'; charset=utf-8'),('Content-Length',str(len(raw))),
                           ('Cache-Control','no-store'),('X-Content-Type-Options','nosniff'),
                           ('Referrer-Policy','no-referrer'),('Content-Security-Policy',CSP),('Vary','Accept, User-Agent')):
            self.send_header(key,value)
        self.end_headers()
        self.close_connection = True
        if self.command != 'HEAD': self.wfile.write(raw)

    def dispatch(self):
        try:
            app = self.server.app
            principal, directory = app.authenticate(self.headers)
            url = urllib.parse.urlsplit(self.path)
            path = url.path
            if url.scheme or url.netloc or url.query:
                raise Error(404,'not_found','Route not found')
            if self.command in ('GET','HEAD') and path == '/':
                media = representation(self.headers.get('Accept',''),self.headers.get('User-Agent',''))
                file = ROOT / ('web/index.html' if media == 'text/html' else 'AGENT.md')
                self.content(file.read_bytes(),media)
                return
            if self.command in ('GET','HEAD') and path in ASSETS:
                file, media = ASSETS[path]
                self.content(file.read_bytes(),media)
                return
            if self.command == 'GET' and path == '/v1/state':
                self.reply(200,app.state(principal,directory))
                return
            if self.command != 'POST': raise Error(404,'not_found','Route not found')
            app.mutation(self.headers,principal)
            body = self.body(4096)
            if path == '/v1/pairings':
                self.reply(201,app.issue(principal,directory,body)); return
            if path == '/v1/pause':
                fields(body,('paused',))
                if type(body['paused']) is not bool: raise Error(400,'invalid_request','Boolean required')
                app.queues.control(paused=body['paused'])
                app.admin.notice('human:'+principal['id'],'endpoint.pause' if body['paused'] else 'endpoint.resume')
                self.reply(200,{'ok':True}); return
            parts = path.strip('/').split('/')
            if len(parts) != 4 or parts[:2] != ['v1','clients'] or not ID.fullmatch(parts[2]):
                raise Error(404,'not_found','Route not found')
            client, operation = parts[2:]
            known = next((u for u in app.admin.list_users('http',app.admin.ACCOUNT) if u['user_id']==client),None)
            if known is None: raise Error(404,'client_unavailable','Client unavailable')
            if operation == 'rotate': self.reply(201,app.issue(principal,directory,body,client)); return
            if operation == 'revoke':
                fields(body,())
                app.admin.revoke(client,'human:'+principal['id'])
            elif operation == 'pause':
                fields(body,('paused',))
                if type(body['paused']) is not bool: raise Error(400,'invalid_request','Boolean required')
                app.queues.control(client,paused=body['paused'])
                app.admin.notice('human:'+principal['id'],'client.pause' if body['paused'] else 'client.resume',client)
            else: raise Error(404,'not_found','Route not found')
            self.reply(200,{'ok':True})
        except Error as error:
            self.reply(error.status,{'error':{'code':error.code,'message':error.message}},error.retry)
        except Exception:
            # Auth/CLI/file exceptions may contain credential-bearing values.
            self.reply(503,{'error':{'code':'unavailable','message':'Settings unavailable'}})

    do_GET = do_POST = do_HEAD = do_OPTIONS = do_PUT = do_PATCH = do_DELETE = do_TRACE = do_CONNECT = dispatch


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir',default=os.environ.get('TREER_APP_STATE_DIR'),required=not os.environ.get('TREER_APP_STATE_DIR'))
    parser.add_argument('--proxy-url',required=True)
    parser.add_argument('--endpoint-url',required=True)
    parser.add_argument('--service-id')
    parser.add_argument('--workspace-id')
    parser.add_argument('--public-url')
    parser.add_argument('--listen',default='127.0.0.1')
    parser.add_argument('--port',type=int,default=9443)
    args = parser.parse_args()
    for value in (args.proxy_url,args.endpoint_url,args.public_url):
        if value is None: continue
        parsed = urllib.parse.urlsplit(value)
        if parsed.scheme not in ('http','https') or not parsed.netloc or parsed.username or parsed.password or parsed.query or parsed.fragment:
            parser.error('URLs must use HTTP(S) without credentials, query or fragment')
    root = Path(args.state_dir)
    queues = QueueStore(root/'data.sqlite')
    app = Settings(queues,AdminStore(root/'admin.sqlite',queues),args.endpoint_url,proxy_url=args.proxy_url,
                   service_id=args.service_id,workspace=args.workspace_id,public_url=args.public_url)
    with Server((args.listen,args.port),Handler,app) as server: server.serve_forever()
