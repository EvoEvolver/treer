#!/usr/bin/env python3
"""Bounded client for a preprovisioned real Treer fixture. Plan mode never reads secrets."""
import argparse
import hashlib
import http.client
import ipaddress
import json
import os
from pathlib import Path
import re
import stat
import sys
from urllib.parse import urlsplit

KEYS = {'public_url', 'owner_url', 'workspace', 'foreign_workspace', 'app', 'agent',
        'allowed', 'denied', 'foreign', 'generation', 'decision', 'policy_evidence_sha256',
        'token_file', 'owner_headers_file'}
ID = re.compile(r'[A-Za-z0-9_-]{1,128}\Z')
TOKEN = re.compile(r'ext1_[0-9a-f]{32}\.[0-9a-f]{64}\Z')


def validate(m):
    if set(m) != KEYS:
        raise ValueError('manifest fields differ from the fixed acceptance contract')
    for key in ['workspace', 'foreign_workspace', 'app', 'agent', 'allowed', 'denied', 'foreign', 'generation']:
        if not isinstance(m[key], str) or not ID.fullmatch(m[key]):
            raise ValueError('invalid fixed identity')
    if m['workspace'] == m['foreign_workspace'] or len({m['allowed'], m['denied'], m['foreign']}) != 3:
        raise ValueError('targets and workspaces must be distinct')
    if not re.fullmatch('[0-9a-f]{64}', m['policy_evidence_sha256']) or not m['decision'].strip():
        raise ValueError('real Policy evidence pin and concrete execution decision required')
    for key in ['public_url', 'owner_url']:
        u = urlsplit(m[key])
        validate_origin(u, allow_owner_prefix=key == 'owner_url')
        if key == 'owner_url' and u.path not in ('', '/') and u.path.rstrip('/') != (
                '/api/workspaces/'+m['workspace']+'/apps/'+m['app']+'/ui'):
            raise ValueError('owner embedded prefix must bind the exact workspace and App')
    if origin_header(m['public_url']) == origin_header(m['owner_url']):
        raise ValueError('public and management origins must be distinct')
    for key in ['token_file', 'owner_headers_file']:
        if not Path(m[key]).is_absolute():
            raise ValueError('private credential paths must be absolute')
    return hashlib.sha256(json.dumps(m, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def validate_origin(u, allow_owner_prefix=False):
    prefix = re.fullmatch(r'/api/workspaces/[A-Za-z0-9_-]{1,128}/apps/[A-Za-z0-9_-]{1,128}/ui/?', u.path)
    if (not u.hostname or u.username or u.password or u.query or u.fragment
            or (u.path not in ('', '/') and not (allow_owner_prefix and prefix))):
        raise ValueError('invalid fixed origin')
    if u.port == 0:
        raise ValueError('port zero is not a bound fixture')
    if u.scheme == 'https':
        return
    if u.scheme == 'http':
        try:
            if ipaddress.ip_address(u.hostname).is_loopback:
                return
        except ValueError:
            pass
    raise ValueError('HTTPS or literal loopback HTTP required')


def origin_header(url):
    u = urlsplit(url)
    return u.scheme+'://'+u.netloc


def private_text(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'r') as f:
        s = os.fstat(f.fileno())
        if not stat.S_ISREG(s.st_mode) or s.st_uid != os.geteuid() or s.st_mode & 0o077 or s.st_size > 16384:
            raise ValueError('credential file must be private, owned, regular and bounded')
        result = f.read(16385)
        if len(result) > 16384:
            raise ValueError('credential file too large')
        return result


def request(origin, method, path, headers, body=None):
    # No environment proxies, redirects, retries, URLs from responses, or body logging.
    u = urlsplit(origin)
    validate_origin(u, allow_owner_prefix=True)
    if not path.startswith('/') or path.startswith('//'):
        raise ValueError('request path must be fixed and absolute')
    target = u.path.rstrip('/')+path
    client = http.client.HTTPConnection if u.scheme == 'http' else http.client.HTTPSConnection
    connection = client(u.hostname, u.port, timeout=10)
    try:
        data = None if body is None else json.dumps(body).encode()
        connection.request(method, target, data, dict(headers, **({'Content-Type': 'application/json'} if data is not None else {})))
        response = connection.getresponse()
        raw = response.read(1048577)
        if len(raw) > 1048576:
            raise ValueError('oversized response')
        return response.status, json.loads(raw)
    finally:
        connection.close()


def execute(m):
    # Exactly nine requests, sequential, fail at first mismatch; no activation or grant mutation.
    validate(m)
    token = private_text(m['token_file']).strip()
    if not TOKEN.fullmatch(token):
        raise ValueError('invalid fixture token')
    token_id = token[5:37]
    owner = json.loads(private_text(m['owner_headers_file']))
    if not isinstance(owner, dict) or not owner or not set(owner) <= {'Cookie', 'X-Treer-Identity-Token'}:
        raise ValueError('only preprovisioned human ingress credentials accepted')
    if any(not isinstance(v, str) or not v or not all(32 <= ord(c) <= 126 for c in v) for v in owner.values()):
        raise ValueError('invalid human ingress credentials')
    owner.update({'Origin': origin_header(m['owner_url']), 'Sec-Fetch-Site': 'same-origin', 'X-External-Agent-CSRF': '1'})
    external = {'Authorization': 'Bearer '+token}
    identity = {'workspace': m['workspace'], 'app': m['app'], 'agent': m['agent']}
    def own(method, path, body=None):
        status, value = request(m['owner_url'], method, path, owner, body)
        if status != 200:
            raise ValueError('fresh owner request rejected')
        return value
    def audit():
        v = own('GET', '/owner/audit')
        if any(v.get('identity', {}).get(k) != val for k, val in identity.items()):
            raise ValueError('audit workload identity mismatch')
        return v['requests']
    state = own('GET', '/owner/state')
    if state.get('identity') != identity or state.get('generation') != m['generation'] or state.get('enabled') is not True:
        raise ValueError('fixture identity/generation/activation mismatch')
    before = {r['request_id'] for r in audit()}
    cases = [('GET', m['allowed'], '', 200, None),
             ('GET', m['denied'], '/transcript', 403, 'policy_denied'),
             ('POST', m['denied'], '/stop', 403, 'policy_denied'),
             ('GET', m['foreign'], '/transcript', 503, 'unavailable')]
    request_ids = set()
    for method, target, suffix, expected, code in cases:
        status, value = request(m['public_url'], method, '/v1/agents/'+target+suffix, external, {} if method == 'POST' else None)
        request_id = value.get('request_id')
        if not isinstance(request_id, str) or not ID.fullmatch(request_id) or request_id in request_ids:
            raise ValueError('missing or duplicate durable request identity')
        request_ids.add(request_id)
        value = value.get('result', {})
        if status != expected or (code and value.get('error', {}).get('code') != code):
            raise ValueError('real boundary response mismatch; stop without retry')
        if not suffix and value.get('agent_id') != target:
            raise ValueError('allowed target mismatch')
    rows = [r for r in audit() if r['request_id'] not in before]
    expected = {('status', m['allowed'], 'completed'), ('transcript', m['denied'], 'policy_denied'),
                ('stop', m['denied'], 'policy_denied'), ('transcript', m['foreign'], 'unavailable')}
    if {r['request_id'] for r in rows} != request_ids or len(rows) != 4 or any(r['token'] != token_id for r in rows) or {(r['operation'], r['target'], r['outcome']) for r in rows} != expected:
        raise ValueError('exact new execution audit mismatch')
    own('POST', '/owner/revoke', {'id': token_id})
    status, value = request(m['public_url'], 'GET', '/v1/agents/'+m['allowed'], external)
    if status != 401 or value.get('error', {}).get('code') != 'authentication_required':
        raise ValueError('fresh revoked request was not rejected')
    # Nine requests above; independent Core non-execution/browser/stop/expiry checks remain separate.
    return {'client_matrix': 'PASS', 'real_provider_provenance': 'EXTERNAL EVIDENCE REQUIRED',
            'foreign_nonexecution': 'EXTERNAL EVIDENCE REQUIRED', 'requests': 9,
            'browser_expiry_independent_stop': 'NOT RUN'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--execute', metavar='MANIFEST_SHA256', help='explicitly run nine requests within the separately approved fixture scope')
    args = parser.parse_args()
    try:
        m = json.loads(args.manifest.read_text())
        digest = validate(m)
        if args.execute is None:
            print(json.dumps({'manifest_sha256': digest, 'status': 'NOT RUN', 'max_requests': 9,
                              'secrets_read': False, 'resources_created': False}))
        else:
            if args.execute != digest:
                raise ValueError('execution scope hash mismatch')
            print(json.dumps(execute(m)))
    except Exception:
        # Never serialize responses, secret files, URLs, or library exception details.
        print('acceptance stopped: invalid scope, transport, authentication or boundary result', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
