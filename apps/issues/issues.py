#!/usr/bin/env python3
"""Workspace issue tracker: no third-party runtime dependencies."""
import argparse
import http.cookies
import json
import mimetypes
import os
import re
import subprocess
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from auth import Auth, Error
from notify import ASK_INTENTS, Digest, POLICY_MODES, candidates, digest_form, issue_ref, label, enabled as notify_enabled, hourly_limit, path_tokens
from reminders import Reminders, backoff as reminder_backoff, enabled as reminders_enabled, roster, skip_labels
from store import Store, fields, fail

ROOT = Path(__file__).resolve().parent
COOKIE = 'treer_issues_session'
# Committed React build (ui/ sources, `npm run build:ui`): a fixed allow-list, never a directory server.
ASSETS = ('/app.js', '/app.css', '/boot.js')


def representation(accept, ua):
    choices = []
    for order, entry in enumerate(accept.split(',')):
        parts = [p.strip().lower() for p in entry.split(';')]
        if parts[0] not in ('text/html', 'text/markdown'):
            continue
        quality = 1
        for part in parts[1:]:
            if part.startswith('q='):
                try:
                    quality = float(part[2:])
                except ValueError:
                    quality = 0
        if 0 < quality <= 1:
            choices.append((-quality, order, parts[0]))
    return min(choices)[2] if choices else 'text/html' if 'mozilla/' in ua.lower() else 'text/markdown'


class Application:
    def __init__(self, store, auth=None):
        self.store, self.auth = store, auth
        self.lock = threading.Lock()
        self.health_at, self.health_value = -1e9, None
        self.search_hits = {}

    def identity_provider(self):
        with self.lock:
            if self.auth is None:
                service_id = os.environ.get('ISSUES_SERVICE_ID')
                public_url = os.environ.get('ISSUES_PUBLIC_URL')
                workspace = os.environ.get('ISSUES_WORKSPACE_ID') or os.environ.get('TREER_WORKSPACE_ID')
                if not service_id or not public_url:
                    # Discover only this Managed App's owned service, never a caller-supplied audience.
                    try:
                        result = subprocess.run([os.environ.get('TREER_BIN', 'treer'), 'app', 'list'],
                                                capture_output=True, text=True, timeout=10, check=True)
                        apps = json.loads(result.stdout)['apps']
                        app = next(a for a in apps if a['runtime_agent_id'] == os.environ.get('TREER_AGENT_ID'))
                        service_id = app['service_id']
                        public_url = app.get('public_url') or (os.environ['ISSUES_PROXY_URL'].rstrip('/') +
                            f"/api/workspaces/{app['workspace_id']}/apps/{app['app_id']}/ui/")
                        workspace = app['workspace_id']
                    except (OSError, ValueError, KeyError, StopIteration, subprocess.SubprocessError):
                        fail('app_not_ready', 'Managed App identity discovery unavailable; configure ISSUES_SERVICE_ID and ISSUES_PUBLIC_URL', 503)
                if not workspace or not public_url:
                    fail('app_not_configured', 'Workspace and workspace-authenticated public URL required for OAuth', 503)
                self.auth = Auth(self.store, os.environ['ISSUES_PROXY_URL'], service_id, workspace, public_url)
            return self.auth

    def resolve_paths(self, action, body, token):
        """`@/team/agent` mentions are resolved by Core with the AUTHOR's own token (their Policy decides what a path
        means and whether it is visible), before the write transaction. Returns {lower-case path: agent ID}."""
        if not notify_enabled() or not isinstance(body, dict):
            return {}
        if action == 'comment':
            text = '\n'.join(v for v in (body.get('title'), body.get('body')) if isinstance(v, str))
        elif action in ('create', 'update'):
            text = '\n'.join(v for v in (body.get('title'), body.get('description')) if isinstance(v, str))
        else:
            return {}
        tokens = path_tokens(text) if isinstance(text, str) else []
        if not tokens:
            return {}
        try:
            auth = self.identity_provider()
        except Error:
            return {}
        resolved = {}
        for path in tokens:
            try:
                answer = auth.request('POST', f'/api/apps/{auth.service_id}/recipients/resolve', token, body={'recipients': [path]})
            except Error:
                continue
            for entry in answer.get('recipients') or []:
                if isinstance(entry, dict) and entry.get('kind') == 'agent' and isinstance(entry.get('id'), str):
                    resolved[path.lower()] = entry['id']
        return resolved

    SEARCH_PER_MINUTE = 30

    def search_allowed(self, principal, clock=time.monotonic):
        """A per-principal sliding-window rate limit (searches can scan the index, so one caller cannot slow the App)."""
        now = clock()
        with self.lock:
            hits = [t for t in self.search_hits.get(principal, []) if now - t < 60]
            if len(hits) >= self.SEARCH_PER_MINUTE:
                self.search_hits[principal] = hits
                fail('rate_limited', f'At most {self.SEARCH_PER_MINUTE} searches per minute', 429)
            hits.append(now)
            self.search_hits[principal] = hits
            if len(self.search_hits) > 1000:
                self.search_hits = {k: v for k, v in self.search_hits.items() if v and now - v[-1] < 60}

    def set_policy(self, body, actor, directory):
        """PUT /v1/notify-policy {mode, agent?}. The verified caller may change their own policy; a human owner/admin may
        change any Agent's. An Agent can never change another principal's. Unknown fields are rejected."""
        fields(body, ('mode', 'agent'))
        mode, subject = body.get('mode'), body.get('agent') or actor['id']
        if mode not in POLICY_MODES:
            fail('invalid_field', 'mode must be all or asks')
        if subject != actor['id'] and not (actor.get('kind') == 'human' and actor.get('role') in ('owner', 'admin')):
            fail('forbidden', 'Only the principal itself or a human owner/admin can change a notification policy', 403)
        if not any(p['id'] == subject and p.get('kind') == 'agent' for p in directory):
            fail('invalid_field', 'Notification policies are kept for Agents in the directory')
        return self.store.set_policy(subject, mode, actor['id'], self.store.clock())

    @staticmethod
    def is_ask_for(event, recipient):
        """Whether this event must reach a recipient who chose `asks` at once: a decision/blocker/incident comment, an
        [action-X] comment aimed at them, the issue becoming blocked, or an assignment to them (a work assignment is
        never hidden by a policy meant to quiet progress chatter)."""
        changes = event.get('changes') or {}
        if event.get('kind') == 'comment':
            return event.get('intent') in ASK_INTENTS or (event.get('intent') == 'action' and event.get('intent_target') == recipient)
        return changes.get('status') == 'blocked' or changes.get('assignee') == recipient

    def route(self, event, recipients, actor, now):
        """Split recipients into (immediate, filtered) by each one's own policy. `all` (the default) is unchanged.
        An `asks` recipient gets ask-type events at once (comments, blocked, assignments), all bounded to ASK_PER_AUTHOR pushes per author per hour."""
        policies = self.store.policies()
        immediate, filtered = [], []
        for recipient in recipients:
            if policies.get(recipient, 'all') != 'asks':
                immediate.append(recipient)
            elif self.is_ask_for(event, recipient) and self.store.ask_push_allowed(recipient, actor['id'], now):
                immediate.append(recipient)
            else:
                filtered.append(recipient)
        return immediate, filtered

    def park(self, issue_id, notice, filtered, actor, now):
        """Filtered updates go to the recipient's hourly digest, but only for an actor whom Core has already let message
        that recipient this hour (notify_proven): the digest is sent under the App identity and must not become a way
        around Message Policy. Unproven updates are dropped, and the hint says so. Returns {recipient: outcome}."""
        outcome = {}
        for recipient in filtered:
            if not self.store.actor_proven(recipient, actor['id'], now):
                outcome[recipient] = 'not_delivered'
            else:
                self.store.queue_hourly(recipient, issue_id, digest_form(notice), now)   # 'overflow' is still a digest
                outcome[recipient] = 'digest'
        return outcome

    @staticmethod
    def hints(event, filtered, parked, directory):
        """For people who wrote a plain comment mentioning an Agent that only wants asks."""
        names = {p['id']: label(p.get('name', p['id']), p['id']) for p in directory}
        out = []
        for agent in event.get('mentions') or []:
            if agent in filtered:
                reached = parked.get(agent) == 'digest'
                out.append({'agent': names.get(agent, agent), 'delivery': 'digest' if reached else 'not_delivered',
                            'tip': (f'{names.get(agent, agent)} sees this only in an hourly digest; ' if reached else f'{names.get(agent, agent)} may not receive this; ')
                            + 'to ask for action start the title with [decision], [blocker], [incident] or '
                            + f'[action-{names.get(agent, agent)}]'})
        return out

    def deliver(self, token, issue_id, event_id, notice, send, directory, actor, now):
        """One Message per recipient kind: Agents get a fixed CLI reference (no URL), people the browser link when the
        App has a public URL. An unknown kind is treated as an Agent. Each group has its own idempotency key and a
        failed group releases only its own recipients."""
        humans = {p['id'] for p in directory if p.get('kind') == 'human'}
        groups = [(key, members, text) for key, members, text in (
            ('', [r for r in send if r not in humans], issue_ref(issue_id, True)),
            ('-human', [r for r in send if r in humans], None)) if members]
        failure = None
        try:
            auth = self.identity_provider()
        except Error as exc:
            auth, failure = None, exc
        delivered = []
        for suffix, members, tail in groups:
            if auth is None:
                self.store.release_notifications(issue_id, members, actor['id'], now)
                continue
            if tail is None:
                url = getattr(auth, 'public_url', '') or ''
                tail = f'{url}#issue={issue_id}' if url else ''
            try:
                auth.request('POST', f'/api/apps/{auth.service_id}/messages', token, body={
                    'recipients': members, 'context_ids': [],
                    'body': f"Issue #{issue_id} \u00b7 {notice}" + (f'\n{tail}' if tail else ''),
                    'idempotency_key': f'issues-event-{event_id}{suffix}'})
                delivered.extend(members)
            except Error as exc:
                failure = failure or exc
                self.store.release_notifications(issue_id, members, actor['id'], now)
        if delivered:
            self.store.prove_notifications(delivered, actor['id'], now)
        if failure is None:
            return {'status': 'sent'}
        return {'status': 'disabled' if failure.code == 'core_messages_disabled' else 'unavailable',
                'code': failure.code, 'message': 'Change saved; Core Message notification was not delivered'
                + (' to everyone' if delivered else '')}

    HEALTH_TTL = 2.0

    def deployment_health(self, clock=time.monotonic):
        """Readiness for scripts/app-deploy: running version (APP_DEPLOY_VERSION), the App's stable ID and a working
        database. Unauthenticated and cacheless for clients like /health; reveals only a commit SHA and the App ID.
        The database probe is memoised for HEALTH_TTL seconds so repeated requests cannot be used as DB load."""
        now = clock()
        with self.lock:
            if now - self.health_at < self.HEALTH_TTL and self.health_value is not None:
                return dict(self.health_value)
        try:
            with self.store.connect() as db:
                db.execute('SELECT 1 FROM issues LIMIT 1').fetchall()
            database = True
        except Exception:
            database = False
        value = {'version': os.environ.get('APP_DEPLOY_VERSION'), 'app_id': os.environ.get('TREER_AGENT_ID'),
                 'ready': database, 'database_ready': database}
        with self.lock:
            self.health_at, self.health_value = now, value
        return dict(value)

    def mutation(self, issue_id, action, body, actor, directory, token):
        paths = self.resolve_paths(action, body, token)
        issue_id, event_id, event, recipients, notice, missed = self.store.mutate(issue_id, action, body, actor, directory, paths)
        notification = {'status': 'not_requested'}
        if action in ('create', 'update', 'comment'):
            if not notify_enabled():
                notification = {'status': 'off'}
            elif not recipients:
                notification = {'status': 'no_recipients'}
            else:
                # One Message per event: this is the only notification path, so nothing is sent twice.
                now = self.store.clock()
                immediate, filtered = self.route(event, recipients, actor, now)
                parked = self.park(issue_id, notice, filtered, actor, now)
                if not immediate:
                    notification = {'status': 'filtered'}
                else:
                    send, merged, throttled = self.store.claim_notifications(issue_id, immediate, digest_form(notice), now, actor['id'], hourly_limit())
                    if not send:
                        notification = {'status': 'coalesced' if merged else 'throttled'}
                    else:
                        notification = self.deliver(token, issue_id, event_id, notice, send, directory, actor, now)
                    if send and merged:
                        notification['coalesced'] = len(merged)
                    if throttled:
                        notification['throttled'] = len(throttled)
                hints = self.hints(event, filtered, parked, directory)
                if hints:
                    notification['hints'] = hints
        if event.get('intent_note'):
            notification = dict(notification, intent_note=event['intent_note'])
        if missed:
            notification = dict(notification, unresolved_mentions=missed)
            options = {t: c for t in missed if (c := candidates(t, directory))}
            if options:
                notification['mention_candidates'] = options
        self.store.notification(event_id, notification)
        return dict(self.store.show(issue_id), notification=notification)


class Server(ThreadingHTTPServer):
    daemon_threads = True
    def __init__(self, address, application):
        self.application = application
        super().__init__(address, Handler)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.dispatch('GET')

    def do_HEAD(self):
        self.dispatch('HEAD')

    def do_POST(self):
        self.dispatch('POST')

    def do_PATCH(self):
        self.dispatch('PATCH')

    def do_PUT(self):
        self.dispatch('PUT')

    def raw_cookie(self, name=COOKIE):
        cookies = http.cookies.SimpleCookie()
        try:
            cookies.load(self.headers.get('Cookie', ''))
            return cookies[name].value if name in cookies else ''
        except http.cookies.CookieError:
            return ''

    def body(self):
        try:
            length = int(self.headers.get('Content-Length', '-1'))
        except ValueError:
            fail('invalid_body', 'Content-Length required')
        if not 0 <= length <= 65536:
            fail('body_too_large', 'Request body must be at most 65536 bytes', 413)
        if self.headers.get('Content-Type', '').split(';')[0] != 'application/json':
            fail('invalid_content_type', 'application/json required', 415)
        try:
            value = json.loads(self.rfile.read(length))
        except (ValueError, UnicodeError):
            fail('invalid_body', 'JSON object required')
        if not isinstance(value, dict):
            fail('invalid_body', 'JSON object required')
        return value

    def cookie(self, raw, age, name=COOKIE):
        secure = '; Secure' if self.server.application.identity_provider().public_url.startswith('https://') else ''
        return f'{name}={raw}; Path=/; Max-Age={age}; HttpOnly; SameSite=Lax{secure}'

    def redirect(self, target, cookie=None):
        self.reply(302, b'', 'text/plain', {'Location': target, **({'Set-Cookie': cookie} if cookie else {})})

    def dispatch(self, method):
        try:
            url = urllib.parse.urlsplit(self.path)
            path = url.path
            query = {k: v[0] for k, v in urllib.parse.parse_qs(url.query, keep_blank_values=True).items()}
            app = self.server.application
            if path == '/' and method in ('GET', 'HEAD'):
                media = representation(self.headers.get('Accept', ''), self.headers.get('User-Agent', ''))
                file = ROOT / ('dist/index.html' if media == 'text/html' else 'AGENT.md')
                self.reply(200, file.read_bytes(), media+'; charset=utf-8', {'Vary': 'Accept, User-Agent'})
                return
            if (path in ASSETS or path == '/issues') and method in ('GET', 'HEAD'):
                file = ROOT / ('issues' if path == '/issues' else 'dist/'+path[1:])
                self.reply(200, file.read_bytes(), 'text/plain; charset=utf-8' if path == '/issues' else mimetypes.guess_type(str(file))[0])
                return
            if path == '/deployment-health' and method in ('GET', 'HEAD'):
                health = app.deployment_health()
                self.json(200 if health['ready'] else 503, health)
                return
            if path == '/health' and method in ('GET', 'HEAD'):
                self.json(200, {'status': 'ok', 'service': 'issues'})
                return
            if path == '/api/auth/start' and method == 'GET':
                location, browser = app.identity_provider().start()
                self.redirect(location, self.cookie(browser, 600, 'treer_issues_oauth'))
                return
            if path == '/api/auth/callback' and method == 'GET':
                auth = app.identity_provider()
                raw, age = auth.finish(query.get('code', ''), query.get('state', ''), self.raw_cookie('treer_issues_oauth'))
                self.redirect(auth.public_url, self.cookie(raw, age))
                return
            if not path.startswith('/v1/') and path != '/api/auth/logout':
                fail('route_not_found', 'Route not found', 404)
            auth = app.identity_provider()
            token, actor, directory, cookie_auth = auth.authenticate(self.headers, self.raw_cookie())
            if method not in ('GET', 'HEAD'):
                auth.check_browser_mutation(self.headers, actor, cookie_auth)
            if path == '/api/auth/logout' and method == 'POST':
                auth.logout(self.raw_cookie())
                self.reply(200, b'{}', 'application/json', {'Set-Cookie': self.cookie('', 0)})
            elif path == '/v1/me' and method == 'GET':
                self.json(200, {'principal': actor, 'notifications': 'Best effort; delivery status is recorded on each activity and mutation'})
            elif path == '/v1/directory' and method == 'GET':
                # Agent `path` is Core's canonical path for display and location only (null: hidden or unplaced; authorization
                # always uses the stable ID). `paths` tells a Proxy without app.directory-paths.v1 apart from a null path.
                capabilities = sorted(getattr(directory, 'capabilities', ()))
                self.json(200, {'principals': directory, 'capabilities': capabilities,
                                'paths': 'supported' if 'app.directory-paths.v1' in capabilities else 'unsupported'})
            elif path == '/v1/search' and method == 'GET':
                app.search_allowed(actor['id'])
                self.json(200, app.store.search(query))
            elif path == '/v1/stale' and method == 'GET':
                fields(query, ('hours', 'limit', 'offset'))
                try:
                    hours, limit, offset = int(query.get('hours', 4)), int(query.get('limit', 50)), int(query.get('offset', 0))
                except ValueError:
                    fail('invalid_field', 'hours, limit and offset must be integers')
                self.json(200, app.store.stale(hours, limit, offset, skip_labels()))
            elif path == '/v1/notify-policy' and method == 'GET':
                fields(query, ('audit',))
                result = {'default': 'all', 'modes': list(POLICY_MODES), 'policies': app.store.policies()}
                if query.get('audit') == '1':
                    result['audit'] = app.store.policy_audit()
                self.json(200, result)
            elif path == '/v1/notify-policy' and method == 'PUT':
                self.json(200, app.set_policy(self.body(), actor, directory))
            elif path == '/v1/issues' and method == 'GET':
                if query.get('assignee') == 'me':
                    query['assignee'] = actor['id']
                self.json(200, app.store.list(query))
            elif path == '/v1/issues' and method == 'POST':
                self.json(201, app.mutation(None, 'create', self.body(), actor, directory, token))
            else:
                parts = path.strip('/').split('/')
                if len(parts) not in (3, 4, 5) or parts[:2] != ['v1', 'issues'] or not parts[2].isdigit() or int(parts[2]) < 1:
                    fail('route_not_found', 'Route not found', 404)
                issue_id = int(parts[2])
                if len(parts) == 5:
                    if method != 'GET' or parts[3] != 'activity' or not parts[4].isdigit():
                        fail('route_not_found', 'Route not found', 404)
                    self.json(200, {'event': app.store.event(issue_id, int(parts[4]))})
                    return
                action = parts[3] if len(parts) == 4 else ''
                if method == 'GET' and action in ('', 'children'):
                    fields(query, ('offset', 'limit', 'comments'))
                    if query.get('comments', 'full') not in ('full', 'titles') or (action and 'comments' in query):
                        fail('invalid_field', 'comments must be full or titles (issue reads only)')
                    try:
                        result = (app.store.children(issue_id, int(query.get('offset', 0)), int(query.get('limit', 50))) if action
                                  else app.store.show(issue_id, int(query.get('offset', 0)), int(query.get('limit', 50)), query.get('comments') == 'titles'))
                    except ValueError:
                        fail('invalid_pagination', 'Invalid pagination')
                    self.json(200, result)
                elif (method == 'PATCH' and not action) or (method == 'POST' and action in ('comment', 'claim')):
                    self.json(200, app.mutation(issue_id, action or 'update', self.body(), actor, directory, token))
                else:
                    fail('route_not_found', 'Route not found', 404)
        except Error as exc:
            self.json(exc.status, {'error': {'code': exc.code, 'message': exc.message}})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:
            print(f'Issues request failed: {type(exc).__name__}', file=sys.stderr)
            self.json(500, {'error': {'code': 'internal_error', 'message': 'Issue operation failed'}})

    def json(self, status, value):
        self.reply(status, json.dumps(value, ensure_ascii=False).encode(), 'application/json; charset=utf-8')

    def reply(self, status, content, media, headers=None):
        self.send_response(status)
        self.send_header('Content-Type', media)
        self.send_header('Content-Length', str(len(content)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'same-origin')
        self.send_header('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'")
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(content)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description='Workspace issue tracker (configuration is environment-based)')
    # Managed Apps deployed with scripts/app-deploy cannot carry environment variables in their saved command; the
    # deployment's fixed arguments can carry this one. It overrides ISSUES_PROXY_URL.
    parser.add_argument('--proxy-url', help='trusted Treer Proxy public URL (default: $ISSUES_PROXY_URL)')
    parser.add_argument('--release-agent', help='stable ID of the Agent whose `[release] … canary_passed <sha>` comments remind assignees to close shipped issues '
                        '(default: $ISSUES_RELEASE_AGENT; unset: the feature is off)')
    parser.add_argument('--leader-agent', help='stable ID of the leader Agent that `leader`/`user` in waiting_on and ownerless issues '
                        'resolve to (default: $ISSUES_LEADER_AGENT; unset: those are refused / not reminded)')
    return parser.parse_args(argv)


def main(argv=None):
    os.umask(0o077)
    arguments = parse_args(argv)
    if arguments.proxy_url:
        os.environ['ISSUES_PROXY_URL'] = arguments.proxy_url
    if not os.environ.get('ISSUES_PROXY_URL'):
        raise ValueError('ISSUES_PROXY_URL is required (the trusted Treer Proxy public URL)')
    proxy = urllib.parse.urlsplit(os.environ['ISSUES_PROXY_URL'])
    if proxy.scheme not in ('http', 'https') or not proxy.netloc or proxy.username or proxy.password:
        raise ValueError('ISSUES_PROXY_URL must be an HTTP(S) URL without credentials')
    schedule = reminder_backoff() if reminders_enabled() else None   # invalid reminder settings stop the start-up, before any side effect
    host, port = os.environ.get('ISSUES_LISTEN', '127.0.0.1:9440').rsplit(':', 1)
    leader = arguments.leader_agent or os.environ.get('ISSUES_LEADER_AGENT') or None
    if leader is not None and not re.fullmatch(r'ag_[0-9a-f]{32}', leader):
        raise ValueError('the leader Agent must be a stable Agent ID (ag_ plus 32 hex digits)')
    release = arguments.release_agent or os.environ.get('ISSUES_RELEASE_AGENT') or None
    if release is not None and not re.fullmatch(r'ag_[0-9a-f]{32}', release):
        raise ValueError('the release Agent must be a stable Agent ID (ag_ plus 32 hex digits)')
    application = Application(Store(os.environ.get('ISSUES_DB', '.treer/apps/issues/issues.sqlite3'), leader_agent=leader, release_agent=release))
    server = Server((host, int(port)), application)
    print(f'Issues listening on {host}:{server.server_port}', flush=True)
    if notify_enabled():
        def digest_link():
            try:
                return application.identity_provider().public_url
            except Error:
                return os.environ.get('ISSUES_PUBLIC_URL') or 'http://issues.treer/'
        Digest(application.store, digest_link, os.environ.get('TREER_BIN', 'treer')).start()
        print('Issues change notifications enabled (ISSUES_NOTIFY=off disables)', flush=True)
    if reminders_enabled():
        Reminders(application.store, os.environ.get('TREER_BIN', 'treer'), schedule=schedule,
                  live_agents=roster(os.environ.get('TREER_BIN', 'treer'))).start()
        print(f'Issues reminders enabled with exponential backoff first={schedule.first:g}s factor={schedule.factor:g} max={schedule.maximum:g}s '
              '(ISSUES_REMINDERS=off disables)', flush=True)
    server.serve_forever()


if __name__ == '__main__':
    main()
