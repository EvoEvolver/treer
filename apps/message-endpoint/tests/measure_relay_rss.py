#!/usr/bin/env python3
"""Local release-relay measurement; synthetic CLI, not a native full-system gate."""
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time


def processes():
    # Numeric process accounting only: never inspect argv or environment.
    output = subprocess.check_output(
        ['/bin/ps', '-axo', 'pid=,ppid=,rss='],
        env={'PATH': '/usr/bin:/bin'}, text=True,
    )
    return {int(pid): (int(parent), int(rss))
            for pid, parent, rss in (line.split() for line in output.splitlines())}


def measure(binary):
    binary = binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='endpoint-rss-') as temporary:
        root = Path(temporary).resolve()
        root.chmod(0o700)
        fake = root / 'fake-treer'
        fake.write_text('''#!/usr/bin/python3
import json,sys,time
from pathlib import Path
args=sys.argv[1:]
root=Path(__file__).parent
principal={'kind':'agent','id':'ag_endpoint','name':'Synthetic endpoint'}
if args[:2]==['message','send']:
    assert '--to=app_hub' in args
    envelope=json.load(sys.stdin)
    body=json.loads(envelope['body'])
    assert body['protocol']=='treer.endpoint.status/v1'
    (root/'startup').touch()
    result={'message':{'message_id':'msg_startup','sender':principal,'body':envelope['body']}}
elif args[:2]==['message','receive']:
    assert args[2:]==['--wait=1000','--limit=10']
    (root/'waiting').touch()
    time.sleep(1)
    result={'remaining_unacknowledged':0,'deliveries':[]}
else:raise AssertionError('unexpected CLI operation')
print(json.dumps(result))
''')
        fake.chmod(0o700)
        digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
        config = root / 'endpoint.json'
        config.write_text(json.dumps({
            'endpoint_id': 'ag_endpoint', 'hub_id': 'app_hub', 'generation': 1,
            'recipients': ['ag_target'], 'cli': str(fake),
            'state_directory': str(root / 'state'),
            'release_sha256': digest(binary), 'cli_sha256': digest(fake),
        }))
        config.chmod(0o600)
        environment = {'HOME': str(root), 'TMPDIR': str(root),
                       'PATH': '/usr/bin:/bin', 'TREER_AGENT_ID': 'ag_endpoint'}
        with tempfile.TemporaryFile() as errors:
            child = subprocess.Popen([str(binary), '--config', str(config)], cwd=root,
                                     env=environment, stdout=subprocess.DEVNULL, stderr=errors)
            try:
                deadline = time.monotonic() + 10
                while not (root / 'waiting').exists():
                    if child.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('synthetic_relay_did_not_start')
                    time.sleep(0.05)
                samples = []
                # Sample only when the owned relay has a waiting synthetic child.
                for _ in range(25):
                    accounting = processes()
                    descendants = [rss for parent, rss in accounting.values()
                                   if parent == child.pid]
                    if child.pid in accounting and descendants:
                        samples.append((accounting[child.pid][1], sum(descendants)))
                    time.sleep(0.1)
                assert child.poll() is None and len(samples) >= 10
                errors.seek(0)
                assert not errors.read(), 'relay_reported_error'
                relay = [row[0] for row in samples]
                fixture = [row[1] for row in samples]
                return {
                    'platform': platform.system(), 'architecture': platform.machine(),
                    'binary_sha256': digest(binary), 'samples': len(samples),
                    'unit': 'KiB RSS (ps)',
                    'relay_only': {'min': min(relay), 'median': statistics.median(relay),
                                   'max': max(relay)},
                    'synthetic_python_cli_child': {'min': min(fixture),
                        'median': statistics.median(fixture), 'max': max(fixture)},
                    'scope': 'one optimized relay, empty mailbox, real private SQLite; synthetic CLI',
                    'not_verified': 'native Treer CLI + Host/Controller total RSS; loaded queue; multiple instances',
                }
            finally:
                # Only terminate the exact relay this measurement owns. Its
                # synthetic child finishes naturally within one second.
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=3)
                time.sleep(1.1)


if __name__ == '__main__':
    print(json.dumps(measure(Path(sys.argv[1])), indent=2))
