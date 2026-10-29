#!/usr/bin/env python3
"""Synthetic `treer` CLI for the Agent Hub tests: a JSON state file, no Core.

Workers are the hub's own child processes, so the hub only uses the CLI for
Core Messages, requester wake-ups, discovery and deleting a legacy worker Agent.
"""
import fcntl
import json
import os
import sys

path = os.environ['FAKE_TREER_STATE']
args = sys.argv[1:]
with open(path + '.lock', 'w') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    with open(path) as f:
        state = json.load(f)
    stdin = sys.stdin.read() if '--stdin-envelope' in args else None
    state['calls'].append(args)
    out, code = {}, 0

    def save(result):
        with open(path, 'w') as f:
            json.dump(state, f)
        return result

    if args[:2] == ['agent', 'prompt']:
        state.setdefault('wakes', []).append({'target': args[2], 'text': args[3]})
        out = {'submission': {'id': 'sub_%d' % len(state['calls'])}}
    elif args[:3] == ['agent', 'admin', 'delete']:
        state.setdefault('deleted', []).append(args[3])
        out = {'deleted': True}
    elif args[:2] == ['message', 'send']:
        if state.get('fail_send'):
            print('message_send_failed', file=sys.stderr)
            raise SystemExit(save(1))
        state['sent'].append({'args': args, 'body': json.loads(stdin)['body'] if stdin else None})
        out = {'message': {'message_id': 'msg_out_%d' % len(state['sent'])}}
    elif args[:2] == ['message', 'receive']:
        out = {'deliveries': [d for d in state['inbox'] if d['delivery_id'] not in state['acked']][:20]}
    elif args[:2] == ['message', 'ack']:
        state['acked'].append(args[2])
        out = {'acknowledged': [args[2]]}
    elif args == ['whoami']:
        out = {'machine': {'root': state['root']}}
    elif args[:2] == ['app', 'list']:
        out = {'apps': []}
    else:
        print('unsupported fake command: ' + ' '.join(args[:3]), file=sys.stderr)
        code = 2
    save(code)
    print(json.dumps(out))
    raise SystemExit(code)
