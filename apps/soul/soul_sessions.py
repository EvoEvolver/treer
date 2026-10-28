"""Thin launcher session-transfer bridge; no harness-native format implementation."""
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path

UUID = re.compile(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}')
MAX_FILE = 128 * 1024 * 1024


def safe_path(value):
    if not isinstance(value,str) or not value or '\\' in value or '\x00' in value or value.startswith('/') or any(part in ('','.','..') for part in value.split('/')):
        raise RuntimeError('Unsafe session-transfer path')
    return value


def project_instruction(path):
    return (path in ('AGENTS.md','CLAUDE.md','MEMORY.md','.claude/CLAUDE.md')
            or re.fullmatch(r'\.(?:codex|claude)/memory/.+\.md', path) is not None)


def transfer_file_allowed(harness, scope, path):
    if scope == 'project': return project_instruction(path)
    if scope == 'session': return path == 'session.jsonl' or (harness == 'claude' and re.fullmatch(r'subagents/.+\.jsonl', path) is not None)
    if scope == 'memory': return harness == 'claude' and path.endswith('.md')
    return False


def copy_snapshot(source, destination, jsonl=False):
    # Generic upload copies only, provider export consistency belongs to launcher.
    if source.is_symlink() or not source.is_file() or source.stat().st_size > MAX_FILE:
        raise RuntimeError('Upload requires a bounded regular file')
    destination.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    shutil.copyfile(source,destination)
    destination.chmod(0o600)


def launcher_path():
    """The installed `treer-launch` command (else its deprecated `treer-agent`
    alias), or an explicit TREER_AGENT_LAUNCHER override."""
    override=os.environ.get('TREER_AGENT_LAUNCHER')
    candidates=[Path(override).expanduser()] if override else [
        Path(found) for name in ('treer-launch','treer-agent')
        for found in [shutil.which(name),Path.home()/'.local/bin'/name] if found]
    for path in candidates:
        if path.is_file() and os.access(path,os.X_OK): return path.resolve()
    if override: raise RuntimeError('Configured launcher is not executable')
    raise RuntimeError('treer-launch is not installed (it ships with Treer); install Treer or configure TREER_AGENT_LAUNCHER')


def run_transfer(*args):
    # `treer-launch session ...` runs the launcher's native offline transfer CLI.
    with tempfile.TemporaryFile() as output:
        result=subprocess.run([str(launcher_path()),'session',*args],stdin=subprocess.DEVNULL,stdout=output,
                              stderr=subprocess.DEVNULL,timeout=60)
        if result.returncode: raise RuntimeError('Launcher session transfer failed; verify session identity, version and destination')
        if output.tell()>MAX_FILE*2: raise RuntimeError('Session-transfer result too large')
        output.seek(0)
        return json.load(output)


def require_transfer_harness(harness):
    if harness!='claude':
        raise RuntimeError('treer-launch session transfer supports Claude only; '+str(harness)+' sessions cannot be saved or restored')


def validate_bundle(bundle,harness=None,session=None):
    if not isinstance(bundle,dict) or bundle.get('schema')!='treer.agent-session/v1' or bundle.get('harness') not in ('claude','codex'):
        raise RuntimeError('Invalid launcher session bundle')
    if harness and bundle['harness']!=harness: raise RuntimeError('Exported harness mismatch')
    if not isinstance(bundle.get('sessionId'),str) or not UUID.fullmatch(bundle['sessionId']):
        raise RuntimeError('Invalid exported session identity')
    if session and bundle['sessionId']!=session: raise RuntimeError('Exported session identity mismatch')
    files=bundle.get('files')
    if not isinstance(files,list) or not 1<=len(files)<=250: raise RuntimeError('Invalid session file list')
    seen=set(); decoded=[]; total=0
    for record in files:
        scope=record.get('scope'); path=safe_path(record.get('path'))
        if scope not in ('session','memory','project') or (scope,path) in seen or record.get('encoding')!='base64':
            raise RuntimeError('Invalid session file record')
        if not transfer_file_allowed(bundle['harness'],scope,path): raise RuntimeError('Unsupported native transfer file')
        if any(part.lower() in ('auth.json','.credentials.json','config.toml','settings.json','.env') for part in path.split('/')):
            raise RuntimeError('Credential/config files may not be exported')
        try: data=base64.b64decode(record.get('content',''),validate=True)
        except (ValueError,TypeError): raise RuntimeError('Invalid session file encoding')
        total+=len(data)
        if total>MAX_FILE or hashlib.sha256(data).hexdigest()!=record.get('sha256'):
            raise RuntimeError('Session file size or integrity failure')
        seen.add((scope,path)); decoded.append((scope,path,data))
    return decoded


def export_session(harness,session,cwd,destination):
    require_transfer_harness(harness)
    if not isinstance(session,str) or not UUID.fullmatch(session):
        raise RuntimeError('Cannot identify own self session; pass --session ID explicitly')
    config=Path(os.environ.get('CLAUDE_CONFIG_DIR') or Path.home()/'.claude').expanduser().resolve()
    bundle=run_transfer('export','--harness',harness,'--config-dir',str(config),
                        '--session-id',session,'--cwd',str(Path(cwd).resolve()))
    decoded=validate_bundle(bundle,harness,session)
    entries=[]
    for scope,path,data in decoded:
        relative='files/'+scope+'/'+path
        target=Path(destination)/relative
        target.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
        target.write_bytes(data);target.chmod(0o600)
        entries.append(dict(scope=scope,path=path,file=relative))
    return dict(name=harness,session_id=session,cwd=bundle.get('cwd',str(Path(cwd).resolve())),
                exported_at=bundle.get('exportedAt'),transfer_files=entries)


def import_session(bundle_root,adapter,workspace,state_root):
    require_transfer_harness(adapter.get('name'))
    entries=adapter.get('transfer_files')
    if not isinstance(entries,list):
        # Legacy v1 snapshots are translated structurally, not interpreted natively.
        # Provider decoding/rebasing remains wholly in the launcher's adapter.
        entries=[dict(scope='session',path='session.jsonl',file=adapter['session_file'])]
        for file in adapter.get('memory_files',[]):
            entries.append(dict(scope='memory',path=file.removeprefix('files/memory/'),file=file))
        if adapter.get('project_instructions_file'):
            entries.append(dict(scope='project',path='CLAUDE.md',file=adapter['project_instructions_file']))
    files=[]
    for entry in entries:
        data=(Path(bundle_root)/safe_path(entry['file'])).read_bytes()
        files.append(dict(scope=entry['scope'],path=safe_path(entry['path']),encoding='base64',
                          content=base64.b64encode(data).decode(),sha256=hashlib.sha256(data).hexdigest()))
    bundle=dict(schema='treer.agent-session/v1',harness=adapter['name'],
                sessionId=adapter.get('source_session_id',adapter['session_id']),cwd=adapter.get('cwd',''),
                exportedAt=adapter.get('exported_at') or datetime.now(timezone.utc).isoformat(),files=files)
    validate_bundle(bundle)
    # The native import owns a new directory. On failed first import, leave it
    # for operator inspection rather than overwriting user data on retry.
    destination=Path(state_root)/'native'
    with tempfile.NamedTemporaryFile(mode='w',suffix='.json',dir=state_root,encoding='utf-8') as source:
        json.dump(bundle,source);source.flush()
        result=run_transfer('import','--directory',str(destination),
                            '--session-id',adapter['session_id'],'--input',source.name)
    if result.get('sessionId')!=adapter['session_id']:
        raise RuntimeError('Imported session ID does not match new incarnation')
    cwd=Path(result.get('cwd','')).resolve()
    if not cwd.is_relative_to(destination.resolve()): raise RuntimeError('Launcher imported outside dedicated Agent directory')
    environment=result.get('environment',{})
    if not isinstance(environment,dict) or set(environment)-{'CLAUDE_CONFIG_DIR'}:
        raise RuntimeError('Launcher returned unsupported environment selectors')
    for path in environment.values():
        if not isinstance(path,str): raise RuntimeError('Invalid launcher storage selector')
        resolved=Path(path).resolve()
        # The launcher may select an isolated home inside the dedicated Agent
        # directory, or exactly the locally configured Claude home.
        local=Path(os.environ.get('CLAUDE_CONFIG_DIR',Path.home()/'.claude')).resolve()
        if resolved!=local and not resolved.is_relative_to(destination.resolve()):
            raise RuntimeError('Launcher selected an unrelated Claude home')
    return environment,str(cwd)
