#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from urllib.error import HTTPError
from urllib.parse import quote, urlencode, urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler

from soul_secrets import redact
from soul_sessions import export_session, import_session, launcher_path

VERSION = '2.0.0'
MAX_BYTES = 128 * 1024 * 1024
ID = re.compile(r'soul_[0-9a-f]{32}')
PROTECTED = {'PATH','HOME','CODEX_HOME','CLAUDE_CONFIG_DIR','LD_PRELOAD','LD_LIBRARY_PATH','PYTHONPATH','NODE_OPTIONS','BASH_ENV','ENV','SHELL','TMPDIR','LD_AUDIT','DYLD_INSERT_LIBRARIES','PYTHONSTARTUP','RUBYOPT','PERL5OPT'}


def treer(*argv):
    result = subprocess.run([os.environ.get('TREER_BIN','treer'), *argv], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError('Treer command failed; check permissions and target availability')
    return result.stdout


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def request(server, method, path, body=None):
    origin = urlsplit(server)
    if origin.scheme not in ('http','https') or not origin.netloc or origin.username or origin.password or origin.query or origin.fragment:
        raise RuntimeError('Soul server must be a trusted HTTP(S) App URL')
    headers = {'Accept':'application/json'}
    # Every request acquires a fresh service token; never expose workload credentials.
    service = os.environ.get('TREER_SOUL_SERVICE', 'soul')
    token = treer('token','create',service).strip()
    if not token or len(token) > 8192:
        raise RuntimeError('Unable to acquire Soul service token')
    headers['Authorization'] = 'Bearer ' + token
    if body is not None:
        headers['Content-Type'] = 'application/x-tar'
    if method != 'GET':
        headers['X-Soul-Request'] = '1'
    try:
        with build_opener(NoRedirect()).open(Request(server.rstrip('/')+'/'+path.lstrip('/'), data=body, headers=headers, method=method), timeout=60) as response:
            data = response.read(MAX_BYTES+1)
            if len(data)>MAX_BYTES:
                raise RuntimeError('Soul response exceeds limit')
            return data
    except HTTPError as error:
        # Server reports sanitized categories only. No raw error bodies in output.
        try:
            detail = json.loads(error.read(65536))['error']
        except (ValueError,KeyError):
            detail = {'code':'request_failed'}
        raise RuntimeError(f"Soul HTTP {error.code}: {detail.get('code')}; findings={detail.get('findings', {})}") from error


def api(server, method, path, body=None):
    return json.loads(request(server,method,path,body))


def print_json(value):
    print(json.dumps(value, indent=2, sort_keys=True))


def archive_path(value):
    if not isinstance(value,str) or not value or '\\' in value or '\x00' in value or value.startswith('/') or any(p in ('','.','..') for p in value.split('/')):
        raise RuntimeError('Unsafe archive path')
    return value


def create_archive(root, manifest, files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream,mode='w') as archive:
        contents = [('manifest.json',json.dumps(manifest,sort_keys=True).encode())]
        contents.extend((archive_path(path), (root/path).read_bytes()) for path in files)
        if sum(len(data) for _,data in contents)>MAX_BYTES:
            raise RuntimeError('Capture exceeds size limit')
        for name,data in contents:
            info=tarfile.TarInfo(name)
            info.size,info.mode=len(data),0o600
            archive.addfile(info,io.BytesIO(data))
    return stream.getvalue()


def upload_options(args):
    options=[]
    for option,attr in [('name','name'),('parent_soul_id','parent'),('visibility','visibility')]:
        if getattr(args,attr,None):
            options.append((option,getattr(args,attr)))
    options.extend(('reader',v) for v in getattr(args,'reader',[]))
    options.extend(('label',v) for v in getattr(args,'label',[]))
    if getattr(args,'allow_secrets',False):
        options.append(('allow_secrets','true'))
    return options


def redact_json(value, counts):
    if isinstance(value, dict):
        clean = {}
        for key, item in value.items():
            if re.fullmatch(r'(?i)(password|secret|api[_-]?key|enrollment[_-]?token|machine[_-]?token)', key):
                if item:
                    counts['secret_assignment'] = counts.get('secret_assignment', 0) + 1
                continue
            clean[key] = redact_json(item, counts)
        return clean
    if isinstance(value, list):
        return [redact_json(item, counts) for item in value]
    if isinstance(value, str):
        data, findings = redact(value.encode())
        for category, count in findings.items():
            counts[category] = counts.get(category, 0) + count
        return data.decode('utf-8')
    return value


def redact_file(path, counts):
    if path.suffix == '.jsonl':
        with path.open('r', encoding='utf-8') as stream:
            records = [json.dumps(redact_json(json.loads(line), counts), ensure_ascii=False) + '\n' for line in stream]
        path.write_text(''.join(records), encoding='utf-8')
    else:
        data, findings = redact(path.read_bytes())
        path.write_bytes(data)
        for category, count in findings.items():
            counts[category] = counts.get(category, 0) + count


def self_session(harness):
    # Ask the authenticated Controller for THIS Agent's registered AIS port.
    # Never use a default UI port which may belong to a different Agent.
    try:
        descriptor = json.loads(treer('interface', 'show'))
    except (RuntimeError, ValueError):
        return None
    if not descriptor:
        return None
    port = descriptor.get('port')
    if not isinstance(port, int) or isinstance(port, bool) or not 1 <= port <= 65535:
        raise RuntimeError('Own Agent Interface has an invalid port')
    from urllib.request import build_opener, ProxyHandler
    try:
        with build_opener(ProxyHandler({})).open(f'http://127.0.0.1:{port}/api/state', timeout=5) as response:
            state = json.loads(response.read(2 * 1024 * 1024))
        actual = state.get('harness', {})
        actual = actual.get('name') if isinstance(actual, dict) else actual
        if actual != harness:
            raise RuntimeError('Current Agent Interface harness differs from requested capture')
        session = state.get('thread', {}).get('id')
        from soul_sessions import UUID
        if not isinstance(session, str) or not UUID.fullmatch(session):
            raise RuntimeError('Current Agent Interface has no active native session')
        return session
    except (OSError, ValueError) as error:
        raise RuntimeError('Cannot read current Agent Interface state; pass --session explicitly') from error


def command_save(args):
    # No provider credential files or original files are modified.
    with tempfile.TemporaryDirectory(prefix='treer-soul-save-') as temp:
        root=Path(temp)
        root.chmod(0o700)
        session = self_session(args.harness) if args.session == 'self' else args.session
        adapter=export_session(args.harness,session or args.session,args.cwd,root)
        who=json.loads(treer('whoami'))
        source_machine=who.get('machine',{}).get('server_id')
        manifest={'schema_version':1,'name':args.name or f"{args.harness} {adapter['session_id'][:8]}",
                  'environment':{},'adapter':adapter}
        files=sorted(p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file())
        counts={}
        if args.redact:
            for path in files:
                redact_file(root/path, counts)
        options=upload_options(args)+[('harness',args.harness),('session_id',adapter['session_id'])]
        if source_machine:
            options.append(('source_machine',source_machine))
        record=api(args.server,'POST','v1/souls?'+urlencode(options),create_archive(root,manifest,files))
        record['redaction']={'enabled':args.redact,'findings':counts,'count':sum(counts.values())}
        # created_by is authoritative server metadata, not self-reported upload input.
        print_json(record)


def command_upload(args):
    manifest=json.loads(Path(args.manifest).read_text())
    with tempfile.TemporaryDirectory(prefix='treer-soul-upload-') as temp:
        root=Path(temp)
        files=[]
        for mapping in args.file:
            path,separator,source=mapping.partition('=')
            if not separator:
                raise RuntimeError('--file requires ARCHIVE_PATH=LOCAL_PATH')
            path=archive_path(path)
            if path in files or path=='manifest.json':
                raise RuntimeError('Duplicate archive path')
            target=root/path
            target.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
            from soul_sessions import copy_snapshot
            copy_snapshot(Path(source),target)
            files.append(path)
        print_json(api(args.server,'POST','v1/souls?'+urlencode(upload_options(args)),create_archive(root,manifest,files)))


def resolve(server,target):
    value,separator,version=target.rpartition('@')
    if not separator:
        value,version=target,'latest'
    if ID.fullmatch(value):
        record=api(server,'GET','v1/souls/'+value)
        if version!='latest' and str(record['version'])!=version:
            raise RuntimeError('Soul ID has a different version')
        return record
    return api(server,'GET','v1/souls/by-name/'+quote(value,safe='')+'?'+urlencode({'version':version}))


# The target probe is read-only and never opens auth/config credential files.
PREFLIGHT = '''import json,os,shutil,subprocess,sys
from pathlib import Path
harness=sys.argv[1]
machine_root=Path(sys.argv[2])
exe=shutil.which('python3' if harness=='generic' else harness)
version=''
if exe:
    try:
        r=subprocess.run([exe,'--version'],stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=10)
        import re
        m=re.search(r"[0-9]+\\.[0-9]+(?:\\.[0-9]+)?",r.stdout)
        version=m.group(0) if m and r.returncode==0 else ''
    except Exception: pass
home=Path.home()
if harness=='generic':
    logged_in=True
elif harness=='codex':
    config=Path(os.environ.get('CODEX_HOME',home/'.codex'))
    logged_in=(config/'auth.json').is_file() or bool(os.environ.get('OPENAI_API_KEY'))
else:
    config=Path(os.environ.get('CLAUDE_CONFIG_DIR',home/'.claude'))
    logged_in=(config/'.credentials.json').is_file() or bool(os.environ.get('ANTHROPIC_API_KEY'))
    if sys.platform=='darwin' and shutil.which('security'):
        try:
            logged_in = logged_in or subprocess.run(['security','find-generic-password','-s','Claude Code-credentials'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=5).returncode==0
        except Exception: pass
root=Path(os.environ.get('TREER_AGENT_INSTALL_ROOT',machine_root/'.treer/launchers/agent'))/'current'
candidates=[Path(os.environ['TREER_AGENT_LAUNCHER'])] if os.environ.get('TREER_AGENT_LAUNCHER') else [root/p for p in ['launchers/agent/scripts/treer-agent.sh','scripts/treer-agent.sh','treer-agent.sh']]
launcher=next((str(p.resolve()) for p in candidates if p.is_file() and os.access(p,os.X_OK)),None)
print(json.dumps(dict(executable=bool(exe),version=version,logged_in=logged_in,launcher=launcher,free_bytes=shutil.disk_usage(home).free)))
'''


def preflight(machine,harness,size):
    machine_record = json.loads(treer('machine', 'show', machine))
    machine_root = machine_record.get('root')
    if not isinstance(machine_root, str) or not machine_root.startswith('/'):
        raise RuntimeError('Target machine has no absolute root')
    probe=json.loads(treer('exec','--machine',machine,'--cwd','.','--timeout','20000','--','python3','-c',PREFLIGHT,harness,machine_root))
    problems=[]
    if not probe.get('executable'): problems.append('Install '+harness)
    if not probe.get('logged_in'): problems.append('Log in to '+harness+' on target machine')
    if harness!='generic' and not probe.get('launcher'): problems.append('Install unified treer-agent launcher on target machine')
    version=probe.get('version','')
    try:
        parsed=tuple(int(p) for p in version.split('.'))
        minimum=(0,149,0) if harness=='codex' else (2,0,0) if harness=='claude' else (3,9,0)
        if parsed<minimum: problems.append('Upgrade '+harness+' to '+'.'.join(map(str,minimum))+' or later')
    except ValueError:
        problems.append('Could not determine harness version')
    if probe.get('free_bytes',0)<max(size*3,64*1024*1024): problems.append('Free disk space on target machine')
    return probe,problems


BOOTSTRAP = '''set -eu
base=$1
soul=$2
shift 2
client=$(mktemp "${TMPDIR:-/tmp}/treer-soul.XXXXXX")
trap 'rm -f "$client"' EXIT INT TERM
curl -fsSL "$base/client.py" -o "$client"
python3 "$client" --server "$base" run "$soul" "$@"
'''


def command_incarnate(args):
    record=resolve(args.server,args.target)
    # A new Agent cannot inherit the current caller's private read authority.
    if record['visibility']!='workspace':
        raise RuntimeError('New Agent cannot read this private Soul; save a new --visibility workspace snapshot first')
    adapter=record.get('manifest',{}).get('adapter',{})
    harness=adapter.get('name','generic')
    if harness not in ('claude','codex','generic'):
        raise RuntimeError('Unsupported harness adapter')
    if harness == 'generic' and not args.command:
        raise RuntimeError('Generic Soul needs an explicit command after --')
    probe,problems=preflight(args.machine,harness,record['size_bytes'])
    if problems and not args.force:
        raise RuntimeError('Preflight refused creation: '+'; '.join(problems)+'; --force explicitly overrides')
    if problems:
        print(json.dumps({'preflight_warnings':problems}),file=sys.stderr)
    name=args.name or f"{harness}-reborn-{record['soul_id'][-8:]}"
    argv=['agent','admin','create','--machine',args.machine,'--kind','command','--name',name,'--cwd','.']
    if args.folder:
        argv.extend(['--folder',args.folder])
    argv.extend(['--','sh','-c',BOOTSTRAP,'treer-soul',args.server,record['soul_id'], '--', *args.command])
    print_json({'soul_id':record['soul_id'],'version':record['version'],'agent':json.loads(treer(*argv)), 'preflight':probe})


def safe_extract(raw,destination):
    destination.mkdir(parents=True,exist_ok=False,mode=0o700)
    total=0
    seen=set()
    with tarfile.open(fileobj=io.BytesIO(raw),mode='r:*') as archive:
        count = 0
        for member in archive:
            count += 1
            if count > 256: raise RuntimeError('Too many archive entries')
            if member.size < 0: raise RuntimeError('Invalid archive size')
            path=archive_path(member.name.rstrip('/') if member.isdir() else member.name)
            if path in seen: raise RuntimeError('Duplicate archive entry')
            seen.add(path)
            if member.isdir():
                (destination/path).mkdir(parents=True,exist_ok=True,mode=0o700)
                continue
            if not member.isfile(): raise RuntimeError('Archive contains non-regular files')
            total+=member.size
            if total>MAX_BYTES: raise RuntimeError('Expanded archive exceeds limit')
            target=destination/path
            target.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
            with target.open('xb') as output:
                shutil.copyfileobj(archive.extractfile(member),output)
            target.chmod(0o600)
    manifest=json.loads((destination/'manifest.json').read_text())
    if manifest.get('schema_version')!=1: raise RuntimeError('Unsupported manifest schema')
    adapter=manifest.get('adapter',{})
    for field in ('session_file','session_relative_path','shell_snapshot_file','project_instructions_file'):
        if field in adapter: archive_path(adapter[field])
    for field in adapter.get('memory_files',[]):
        if not archive_path(field).startswith('files/memory/'): raise RuntimeError('Invalid memory file')
    return manifest


def command_run(args):
    actor=os.environ.get('TREER_AGENT_ID')
    if not actor or not re.fullmatch(r'[A-Za-z0-9_-]{1,160}',actor):
        raise RuntimeError('Restore requires managed Agent identity')
    base=Path(os.environ.get('TREER_SOUL_STATE_DIR',Path.home()/'.local/state/treer-soul')).expanduser().resolve()
    root=base/actor
    root.mkdir(parents=True,exist_ok=True,mode=0o700)
    # Prevent concurrent startup of a restore into the same Agent's state.
    import fcntl
    with (root/'.restore.lock').open('a+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        marker=root/'restored.json'
        workspace=root/'workspace'
        if marker.exists():
            state=json.loads(marker.read_text())
            if state['soul_id']!=args.soul_id: raise RuntimeError('Agent already restored from a different Soul')
        else:
            record=resolve(args.server,args.soul_id)
            raw=request(args.server,'GET','v1/souls/'+record['soul_id']+'/archive')
            if len(raw)!=record['archive_size'] or hashlib.sha256(raw).hexdigest()!=record['archive_sha256']:
                raise RuntimeError('Soul archive checksum mismatch')
            with tempfile.TemporaryDirectory(prefix='treer-soul-restore-',dir=root) as temp:
                bundle=Path(temp)/'bundle'
                manifest=safe_extract(raw,bundle)
                adapter=dict(manifest.get('adapter',{}))
                if adapter.get('name') in ('claude','codex'):
                    import uuid
                    adapter['source_session_id'] = adapter['session_id']
                    adapter['session_id'] = str(uuid.uuid4())
                    if adapter['name'] == 'codex':
                        adapter['session_relative_path'] = 'imported/' + adapter['session_id'] + '.jsonl'
                if adapter.get('name', 'generic') == 'generic':
                    adapter = {'name': 'generic'}
                    files_root = root / 'generic'
                    if files_root.exists():
                        shutil.rmtree(files_root)
                    shutil.copytree(bundle, files_root)
                    workspace.mkdir(parents=True, exist_ok=True, mode=0o700)
                    environment = {}
                    for key, path in manifest.get('environment', {}).items():
                        if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]{0,127}', key) or key in PROTECTED or key.startswith(('TREER_','AGENT_UI_','CODEX_UI_','LD_','DYLD_')):
                            raise RuntimeError('Protected environment binding')
                        path = archive_path(path)
                        if not (files_root / path).is_file():
                            raise RuntimeError('Missing environment binding file')
                        environment[key] = str((files_root / path).resolve())
                else:
                    environment=import_session(bundle,adapter,workspace,root)
                state=dict(soul_id=args.soul_id,adapter=adapter,environment=environment)
            temporary=marker.with_suffix('.tmp')
            temporary.write_text(json.dumps(state))
            temporary.chmod(0o600)
            os.replace(temporary,marker)
    environment=os.environ.copy()
    environment.update(state['environment'])
    # Restart never downloads/reimports the snapshot or overwrites learned memory.
    adapter=state['adapter']
    if adapter['name'] == 'generic':
        argv = args.command
        if argv and argv[0] == '--': argv = argv[1:]
        if not argv: raise RuntimeError('Generic Soul requires a command after --')
    else:
        machine_root = None
        if not os.environ.get('TREER_AGENT_LAUNCHER') and not os.environ.get('TREER_AGENT_INSTALL_ROOT'):
            machine_root = json.loads(treer('whoami')).get('machine', {}).get('root')
            if not isinstance(machine_root, str) or not Path(machine_root).is_absolute():
                raise RuntimeError('Cannot locate launcher without an absolute machine root')
        argv=[str(launcher_path(machine_root)),'--harness',adapter['name'],'--session-id',adapter['session_id'],'--cwd',str(workspace)]
    os.chdir(workspace)
    os.execvpe(argv[0],argv,environment)


def parser():
    p=argparse.ArgumentParser(description='Save and restore private immutable harness sessions')
    p.add_argument('--version',action='version',version='treer-soul '+VERSION)
    p.add_argument('--server',default=os.environ.get('TREER_SOUL_URL','http://soul.treer'))
    sub=p.add_subparsers(dest='subcommand',required=True)
    def sharing(command):
        command.add_argument('--name')
        command.add_argument('--parent')
        command.add_argument('--visibility',choices=['private','workspace'],default='private')
        command.add_argument('--reader',action='append',default=[])
        command.add_argument('--label',action='append',default=[])
        command.add_argument('--allow-secrets',action='store_true')
    save=sub.add_parser('save')
    save.add_argument('--harness',choices=['claude','codex'],required=True)
    save.add_argument('--session',default='self')
    save.add_argument('--cwd',default=str(Path.cwd()))
    save.add_argument('--redact',action=argparse.BooleanOptionalAction,default=True)
    sharing(save)
    save.set_defaults(function=command_save)
    capture=sub.add_parser('capture-codex',help='compatibility alias for save --harness codex')
    capture.add_argument('--session',default='self')
    capture.add_argument('--cwd',default=str(Path.cwd()))
    capture.add_argument('--redact',action=argparse.BooleanOptionalAction,default=True)
    capture.add_argument('--include-shell-snapshot',action='store_true',help='now included and redacted by save')
    sharing(capture)
    capture.set_defaults(harness='codex',function=command_save)
    upload=sub.add_parser('upload')
    upload.add_argument('--manifest',required=True)
    upload.add_argument('--file',action='append',default=[])
    sharing(upload)
    upload.set_defaults(function=command_upload)
    for name in ('show','delete'):
        command=sub.add_parser(name)
        command.add_argument('target')
        command.set_defaults(function=command_show if name=='show' else command_delete)
    listing=sub.add_parser('list')
    for name in ('name','harness','owner'): listing.add_argument('--'+name)
    listing.add_argument('--limit',type=int,default=50)
    listing.add_argument('--offset',type=int,default=0)
    listing.set_defaults(function=command_list)
    incarnation=sub.add_parser('incarnate')
    incarnation.add_argument('target')
    incarnation.add_argument('--machine',default='self')
    incarnation.add_argument('--name')
    incarnation.add_argument('--folder')
    incarnation.add_argument('--force',action='store_true')
    incarnation.set_defaults(function=command_incarnate, command=[])
    run=sub.add_parser('run',help=argparse.SUPPRESS)
    run.add_argument('soul_id')
    run.add_argument('command', nargs=argparse.REMAINDER)
    run.set_defaults(function=command_run)
    return p


def command_show(args): print_json(resolve(args.server,args.target))
def command_delete(args):
    record=resolve(args.server,args.target)
    print_json(api(args.server,'DELETE','v1/souls/'+record['soul_id']))
def command_list(args):
    query={k:getattr(args,k) for k in ('name','harness','owner','limit','offset') if getattr(args,k) is not None}
    print_json(api(args.server,'GET','v1/souls?'+urlencode(query)))


def main():
    arguments = sys.argv[1:]
    command = []
    if 'incarnate' in arguments and '--' in arguments[arguments.index('incarnate')+1:]:
        divider = arguments.index('--', arguments.index('incarnate')+1)
        command = arguments[divider+1:]
        arguments = arguments[:divider]
    args=parser().parse_args(arguments)
    if args.subcommand == 'incarnate': args.command = command
    try:
        args.function(args)
    except (OSError,RuntimeError,ValueError,KeyError,tarfile.TarError) as error:
        print('treer-soul: '+str(error),file=sys.stderr)
        raise SystemExit(1) from error


if __name__=='__main__': main()
