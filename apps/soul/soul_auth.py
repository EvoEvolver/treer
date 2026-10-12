"""Treer App OAuth/identity contract, adapted from apps/issues; no local accounts."""
import base64
import hashlib
import json
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime


class Error(Exception):
    def __init__(self, status, code, message):
        self.status, self.code, self.message = status, code, message
        super().__init__(message)


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


class Auth:
    def __init__(self, store, proxy_url, service_id, workspace_id, public_url):
        self.store = store
        self.proxy_url = proxy_url.rstrip('/') + '/'
        self.service_id = service_id
        self.workspace_id = workspace_id
        self.public_url = public_url.rstrip('/') + '/'

    def request(self, method, path, token=None, body=None, form=None):
        headers = {'Accept': 'application/json'}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers['Content-Type'] = 'application/json'
        if form is not None:
            data = urllib.parse.urlencode(form).encode()
            headers['Content-Type'] = 'application/x-www-form-urlencoded'
        request = urllib.request.Request(urllib.parse.urljoin(self.proxy_url, path.lstrip('/')),
                                         data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                result = json.loads(response.read(2 * 1024 * 1024))
                if not isinstance(result, dict):
                    raise ValueError()
                return result
        except urllib.error.HTTPError as exc:
            try:
                code = json.loads(exc.read(65536))['error']['code']
            except (ValueError, KeyError, TypeError):
                code = 'treer_request_failed'
            raise Error(exc.code if exc.code in (400, 401, 403, 404) else 502,
                        code, 'Treer rejected the request') from exc
        except (OSError, ValueError) as exc:
            raise Error(502, 'treer_unavailable', 'Treer API unavailable') from exc

    def verify(self, token):
        if not token or len(token) > 8192:
            raise Error(401, 'authentication_required', 'Sign in with Treer or use a service token')
        result = self.request('POST', '/.treer/apps/identity/verify',
                              body={'token': token, 'audience': self.service_id})
        claims = result.get('claims') or {}
        if (result.get('active') is not True or claims.get('service_id') != self.service_id
                or claims.get('workspace_id') != self.workspace_id
                or claims.get('principal_kind') not in ('human', 'agent') or not claims.get('sub')):
            raise Error(401, 'identity_invalid', 'Identity is inactive or outside this App workspace')
        # Require current workspace directory membership in addition to verified claims.
        directory = self.directory(token)
        principal = next((p for p in directory if p.get('id') == claims['sub']
                          and p.get('kind') == claims['principal_kind']), None)
        if principal is None:
            raise Error(403, 'identity_unavailable', 'Caller is absent from the workspace directory')
        return principal, directory

    def authenticate(self, headers, raw_cookie):
        # Header presence, including an empty/invalid value, is authoritative:
        # never silently fall back to a different human session after rejection.
        cookie_auth = False
        if 'X-Treer-Identity-Token' in headers:
            token = headers.get('X-Treer-Identity-Token')
            if not token:
                raise Error(401, 'identity_invalid', 'Injected identity token is empty')
        elif 'Authorization' in headers:
            value = headers.get('Authorization', '')
            token = value[7:] if value.startswith('Bearer ') else None
        else:
            cookie_auth = True
            token = self.session_token(raw_cookie)
        principal, directory = self.verify(token)
        return token, principal, directory, cookie_auth

    def check_browser_mutation(self, headers, principal, cookie_auth):
        # Injected human headers originated in an ambient browser session too.
        # Their presence must not skip CSRF protection as Agent bearer calls do.
        site = headers.get('Sec-Fetch-Site')
        if site is not None and site not in ('same-origin', 'same-site'):
            raise Error(403, 'csrf_rejected', 'Cross-site browser mutations rejected')
        origin = headers.get('Origin')
        if origin is not None:
            allowed = {self.origin(self.public_url), self.origin(self.proxy_url)}
            if origin not in allowed:
                raise Error(403, 'csrf_rejected', 'Browser origin is not this App or its Treer tunnel')
        if (cookie_auth or principal['kind'] == 'human') and headers.get('X-Soul-Request') != '1':
            raise Error(403, 'csrf_rejected', 'Human mutations require X-Soul-Request: 1')

    @staticmethod
    def origin(value):
        url = urllib.parse.urlsplit(value)
        return f'{url.scheme}://{url.netloc}'

    def directory(self, token):
        return self.request('GET', f'/api/apps/{self.service_id}/directory', token)['principals']

    def start(self):
        state, verifier = secrets.token_urlsafe(32), secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
        with self.store.connect() as db:
            db.execute('DELETE FROM oauth WHERE expires <= ?', (time.time(),))
            db.execute('INSERT INTO oauth VALUES(?,?,?)', (digest(state), verifier, time.time() + 600))
        query = urllib.parse.urlencode({'response_type': 'code', 'client_id': self.service_id,
                 'redirect_uri': self.public_url + 'api/auth/callback', 'state': state,
                 'code_challenge': challenge, 'code_challenge_method': 'S256'})
        return self.proxy_url + 'api/apps/oauth/authorize?' + query

    def finish(self, code, state):
        with self.store.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT * FROM oauth WHERE state=? AND expires>?',
                             (digest(state), time.time())).fetchone()
            db.execute('DELETE FROM oauth WHERE state=?', (digest(state),))
        if not row:
            raise Error(401, 'oauth_state_invalid', 'OAuth state invalid or expired')
        result = self.request('POST', '/api/apps/oauth/token', form={
            'grant_type': 'authorization_code', 'code': code, 'client_id': self.service_id,
            'redirect_uri': self.public_url + 'api/auth/callback', 'code_verifier': row['verifier']})
        token = result['access_token']
        principal, _ = self.verify(token)
        if principal['kind'] != 'human':
            raise Error(401, 'oauth_identity_invalid', 'Human OAuth identity required')
        expires = datetime.fromisoformat(result['expires_at'].replace('Z', '+00:00')).timestamp()
        raw = secrets.token_urlsafe(48)
        with self.store.connect() as db:
            db.execute('DELETE FROM sessions WHERE expires<=?', (time.time(),))
            db.execute('INSERT INTO sessions VALUES(?,?,?)', (digest(raw), token, expires))
        return raw, max(0, int(expires - time.time()))

    def session_token(self, raw):
        with self.store.connect() as db:
            row = db.execute('SELECT token FROM sessions WHERE secret=? AND expires>?',
                             (digest(raw), time.time())).fetchone()
        return row['token'] if row else None

    def logout(self, raw):
        with self.store.connect() as db:
            db.execute('DELETE FROM sessions WHERE secret=?', (digest(raw),))
