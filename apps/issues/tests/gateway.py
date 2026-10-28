"""Local browser gateways reproducing Proxy session-to-identity forwarding.

Not the Rust Proxy: Rust tests separately exercise the actual authenticated
handlers/sanitizers through controller HTTP streams and isolated PostgreSQL.
"""
import base64
import hashlib
import hmac
import html
import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler

PREFIX = '/api/workspaces/ws_test/apps/app_issues/ui/'
KEY = secrets.token_bytes(32)
FORWARDED = []


def issue_human_token(audience='svc_issues', seconds=60):
    claims = {'service_id': audience, 'workspace_id': 'ws_test', 'sub': 'usr_a',
              'principal_kind': 'human', 'name': 'Alice', 'role': 'member',
              'iat': int(time.time()), 'exp': int(time.time()) + seconds, 'jti': secrets.token_hex(16)}
    payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).rstrip(b'=')
    signature = hmac.new(KEY, payload, hashlib.sha256).hexdigest()
    return payload.decode() + '.' + signature


def verify_human_token(token, audience):
    try:
        payload, signature = token.split('.')
        expected = hmac.new(KEY, payload.encode(), hashlib.sha256).hexdigest()
        if not hmac.compare_digest(signature, expected): return None
        claims = json.loads(base64.urlsafe_b64decode(payload + '=' * (-len(payload) % 4)))
        if claims['exp'] <= time.time() or claims['service_id'] != audience: return None
        return claims
    except (ValueError, KeyError, TypeError): return None


def forward(handler, path, embedded):
    # The local gateway is HTTP; use a test name instead of HTTPS-only __Host- cookies.
    cookie_name = 'treer_session' if embedded else 'treer_test_ingress'
    cookies = dict(item.strip().split('=', 1) for item in handler.headers.get('Cookie', '').split(';') if '=' in item)
    if cookies.get(cookie_name) != 'test-login':
        handler.send_response(401); handler.send_header('Content-Type', 'application/json'); handler.end_headers()
        handler.wfile.write(b'{"error":{"code":"gateway_login_required","message":"Treer session required"}}'); return
    headers = {key: value for key, value in handler.headers.items()
               if not key.lower().startswith('x-treer-') and key.lower() not in
               ('cookie', 'authorization', 'proxy-authorization', 'treer-authorization', 'host', 'connection')}
    if not embedded:
        app_cookies = '; '.join(f'{key}={value}' for key,value in cookies.items()
                              if key not in ('treer_session', 'treer_admin_session', '__Host-treer_ingress', 'treer_test_ingress'))
        if app_cookies: headers['Cookie'] = app_cookies
    token = issue_human_token()
    headers['X-Treer-Identity-Token'] = token
    # Match the actual sanitizers' upstream Host rewrite, rather than forwarding
    # the browser's Host. Origin/Fetch Metadata still describe the browser request.
    headers['Host'] = 'svc-issues.internal'
    claims = verify_human_token(token, 'svc_issues')
    FORWARDED.append({'path':path,'embedded':embedded,'jti':claims['jti'],
                      'ttl':claims['exp']-claims['iat'], 'sub':claims['sub'],
                      'cookie':headers.get('Cookie'),
                      'spoofed_header':headers.get('X-Treer-Agent-ID')})
    body = handler.rfile.read(int(handler.headers.get('Content-Length', 0)))
    request = urllib.request.Request(handler.server.app_url + path.lstrip('/'),
                                    method=handler.command, headers=headers, data=body if body else None)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try: response = opener.open(request, timeout=10)
    except urllib.error.HTTPError as exc: response = exc
    with response:
        raw = response.read()
        handler.send_response(response.code)
        for key,value in response.headers.items():
            if key.lower() in ('connection', 'transfer-encoding', 'content-length'): continue
            if embedded and key.lower() in ('set-cookie', 'x-frame-options'): continue
            if embedded and key.lower() == 'content-security-policy':
                value = '; '.join(d for d in value.split(';') if not d.strip().startswith('frame-ancestors')) + f"; frame-ancestors 'self' {handler.server.browser_origin}"
            handler.send_header(key,value)
        handler.send_header('Content-Length', str(len(raw)));handler.end_headers();handler.wfile.write(raw)


class Ingress(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self): forward(self, self.path, False)
    def do_POST(self): forward(self, self.path, False)
    def do_PATCH(self): forward(self, self.path, False)
    def do_DELETE(self): forward(self, self.path, False)


class ControlPlane(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        issue = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query).get('issue', [''])[0]
        root = self.server.embedded_url + ('#issue=' + urllib.parse.quote(issue) if issue else '')
        content = ('<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
                   '<title>Treer test workspace</title><style>body{margin:0;font:14px system-ui;background:#f4f5f7}'
                   'header{padding:14px 20px;background:#1e2633;color:white}iframe{display:block;width:100%;height:900px;border:0}</style>'
                   '</head><body><header>Treer workspace · Issues</header><iframe title="Issues" src="' + html.escape(root, quote=True) + '"></iframe></body></html>').encode()
        self.send_response(200);self.send_header('Content-Type','text/html');self.send_header('Content-Length',str(len(content)));self.end_headers();self.wfile.write(content)
