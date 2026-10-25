#!/usr/bin/env python3
"""Prepare a fresh private relay launch bundle. Read-only Core discovery; never send mail."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import stat
import subprocess
import sys
import time


def absolute(value):
    p = Path(value)
    if not p.is_absolute() or p.resolve() != p or '..' in p.parts:
        raise ValueError('absolute_physical_path_required')
    return p


def binary(path, expected):
    p = absolute(path)
    st = p.lstat()
    if not stat.S_ISREG(st.st_mode) or not os.access(p, os.X_OK) or st.st_size > 64*1024*1024:
        raise ValueError('invalid_binary')
    if not re.fullmatch('[0-9a-f]{64}', expected) or hashlib.sha256(p.read_bytes()).hexdigest() != expected:
        raise ValueError('binary_pin_mismatch')
    return p


def metadata(cli, target, env, timeout=10):
    """Bound time and memory even when the trusted CLI's Controller is unavailable."""
    with subprocess.Popen([str(cli), '--format', 'json', 'agent', 'show', '--', target],
                          env=env, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
        raw = bytearray()
        deadline = time.monotonic() + timeout
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise ValueError('metadata_timeout')
                    chunk = os.read(process.stdout.fileno(), 65536)
                    if not chunk:
                        break
                    raw.extend(chunk)
                    if len(raw) > 262144:
                        raise ValueError('metadata_too_large')
                if process.wait(timeout=max(0.001, deadline - time.monotonic())):
                    raise ValueError('metadata_unavailable')
        except BaseException:
            process.kill()
            process.wait()
            raise
    return bytes(raw)


def prepare(args):
    ids = [args.hub, args.endpoint, args.recipient, args.workspace]
    if not (all(re.fullmatch('(ag|app)_[A-Za-z0-9_-]+', x) for x in ids[:3])
            and re.fullmatch('ws_[A-Za-z0-9_-]+', args.workspace)):
        raise ValueError('invalid_identity')
    if not args.endpoint.startswith('ag_') or len(set(ids[:3])) != 3 or args.generation < 1:
        raise ValueError('invalid_binding')
    cli = binary(args.cli, args.cli_sha256)
    relay = binary(args.relay, args.relay_sha256)
    # Same managed identity/Policy as the caller; no profile or operator fallback.
    env = {k: v for k, v in os.environ.items() if k in
           ('PATH', 'HOME', 'TMPDIR', 'TREER_AGENT_ID', 'TREER_WORKSPACE_ID',
            'TREER_SERVER_ID', 'TREER_AGENT_SERVER_URL', 'TREER_WORKLOAD_CREDENTIAL')}
    if env.get('TREER_WORKSPACE_ID') != args.workspace or not env.get('TREER_AGENT_ID'):
        raise ValueError('managed_workspace_required')
    env['TREER_IDENTITY_MODE'] = 'agent'
    objects = []
    for target in ids[:3]:
        raw = metadata(cli, target, env)
        obj = json.loads(raw)
        if obj.get('agent_id') != target or obj.get('workspace_id') != args.workspace:
            raise ValueError('metadata_identity_mismatch')
        if not isinstance(obj.get('cwd'), str) or not re.fullmatch('srv_[A-Za-z0-9_-]+', obj.get('server_id', '')):
            raise ValueError('metadata_placement_required')
        objects.append({k: obj[k] for k in ('agent_id','workspace_id','server_id','kind','path','cwd','status')})
    hub, endpoint, recipient = objects
    if hub.get('kind') != 'app' or endpoint.get('kind') != 'command' or not endpoint.get('path'):
        raise ValueError('managed_hub_and_placed_command_relay_required')
    if endpoint.get('status') not in ('exited', 'stopped'):
        raise ValueError('relay_must_be_stopped_for_fresh_setup')
    if endpoint['server_id'] != env.get('TREER_SERVER_ID'):
        raise ValueError('setup_must_run_on_relay_machine')
    # Do not silently adopt/rebind old state or set startup on another Agent.
    out = absolute(args.output)
    parent = out.parent
    ps = parent.stat()
    if ps.st_uid != os.getuid() or stat.S_IMODE(ps.st_mode) != 0o700:
        raise ValueError('private_parent_required')
    if any((p / '.git').exists() for p in [parent, *parent.parents]):
        raise ValueError('bundle_must_be_outside_repository')
    out.mkdir(mode=0o700)  # exclusive; existing bundle is never replayed/overwritten
    def write(name, value):
        with os.fdopen(os.open(out / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as f:
            json.dump(value, f, indent=2)
            f.write('\n')
            f.flush()
            os.fsync(f.fileno())
    config = dict(endpoint_id=args.endpoint, hub_id=args.hub, generation=args.generation,
                  recipients=[args.recipient], cli=str(cli), cli_sha256=args.cli_sha256,
                  release_sha256=args.relay_sha256, state_directory=str(out / 'state'))
    write('relay.json', config)
    write('launch.json', {'required_agent_id': args.endpoint, 'workspace_id': args.workspace,
                         'server_id': endpoint['server_id'], 'argv':[str(relay), '--config', str(out/'relay.json')],
                         'cwd':endpoint['cwd'],
                         'startup_argv':[str(cli),'agent','admin','startup','set',args.endpoint,
                                         '--cwd',endpoint['cwd'],'--',str(relay),'--config',str(out/'relay.json')],
                         'start_argv':[str(cli),'agent','start',args.endpoint],
                         'startup_registration':'authorized admin startup set preserves stopped intent; agent start launches as the relay identity'})
    write('preview-create.json', dict(action='preview_create', endpoint_id=args.endpoint,
                                    generation=args.generation, recipient=args.recipient))
    write('readback.json', {'workspace_id':args.workspace,'hub':hub,'endpoint':endpoint,
                          'recipient':recipient,'contact_required':True,'channel_activation':False,
                          'operations_performed':['three agent show reads'],
                          'next':'Start relay only under its own identity with launch.json; owner POST preview-create.json, confirm returned preview/create paused binding, then choose member/address in Hub. No mail is sent while Hub activation is false.'})
    fd = os.open(out, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return {'status':'prepared','bundle':str(out),'endpoint_id':args.endpoint,
            'workspace_id':args.workspace,'relay_server_id':endpoint['server_id'],
            'mutations':0,'channel_activation':False}


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('hub','endpoint','recipient','workspace','cli','cli-sha256','relay','relay-sha256','output'):
        p.add_argument('--'+name, required=True)
    p.add_argument('--generation', type=int, default=1)
    return p


if __name__ == '__main__':
    try:
        print(json.dumps(prepare(parser().parse_args())))
    except Exception:
        print('email_relay_setup_refused; inspect input identity, stopped ownership and private paths', file=sys.stderr)
        sys.exit(1)
