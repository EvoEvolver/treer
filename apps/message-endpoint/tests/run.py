#!/usr/bin/env python3
"""Focused, offline App checks with no inherited Treer/harness identity."""
import os
import argparse
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
repo = root.parents[1]
tool_home = Path.home()
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--cargo', type=Path, default=tool_home / '.cargo/bin/cargo')
parser.add_argument('--cargo-home', type=Path, default=tool_home / '.cargo')
parser.add_argument('--rustup-home', type=Path, default=tool_home / '.rustup')
options = parser.parse_args()
cargo = options.cargo.absolute()  # rustup is a multicall symlink; retain argv[0].
with tempfile.TemporaryDirectory(prefix='treer-endpoint-check-') as home:
    environment = {
        'HOME': home,
        'PATH': '/usr/bin:/bin:/usr/sbin:/sbin',
        'TMPDIR': home,
        'CARGO_HOME': str(options.cargo_home.resolve()),
        'RUSTUP_HOME': str(options.rustup_home.resolve()),
        'CARGO_TARGET_DIR': str(repo / 'target'),
        'CARGO_BUILD_JOBS': '2',
    }
    # Pin tooling/cache locations only. No TREER_*, AGENT_UI_*, harness, cloud
    # credentials, proxy vars or real HOME reach tests or their child processes.
    for command in [
        ['/usr/bin/python3', '-I', str(root / 'tests/test_runtime_smoke.py')],
        [str(cargo), 'fmt', '--check'],
        [str(cargo), 'test', '--offline', '--locked', '--all-features', '--', '--test-threads=2'],
        [str(cargo), 'build', '--offline', '--locked', '--features', 'hub', '--bin', 'treer-message-hub'],
        ['/usr/bin/python3', '-I', str(root / 'tests/test_production_boundary.py'), str(repo / 'target/debug/treer-message-hub')],
        ['/usr/bin/python3', '-I', str(root / 'tests/test_http_runtime.py'), str(repo / 'target/debug/treer-message-hub')],
        [str(cargo), 'clippy', '--offline', '--locked', '--all-features', '--all-targets', '--', '-D', 'warnings'],
        [str(cargo), 'build', '--release', '--offline', '--locked', '--all-features', '--bins'],
        ['/usr/bin/python3', '-I', str(root / 'tests/test_production_boundary.py'), str(repo / 'target/release/treer-message-hub')],
    ]:
        subprocess.run(command, cwd=root, env=environment, check=True)
