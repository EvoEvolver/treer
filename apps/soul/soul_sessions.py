"""Temporary native harness export/import boundary, to move to launcher adapters.

No credentials are exported. All import destinations belong to a new Agent.
"""
import json
import os
import re
import shutil
import time
from pathlib import Path

UUID = re.compile(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}')
MAX_FILE = 128 * 1024 * 1024


def project_key(cwd):
    return re.sub(r'[^A-Za-z0-9]', '-', str(Path(cwd).resolve()))


def copy_snapshot(source, destination, jsonl=False):
    if source.is_symlink() or not source.is_file():
        raise RuntimeError('Session capture rejects symbolic links and non-files')
    if source.stat().st_size > MAX_FILE:
        raise RuntimeError('Session file is too large')
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    for _ in range(5):
        before = source.stat()
        shutil.copyfile(source, destination)
        destination.chmod(0o600)
        after = source.stat()
        if (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns):
            if jsonl and destination.stat().st_size:
                with destination.open('rb') as stream:
                    stream.seek(-1, 2)
                    if stream.read(1) != b'\n':
                        raise RuntimeError('Session contains an incomplete JSONL record')
            return
        time.sleep(0.05)
    raise RuntimeError('Session kept changing; retry when idle')


def export_session(harness, session, cwd, destination):
    cwd = Path(cwd).resolve()
    destination = Path(destination)
    if session in (None, 'self'):
        keys = ('CLAUDE_SESSION_ID', 'AGENT_UI_SESSION_ID', 'TREER_SESSION_ID') if harness == 'claude' else ('CODEX_SESSION_ID', 'CODEX_THREAD_ID', 'AGENT_UI_SESSION_ID', 'TREER_SESSION_ID')
        session = next((os.environ[k] for k in keys if os.environ.get(k)), None)
    if session and not UUID.fullmatch(session):
        raise RuntimeError('Session ID must be a UUID')
    home = Path.home()
    if harness == 'claude':
        project = Path(os.environ.get('CLAUDE_CONFIG_DIR', home / '.claude')) / 'projects' / project_key(cwd)
        candidates = list(project.glob((session or '*') + '.jsonl'))
    else:
        config = Path(os.environ.get('CODEX_HOME', home / '.codex'))
        candidates = list((config / 'sessions').rglob('*.jsonl'))
        matching = []
        for path in candidates:
            if path.is_symlink():
                continue
            with path.open('rb') as stream:
                line = stream.readline(1024 * 1024)
            try:
                record = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            meta = record.get('payload', {}) if record.get('type') == 'session_meta' else {}
            if (session and meta.get('id') == session) or (not session and isinstance(meta.get('cwd'), str) and Path(meta['cwd']).resolve() == cwd):
                matching.append(path)
        candidates = matching
    # Never pick newest from a shared directory; ambiguity must be explicit.
    if len(candidates) != 1:
        raise RuntimeError('Cannot identify one self session; pass --session ID explicitly')
    source = candidates[0]
    if not session:
        if harness == 'claude':
            session = source.stem
        else:
            with source.open('rb') as stream:
                session = json.loads(stream.readline(1024*1024))['payload']['id']
    if not UUID.fullmatch(session):
        raise RuntimeError('Captured session ID is invalid')
    copy_snapshot(source, destination / 'files/session.jsonl', True)
    adapter = dict(name=harness, session_id=session, session_file='files/session.jsonl', cwd=str(cwd))
    if harness == 'codex':
        adapter['session_relative_path'] = source.relative_to(config / 'sessions').as_posix()
        snapshots = list((config / 'shell_snapshots').glob(session + '*.sh'))
        if snapshots:
            if len(snapshots) != 1:
                raise RuntimeError('Ambiguous shell snapshot')
            copy_snapshot(snapshots[0], destination / 'files/shell_snapshot.sh')
            adapter.update(shell_snapshot_file='files/shell_snapshot.sh', shell_snapshot_name=snapshots[0].name)
    else:
        adapter['memory_files'] = []
        memory = project / 'memory'
        if memory.is_symlink():
            raise RuntimeError('Memory directory may not be a symbolic link')
        if memory.exists():
            for path in sorted(memory.rglob('*')):
                if path.is_symlink():
                    raise RuntimeError('Memory may not contain symbolic links')
                if path.is_file():
                    relative = 'files/memory/' + path.relative_to(memory).as_posix()
                    copy_snapshot(path, destination / relative)
                    adapter['memory_files'].append(relative)
        instructions = cwd / 'CLAUDE.md'
        if instructions.exists():
            copy_snapshot(instructions, destination / 'files/CLAUDE.md')
            adapter['project_instructions_file'] = 'files/CLAUDE.md'
    return adapter


def import_session(bundle, adapter, workspace, state_root):
    bundle, workspace, state_root = Path(bundle), Path(workspace).resolve(), Path(state_root).resolve()
    workspace.mkdir(parents=True, exist_ok=True, mode=0o700)
    harness, session = adapter['name'], adapter.get('session_id')
    if harness not in ('claude','codex') or not isinstance(session, str) or not UUID.fullmatch(session):
        raise RuntimeError('Invalid declarative harness adapter')
    # Rebase structured cwd fields only; textual tool records remain archival evidence.
    records = []
    with (bundle / adapter['session_file']).open('r', encoding='utf-8') as stream:
        for line in stream:
            value = json.loads(line)
            if not isinstance(value, dict):
                raise RuntimeError('Session JSONL record must be an object')
            if 'cwd' in value:
                value['cwd'] = str(workspace)
            payload = value.get('payload')
            if isinstance(payload, dict) and 'cwd' in payload:
                payload['cwd'] = str(workspace)
            records.append(json.dumps(value, ensure_ascii=False) + '\n')
    environment = {}
    if harness == 'codex':
        config = state_root / 'codex'
        target = config / 'sessions' / adapter['session_relative_path']
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        original = Path(os.environ.get('CODEX_HOME', Path.home()/'.codex'))
        # Login remains local: link, never copy/read credential content or export it.
        if (original / 'auth.json').exists() and not (config / 'auth.json').exists():
            (config / 'auth.json').symlink_to((original / 'auth.json').resolve())
        environment['CODEX_HOME'] = str(config)
        # Shell snapshots are archival only: never execute source shell state.
    else:
        config = Path(os.environ.get('CLAUDE_CONFIG_DIR', Path.home()/'.claude'))
        project = config / 'projects' / project_key(workspace)
        project.mkdir(parents=True, exist_ok=True, mode=0o700)
        target = project / (session + '.jsonl')
        for path in adapter.get('memory_files', []):
            destination = project / 'memory' / Path(path).relative_to('files/memory')
            destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            shutil.copyfile(bundle / path, destination)
            destination.chmod(0o600)
        if adapter.get('project_instructions_file'):
            shutil.copyfile(bundle / adapter['project_instructions_file'], workspace / 'CLAUDE.md')
            (workspace / 'CLAUDE.md').chmod(0o600)
    target.write_text(''.join(records), encoding='utf-8')
    target.chmod(0o600)
    return environment


def launcher_path():
    override = os.environ.get('TREER_AGENT_LAUNCHER')
    if override:
        path = Path(override).expanduser()
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError('Configured launcher is not executable')
        return path.resolve()
    root = Path(os.environ.get('TREER_AGENT_INSTALL_ROOT', Path(os.environ.get('TREER_MACHINE_ROOT', str(Path.home()))) / '.treer/launchers/agent')) / 'current'
    for relative in ('launchers/agent/scripts/treer-agent.sh', 'scripts/treer-agent.sh', 'treer-agent.sh'):
        path = root / relative
        if path.is_file() and os.access(path, os.X_OK):
            return path.resolve()
    raise RuntimeError('Unified launcher is not installed; configure TREER_AGENT_LAUNCHER or install treer-agent')
