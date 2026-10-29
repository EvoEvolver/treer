#!/usr/bin/env python3
"""Synthetic `treer` CLI for the Agent Hub tests: a JSON state file, no Core."""
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

    def agent(target):
        a = state['agents'].get(target)
        if not a or a.get('deleted'):
            print('agent_not_found', file=sys.stderr)
            raise SystemExit(save(1))
        return a

    def save(result):
        with open(path, 'w') as f:
            json.dump(state, f)
        return result

    if args[:2] == ['agent', 'admin'] and args[2] == 'create':
        if state.get('fail_create'):
            print('agent_create_denied', file=sys.stderr)
            raise SystemExit(save(1))
        state['next'] += 1
        aid = f"ag_worker{state['next']}"
        sep = args.index('--')
        state['agents'][aid] = {'name': args[args.index('--name') + 1], 'folder': args[args.index('--folder') + 1],
                                'cwd': args[args.index('--cwd') + 1], 'launch': args[sep + 1:], 'status': 'starting',
                                'ready': state.get('ready_on_create', False), 'prompts': [], 'deleted': False}
        out = {'agent_id': aid, 'name': state['agents'][aid]['name'], 'status': 'starting'}
    elif args[:2] == ['agent', 'prompt']:
        target, text = args[2], args[3]
        if target in state['agents']:
            a = agent(target)
            if not a['ready']:
                print('interface_unavailable', file=sys.stderr)
                raise SystemExit(save(1))
            a['prompts'].append({'text': text, 'steer': '--steer' in args})
            a['status'] = 'working'
        else:
            state.setdefault('wakes', []).append({'target': target, 'text': text})
        out = {'submission': {'id': 'sub_%d' % len(state['calls'])}}
    elif args[:2] == ['agent', 'show']:
        a = agent(args[2])
        out = {'agent_id': args[2], 'status': a['status'], 'path': '/agent-hub-workers/' + a['name'],
               'interface': {'origin_url': 'https://ag-' + args[2][3:] + '.apps.example/'}}
    elif args[:2] == ['agent', 'transcript']:
        a = agent(args[2])
        out = {'turns': [{'prompt': p['text'][:40]} for p in a['prompts']], 'thread_id': 'thread-' + args[2]}
    elif args[:3] == ['agent', 'admin', 'delete']:
        agent(args[3])['deleted'] = True
        out = {'deleted': True}
    elif args[:2] == ['agent', 'mkdir']:
        out = {'path': args[-1]}
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
        print('unsupported fake command', file=sys.stderr)
        code = 2
    save(code)
    print(json.dumps(out))
    raise SystemExit(code)
