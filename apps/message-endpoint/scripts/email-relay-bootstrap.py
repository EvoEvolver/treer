#!/usr/bin/env python3
"""Fixed managed relay bootstrap: no CLI mutation, channel activation or mail send."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys


def private(path):
    if not path.is_absolute() or path.resolve() != path:
        raise ValueError('physical_path_required')
    st = path.lstat()
    if st.st_uid != os.getuid() or stat.S_IMODE(st.st_mode) != 0o700 or not path.is_dir():
        raise ValueError('private_directory_required')
    if any((p/'.git').exists() for p in (path,*path.parents)):
        raise ValueError('outside_repository_required')


def load(path):
    private(path.parent)
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as f:
        st=os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_uid!=os.getuid() or st.st_nlink!=1 or stat.S_IMODE(st.st_mode)!=0o600:
            raise ValueError('private_manifest_required')
        raw=f.read(16385)
        if len(raw)>16384: raise ValueError('manifest_too_large')
    return json.loads(raw)


def pin(path,expected):
    p=Path(path)
    if not p.is_absolute() or p.resolve()!=p or not re.fullmatch('[0-9a-f]{64}',expected):
        raise ValueError('invalid_pin')
    fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as f:
        st=os.fstat(f.fileno())
        if not stat.S_ISREG(st.st_mode) or st.st_size>64*1024*1024 or not os.access(p,os.X_OK):
            raise ValueError('invalid_binary')
        digest=hashlib.sha256(f.read(64*1024*1024+1)).hexdigest()
    if digest!=expected: raise ValueError('binary_pin_mismatch')


def prepare(path):
    manifest=load(Path(path))
    if set(manifest)!={'workspace_id','server_id','hub_id','recipient','generation','cli','cli_sha256','relay','relay_sha256','directory'}:
        raise ValueError('invalid_manifest')
    own=os.environ.get('TREER_AGENT_ID','')
    if not re.fullmatch('ag_[A-Za-z0-9_-]+',own): raise ValueError('managed_command_identity_required')
    for field,env in [('workspace_id','TREER_WORKSPACE_ID'),('server_id','TREER_SERVER_ID')]:
        if not manifest[field] or manifest[field]!=os.environ.get(env): raise ValueError('managed_placement_mismatch')
    for field in ('hub_id','recipient'):
        if not re.fullmatch('(ag|app)_[A-Za-z0-9_-]+',manifest[field]): raise ValueError('invalid_target')
    if len({own,manifest['hub_id'],manifest['recipient']})!=3 or type(manifest['generation']) is not int or manifest['generation']<1:
        raise ValueError('invalid_binding')
    if not os.environ.get('TREER_WORKLOAD_CREDENTIAL') or not os.environ.get('TREER_AGENT_SERVER_URL'):
        raise ValueError('managed_connection_required')
    directory=Path(manifest['directory']);private(directory)
    if directory!=Path(path).parent: raise ValueError('manifest_directory_mismatch')
    pin(manifest['cli'],manifest['cli_sha256']);pin(manifest['relay'],manifest['relay_sha256'])
    config={'endpoint_id':own,'hub_id':manifest['hub_id'],'generation':manifest['generation'],
            'recipients':[manifest['recipient']],'cli':manifest['cli'],'cli_sha256':manifest['cli_sha256'],
            'release_sha256':manifest['relay_sha256'],'state_directory':str(directory/'state')}
    target=directory/'relay.json'
    try:
        fd=os.open(target,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    except FileExistsError:
        if load(target)!=config: raise ValueError('existing_binding_mismatch')
    else:
        with os.fdopen(fd,'w') as f:
            json.dump(config,f,sort_keys=True);f.write('\n');f.flush();os.fsync(f.fileno())
        fd=os.open(directory,os.O_RDONLY|os.O_DIRECTORY)
        try: os.fsync(fd)
        finally: os.close(fd)
    # Own identity remains inherited in memory. No operator/profile/external-client fallback.
    env={k:v for k,v in os.environ.items() if k in ('PATH','HOME','TMPDIR','TREER_AGENT_ID','TREER_WORKSPACE_ID','TREER_SERVER_ID','TREER_AGENT_SERVER_URL','TREER_WORKLOAD_CREDENTIAL')}
    env['TREER_IDENTITY_MODE']='agent'
    return [manifest['relay'],'--config',str(target)],env


if __name__=='__main__':
    try:
        if len(sys.argv)!=2: raise ValueError('one_manifest_required')
        argv,env=prepare(sys.argv[1]);os.execve(argv[0],argv,env)
    except Exception:
        print('email_relay_bootstrap_refused',file=sys.stderr);sys.exit(1)
