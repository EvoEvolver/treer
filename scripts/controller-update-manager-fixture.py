#!/usr/bin/env python3
"""Prepare an inert manager topology; never install a service or run an updater.

Actual manager execution is a separate, explicit disposable-account operation.
The heartbeat worker models process placement only, not native update acceptance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def write_new(path, value):
    with path.open('x', encoding='utf-8') as stream:
        os.chmod(path, 0o600)
        json.dump(value, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())


def checked_root(value):
    root = Path(value)
    if not root.is_absolute() or root.is_symlink() or root.resolve() != root:
        raise ValueError('fixture_root_must_be_absolute_without_aliases')
    # Work only in a new private directory under the OS temporary directory.
    if not root.is_relative_to(Path(tempfile.gettempdir()).resolve()) or root == Path(tempfile.gettempdir()).resolve():
        raise ValueError('fixture_root_must_be_private_temporary_directory')
    return root


def prepare(root, manager):
    root = checked_root(root)
    root.mkdir(mode=0o700)  # exclusive: never adopt or overwrite another run
    script = Path(__file__).resolve()
    label = 'treer.update-fixture.' + uuid.uuid4().hex
    argv = [str(Path(sys.executable).resolve()), str(script), 'host', '--root', str(root), '--disposable']
    plan = {'schema': 1, 'scope': 'inert topology only; no updater or service execution by prepare',
            'uid': os.getuid(), 'manager': manager, 'label': label,
            'script_sha256': hashlib.sha256(script.read_bytes()).hexdigest(),
            'host_argv': argv, 'lifetime_seconds': 90,
            'replace_request': str(root / 'replace-controller'),
            'stop_request': str(root / 'stop'), 'manager_acceptance': 'not_run',
            'native_update_acceptance': 'not_run'}
    if manager == 'systemd':
        plan['manual_start_argv'] = ['systemd-run', '--user', '--collect', '--unit=' + label,
                                     '--property=KillMode=control-group', '--'] + argv
        plan['manual_stop_argv'] = ['systemctl', '--user', 'stop', label + '.service']
        plan['manual_inspect_argv'] = ['systemctl', '--user', 'show', label + '.service', '-p', 'ControlGroup', '-p', 'MainPID', '-p', 'KillMode']
    else:
        plist = root / (label + '.plist')
        # No KeepAlive, RunAtLoad, global domain or service registration.
        with plist.open('xb') as stream:
            os.chmod(plist, 0o600)
            plistlib.dump({'Label': label, 'ProgramArguments': argv}, stream)
        domain = 'gui/' + str(os.getuid())
        plan['manual_start_argv'] = ['launchctl', 'bootstrap', domain, str(plist)]
        plan['manual_kickstart_argv'] = ['launchctl', 'kickstart', domain + '/' + label]
        plan['manual_stop_argv'] = ['launchctl', 'bootout', domain + '/' + label]
        plan['manual_inspect_argv'] = ['launchctl', 'print', domain + '/' + label]
    write_new(root / 'plan.json', plan)
    return plan


def internal(root, role):
    # Inert children perform no external command, read no Treer identity and
    # write only fixed files below their prepared private fixture directory.
    write_new(root / (role + '.json'), {'pid': os.getpid(), 'parent': os.getppid(), 'session': os.getsid(0)})
    if role == 'controller':
        worker = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'worker', '--root', str(root)],
                                  start_new_session=True, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={})
        # Deliberately no kill-on-drop: test the detached child lifetime.
        del worker
    deadline = time.monotonic() + 90
    while not (root / 'stop').exists() and time.monotonic() < deadline:
        if role == 'worker':
            (root / 'heartbeat').write_text(str(time.monotonic()), encoding='ascii')
        time.sleep(0.05)
    if role == 'worker':
        write_new(root / 'worker-done.json', {'cooperative_exit': True})


def host(root, disposable):
    plan = json.loads((root / 'plan.json').read_text())
    if not disposable or os.getuid() == 0 or os.environ.get('TREER_AGENT_ID'):
        raise ValueError('requires_explicit_disposable_nonroot_unmanaged_account')
    if plan['uid'] != os.getuid() or hashlib.sha256(Path(__file__).read_bytes()).hexdigest() != plan['script_sha256']:
        raise ValueError('fixture_binding_changed')
    if sys.platform not in ('linux', 'darwin') or (plan['manager'] == 'systemd') != sys.platform.startswith('linux'):
        raise ValueError('fixture_platform_mismatch')
    controller = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'controller', '--root', str(root)],
                                  start_new_session=True, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env={})
    replaced = False
    deadline = time.monotonic() + 90
    try:
        while not (root / 'stop').exists() and time.monotonic() < deadline:
            if not replaced and (root / 'replace-controller').exists():
                # Signal only the retained owned Controller's private group.
                # Never recover a PID/group from a file to signal it.
                if controller.poll() is not None:
                    raise ValueError('controller_exited_before_owned_replace')
                os.killpg(controller.pid, signal.SIGKILL)
                controller.wait(timeout=5)
                replaced = True
                first = float((root / 'heartbeat').read_text())
                time.sleep(0.25)
                second = float((root / 'heartbeat').read_text())
                write_new(root / 'replacement-result.json', {
                    'worker_heartbeat_advanced': second > first,
                    'scope': 'inert owned-controller group kill; not native activation or manager-stop proof'})
            time.sleep(0.05)
    finally:
        (root / 'stop').touch(exist_ok=True)
        if controller.poll() is None:
            os.killpg(controller.pid, signal.SIGKILL)
        controller.wait(timeout=5)
        # The orphan worker exits cooperatively or at its absolute90s lifetime.
        # Neither timeout nor a heartbeat is a descendant-quiescence witness.
        # Manager-stop cleanup must be independently observed in the disposable
        # account/cgroup; do not kill a saved PID or label cleanup complete here.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'host', 'controller', 'worker'])
    parser.add_argument('--root', required=True)
    parser.add_argument('--manager', choices=['systemd', 'launchd'])
    parser.add_argument('--disposable', action='store_true')
    args = parser.parse_args()
    root = checked_root(args.root)
    if args.action == 'prepare':
        if args.manager is None:
            parser.error('prepare requires --manager')
        print(json.dumps(prepare(root, args.manager), indent=2))
    elif args.action == 'host':
        host(root, args.disposable)
    else:
        internal(root, args.action)


if __name__ == '__main__':
    main()
