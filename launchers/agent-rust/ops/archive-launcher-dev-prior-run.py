#!/usr/bin/env python3
"""Leader-only launcher-dev stopped-agent preservation step for #158/7167.
Adapted from reviewed #158/5011+5012 helper; successor review required.
No data is removed. Never use --undo after a fresh import has begun.
"""
import argparse
import fcntl
import hashlib
import json
import os
import signal
from pathlib import Path
import stat

ROOT = Path('/Users/dooms/.local/state/treer/agent-private/srv_9452d9fd5db447d3b242dd66c7689594/ag_cfc9d39a455d48d4995faf3d314adf08')
AUDIT = Path('/Users/dooms/.local/state/treer/switch-evidence/launcher-dev-active-history-attempt-4')
ARCHIVE = AUDIT / 'prior-run'
REQUIRED = ['rust-spike.json', 'node-backup-v1', '.node-backup-restored']
OPTIONAL = ['rust-journal-history', 'rust-images', 'rust-provider-stderr.log']
NAMES = REQUIRED.copy()

def safe(path, directory=False):
    info=path.lstat()
    assert not stat.S_ISLNK(info.st_mode) and info.st_uid==os.getuid(), 'unsafe owner/link'
    assert info.st_mode & 0o077==0, 'unsafe permissions'
    assert stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode), 'unsafe file type'
    return info

def inventory(base):
    result=[]
    for name in NAMES:
        path=base/name
        directory = name in ('node-backup-v1', 'rust-journal-history', 'rust-images')
        safe(path,directory=directory)
        files=sorted(path.iterdir()) if directory else [path]
        if name == 'rust-images': assert not files, 'image directory is not empty'
        if name == 'rust-journal-history':
            assert len(files) <= 64, 'history count limit'
            assert all(len(f.name.removesuffix('.tmp')) == 25 and f.name.removesuffix('.tmp')[-5:] == '.json' and f.name[:20].isascii() and f.name[:20].isdigit() for f in files), 'unexpected history entry'
        for file in files:
            info=safe(file)
            assert info.st_nlink == 1, 'linked evidence file'
            assert info.st_size<=32*1024*1024 and len(result)<102, 'inventory limit'
            digest=hashlib.sha256()
            with file.open('rb') as stream:
                for chunk in iter(lambda:stream.read(65536),b''): digest.update(chunk)
            result.append({'path':str(file.relative_to(base)),'bytes':info.st_size,'sha256':digest.hexdigest()})
    assert sum(item['bytes'] for item in result)<=164*1024*1024, 'total limit'
    return sorted(result,key=lambda item:item['path'])

def audit(name,value):
    data=(json.dumps(value,sort_keys=True,indent=2)+'\n').encode()
    descriptor=os.open(AUDIT/name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(descriptor,'wb') as file: file.write(data);file.flush();os.fsync(file.fileno())
    print(json.dumps({'audit':str(AUDIT/name),'files':len(value),'bytes':sum(item['bytes'] for item in value),'manifestSha256':hashlib.sha256(data).hexdigest()}))

def main():
    global NAMES
    def interrupted(_signal,_frame): raise KeyboardInterrupt('interrupted; rolling back move')
    signal.signal(signal.SIGTERM,interrupted)
    parser=argparse.ArgumentParser()
    parser.add_argument('--stopped-pid',required=True,type=int)
    parser.add_argument('--undo',action='store_true')
    args=parser.parse_args()
    assert args.stopped_pid>1, 'invalid recorded PID'
    try: os.kill(args.stopped_pid,0)
    except ProcessLookupError: pass
    else: raise RuntimeError('recorded launcher PID still exists; keep agent stopped')
    assert ROOT.resolve()==ROOT and ROOT.parent.resolve()==ROOT.parent, 'linked ancestor'
    safe(ROOT,True);safe(ROOT.parent,True)
    safe(ROOT/'images',True)
    assert not any((ROOT/'images').iterdir()), 'image directory is not empty'
    evidence = ARCHIVE if args.undo else ROOT
    NAMES = REQUIRED[:-1] + [name for name in OPTIONAL if os.path.lexists(evidence/name)] + REQUIRED[-1:]
    assert set(os.listdir(ROOT)) <= set(NAMES) | {'images', '.rust-launcher.lock'}, 'unexpected private entry'
    lease=os.open(ROOT/'.rust-launcher.lock',os.O_RDWR|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(lease,'rb') as lock:
        safe(ROOT/'.rust-launcher.lock')
        assert os.fstat(lock.fileno()).st_nlink==1, 'linked lease inode'
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        AUDIT.mkdir(mode=0o700,parents=True,exist_ok=True);safe(AUDIT,True)
        if args.undo:
            safe(ARCHIVE,True)
            assert all(not os.path.lexists(ROOT/name) for name in NAMES), 'new import/state exists; use NEW backup rollback instead'
            source,destination=ARCHIVE,ROOT
            before=inventory(source)
            expected=json.loads((AUDIT/'prior-before.json').read_text())
            assert before==expected, 'archive manifest changed'
            order=list(reversed(NAMES)) # restore the fence first
        else:
            assert not os.path.lexists(ARCHIVE), 'archive must be absent'
            before=inventory(ROOT)
            audit('prior-before.json',before)
            os.mkdir(ARCHIVE,0o700)
            source,destination=ROOT,ARCHIVE
            order=NAMES # marker moves last; old start stays fenced mid-step
        assert source.stat().st_dev==destination.stat().st_dev, 'cross-filesystem move refused'
        moved=[]
        try:
            for name in order:
                assert not os.path.lexists(destination/name), 'destination appeared'
                os.rename(source/name,destination/name);moved.append(name)
            after=inventory(destination)
            assert after==before, 'post-move manifest mismatch'
            audit('undo-after.json' if args.undo else 'prior-after.json',after)
        except BaseException:
            for name in reversed(moved):
                assert not os.path.lexists(source/name), 'rollback destination appeared; do not start agent'
                os.rename(destination/name,source/name)
            raise
        finally:
            for directory in [ROOT,ARCHIVE]:
                descriptor=os.open(directory,os.O_RDONLY)
                try: os.fsync(descriptor)
                finally: os.close(descriptor)
    print('prior_run_restored_before_import' if args.undo else 'prior_run_archived; keep old archive; fresh import required')

if __name__=='__main__': main()
