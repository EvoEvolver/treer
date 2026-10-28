#!/usr/bin/env python3
"""Incremental real-fixture client; no bootstrap, token issuance, enable or retries."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

import acceptance as base


def validate(m):
    if set(m) != base.KEYS | {'expected_output_sha256'}:
        raise ValueError('fixed output scope required')
    if not isinstance(m['expected_output_sha256'], str) or not re.fullmatch(
            '[0-9a-f]{64}', m['expected_output_sha256']):
        raise ValueError('output pin required')
    base.validate({k: v for k, v in m.items() if k in base.KEYS})
    return hashlib.sha256(json.dumps(m, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def execute(m):
    validate(m)
    token = base.private_text(m['token_file']).strip()
    if not base.TOKEN.fullmatch(token):
        raise ValueError('invalid fixture token')
    owner = json.loads(base.private_text(m['owner_headers_file']))
    if (not isinstance(owner, dict) or not owner
            or not set(owner) <= {'Cookie', 'X-Treer-Identity-Token'}
            or any(not isinstance(v, str) or not v
                   or not all(32 <= ord(c) <= 126 for c in v) for v in owner.values())):
        raise ValueError('invalid owner headers')
    owner.update({'Origin': base.origin_header(m['owner_url']), 'Sec-Fetch-Site': 'same-origin',
                  'X-External-Agent-CSRF': '1'})
    external = {'Authorization': 'Bearer '+token}
    identity = {k: m[k] for k in ('workspace', 'app', 'agent')}

    def own(method, path, body=None):
        status, value = base.request(m['owner_url'], method, path, owner, body)
        if status != 200:
            raise ValueError('owner request refused')
        return value

    def state(enabled):
        value = own('GET', '/owner/state')
        if (value.get('identity') != identity or value.get('generation') != m['generation']
                or value.get('enabled') is not enabled):
            raise ValueError('state mismatch')

    def audit():
        value = own('GET', '/owner/audit')
        if (not isinstance(value, dict)
                or any(value.get('identity', {}).get(k) != v for k, v in identity.items())
                or not isinstance(value.get('requests'), list)
                or len(value['requests']) > 100):
            raise ValueError('audit identity mismatch')
        return value['requests']

    state(True)
    before = {r['request_id'] for r in audit()}
    ids = set()
    for target, expected_status, outcome in (
            (m['allowed'], 200, 'completed'), (m['denied'], 403, 'policy_denied')):
        status, value = base.request(m['public_url'], 'GET', '/v1/agents/'+target+'/output', external)
        request_id = value.get('request_id')
        if (status != expected_status or not isinstance(request_id, str)
                or not base.ID.fullmatch(request_id) or request_id in ids):
            raise ValueError('output boundary mismatch')
        ids.add(request_id)
        result = value.get('result', {})
        if outcome == 'completed':
            if (result.get('agent_id') != target or not isinstance(result.get('text'), str)
                    or type(result.get('revision')) is not int or result['revision'] < 0
                    or type(result.get('truncated')) is not bool or result['truncated']
                    or hashlib.sha256(result['text'].encode()).hexdigest() != m['expected_output_sha256']):
                raise ValueError('output identity or content mismatch')
        elif result.get('error', {}).get('code') != 'policy_denied':
            raise ValueError('Policy denial mismatch')
    rows = [r for r in audit() if r['request_id'] not in before]
    if (len(rows) != 2 or {r['request_id'] for r in rows} != ids
            or any(r['token'] != token[5:37] or r['operation'] != 'output' for r in rows)
            or {(r['target'], r['outcome']) for r in rows}
            != {(m['allowed'], 'completed'), (m['denied'], 'policy_denied')}):
        raise ValueError('output audit mismatch')
    disabled = own('POST', '/owner/activation', {
        'enabled': False, 'generation': m['generation'], 'agent': m['agent'], 'policy_reviewed': False})
    if disabled.get('enabled') is not False:
        raise ValueError('disable outcome uncertain')
    state(False)
    status, value = base.request(m['public_url'], 'GET', '/v1/agents/'+m['allowed']+'/output', external)
    if status != 503 or value.get('error', {}).get('code') != 'disabled':
        raise ValueError('disabled output not refused')
    # Refusal before admission must not create an execution row.
    final_rows = audit()
    if len(final_rows) != len(before) + 2 or {r['request_id'] for r in final_rows} != before | ids:
        raise ValueError('unexpected disabled dispatch')
    return {'output_containment': 'PASS', 'requests': 9, 'enabled': False,
            'output_body_recorded': False, 'independent_stop': 'EXECUTION_OWNER_REQUIRED'}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('manifest', type=Path)
    p.add_argument('--execute', metavar='MANIFEST_SHA256')
    args = p.parse_args()
    try:
        m = json.loads(args.manifest.read_text())
        digest = validate(m)
        if args.execute is None:
            print(json.dumps({'status': 'NOT_RUN', 'manifest_sha256': digest,
                              'max_requests': 9, 'secrets_read': False}))
        elif args.execute != digest:
            raise ValueError('scope mismatch')
        else:
            print(json.dumps(execute(m)))
    except Exception:
        print('output containment stopped; no request retried', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
