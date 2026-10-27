#!/usr/bin/env python3
"""Finite native CLI probe launched ONLY as a fresh fixture-owned Agent."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', required=True)
    parser.add_argument('--target', required=True)
    parser.add_argument('--owner-id', required=True)
    parser.add_argument('--endpoint', default='team/tools/agent1')
    parser.add_argument('--output', required=True)
    args = parser.parse_args()
    os.umask(0o077)
    result = {'passed': False, 'stage': 'managed_identity', 'checks': []}
    try:
        if not all(os.environ.get(k) for k in ('TREER_AGENT_ID', 'TREER_WORKLOAD_CREDENTIAL', 'TREER_AGENT_SERVER_URL')):
            raise RuntimeError('managed_identity_missing')
        owner, endpoint = args.owner_id, args.endpoint

        def run(argv, body=None, success=True):
            response = subprocess.run([args.cli] + argv, input=None if body is None else json.dumps({'body': body}),
                                      text=True, capture_output=True, timeout=15)
            if (response.returncode == 0) != success:
                raise RuntimeError('unexpected_cli_status')
            return json.loads(response.stdout) if success else None

        def send(target, key, body):
            return run(['message', 'send', '--to', target, '--idempotency-key', key, '--stdin-envelope'], body)

        def reply_to(mid, expected):
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                incoming = run(['message', 'receive', '--wait', '1000', '--limit', '20'])
                for delivery in incoming['deliveries']:
                    message = delivery['message']
                    if mid not in message['context_ids']:
                        continue
                    if message['sender']['id'] != owner or message['sender'].get('app_endpoint') != expected:
                        raise RuntimeError('reply_identity_mismatch')
                    run(['message', 'ack', delivery['delivery_id'], '--operation-id', 'probe-ack-' + delivery['delivery_id']])
                    return message
            raise RuntimeError('reply_deadline')

        result['stage'] = 'send'
        first = send(args.target, 'cli-probe-230-first', 'native-cli-230')
        replay = send(args.target, 'cli-probe-230-first', 'native-cli-230')
        if first['message']['message_id'] != replay['message']['message_id'] or not replay['idempotent_replay']:
            raise RuntimeError('retry_not_same_message')
        result['checks'].append('actual managed CLI nested send and identical idempotent retry')
        result['stage'] = 'child_reply'
        reply = reply_to(first['message']['message_id'], endpoint)
        if reply['body'] != '[app-hub:reply:v1] [' + endpoint + '] native-cli-230':
            raise RuntimeError('handler_reply_mismatch')
        result['checks'].append('actual App handler explicit authenticated owner plus child reply')
        result['stage'] = 'reply_address'
        follow = run(['message', 'reply', reply['message_id'], '--idempotency-key', 'cli-probe-230-follow', '--body', 'native-followup'])
        if follow['message']['recipients'][0].get('app_endpoint') != endpoint:
            raise RuntimeError('reply_lost_subaddress')
        result['checks'].append('actual CLI reply preserves full nested logical sender address')
        result['stage'] = 'invalid_sender'
        run(['message', 'send', '--to', args.target, '--from-endpoint', endpoint,
             '--idempotency-key', 'cli-probe-230-spoof', '--stdin-envelope'], 'spoof', success=False)
        result['checks'].append('ordinary Agent cannot assert an App logical sender')
        result['stage'] = 'unknown_endpoint'
        missing = send(owner + '/team/tools/missing', 'cli-probe-230-missing', 'missing')
        refused = reply_to(missing['message']['message_id'], 'team/tools/missing')
        if not refused['body'].startswith('[app-hub:reply:v1] [app_endpoint_not_found]'):
            raise RuntimeError('missing_handler_not_explicit')
        result['checks'].append('unknown nested endpoint has explicit App failure without root fallback')
        result.update(passed=True, stage='complete', sender_id=os.environ['TREER_AGENT_ID'])
    except Exception as error:
        result['failure_type'] = type(error).__name__
    output = Path(args.output)
    with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as stream:
        json.dump(result, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
