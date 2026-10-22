"""Bounded read-only projection for NEW synthetic Docker fixtures, never hosts."""
import hashlib
import json
import os
from pathlib import Path
import pwd
import socket
import stat
import sys
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def read_private(file, home):
    if file.is_symlink() or file.resolve() != file or not file.is_relative_to(home):
        raise ValueError('fixture file alias')
    fd = os.open(file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid() or metadata.st_size > 65536:
            raise ValueError('unsafe fixture file')
        raw = os.read(fd, 65537)
        if len(raw) > 65536 or os.fstat(fd).st_mtime_ns != metadata.st_mtime_ns:
            raise ValueError('changed or large fixture file')
        return raw
    finally:
        os.close(fd)


def projection(home, workspace, server):
    config_dir = home / '.config/treer/agent-servers'
    configs = list(config_dir.glob('*-controller.json'))
    if len(configs) != 1:
        raise ValueError('ambiguous fixture registration')
    config_path = configs[0]
    host_path = config_path.with_name(config_path.name.replace('-controller.json', '-host.json'))
    files = [config_path, host_path]
    identities = list((home / '.local/state/treer/machines').glob('*/machine-identity.json'))
    if len(identities) != 1:
        raise ValueError('ambiguous fixture machine identity')
    files += identities
    hashes = {}
    for i, file in enumerate(files):
        raw = read_private(file, home)
        hashes[str(i)] = hashlib.sha256(raw).hexdigest()
        if i == 0:
            config = json.loads(raw)
    if config['workspace'] != workspace or config['server_id'] != server or config['service_manager'] != 'nohup':
        raise ValueError('fixture registration mismatch')
    address = Path(config['host_socket'])
    if not address.is_absolute():
        raise ValueError('fixture socket mismatch')
    reachable = False
    with socket.socket(socket.AF_UNIX) as probe:
        probe.settimeout(1)
        try:
            probe.connect(str(address))
            reachable = True
        except OSError:
            pass
    record = home / '.local/state/treer' / f'agent-host-{server}.pid'
    identity = None
    matches = False
    if record.is_file() and not record.is_symlink():
        import subprocess
        data = json.loads(read_private(record, home))
        pid = data['pid']
        if not isinstance(pid, int) or pid <= 0:
            raise ValueError('invalid fixture pid')
        out = subprocess.run(['/bin/ps', '-p', str(pid), '-o', 'lstart='], capture_output=True, text=True, timeout=2)
        current = out.stdout.strip()
        matches = out.returncode == 0 and bool(current) and current == data['started_at']
        if matches:
            # Linux kernel creation ticks supplement the product's ps birth pin.
            ticks = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
            image = os.readlink(f'/proc/{pid}/exe')
            if Path(image).name != 'treer-agent-host':
                raise ValueError('fixture Host image mismatch')
            identity = {'pid': pid, 'birth_ticks': ticks}
    connected = False
    if reachable:
        host, port = config['listen'].rsplit(':', 1)
        if host not in ('127.0.0.1', '0.0.0.0') or not 0 < int(port) <= 65535:
            raise ValueError('fixture health address mismatch')
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
            with opener.open(f'http://127.0.0.1:{int(port)}/api/health', timeout=2) as response:
                raw = response.read(65537)
                if len(raw) > 65536:
                    raise ValueError('large fixture health')
                health = json.loads(raw)
            connected = health.get('workspace_id') == workspace and health.get('server_id') == server and health.get('proxy_connected') is True
        except (OSError, ValueError):
            pass
    return dict(workspace_id=workspace, server_id=server, registration_hashes=hashes,
                service_manager=config['service_manager'], credentials_present=bool(config.get('operator_credential')) and bool(config.get('machine_token')),
                host_reachable=reachable, host_record_matches_process=matches,
                host_identity=identity, proxy_connected=connected)


if __name__ == '__main__':
    if os.environ.get('TREER_SYNTHETIC_RECOVERY_226') != '1' or not Path('/.dockerenv').is_file() or len(sys.argv) != 3:
        raise SystemExit('synthetic Docker recovery fixture only')
    home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
    if Path(os.environ.get('HOME', '')).resolve() != home.resolve():
        raise SystemExit('fixture HOME mismatch')
    try:
        print(json.dumps(projection(home.resolve(), sys.argv[1], sys.argv[2])))
    except Exception:
        raise SystemExit('fixture projection unavailable') from None
