"""Continue one already-restored Soul; never download or execute restoration.

The private, sealed manifest binds a single existing Agent and incarnation.
This is an operator entry wrapper, not a Soul installer or session importer.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys


def require(ok):
    if not ok:
        raise ValueError('soul_continuation_binding')


def pairs(items):
    value = {}
    for key, item in items:
        require(key not in value)
        value[key] = item
    return value


def checked(path, *, directory=False, limit=None):
    path = Path(path)
    require(path.is_absolute() and '..' not in path.parts)
    for parent in path.parents:
        meta = parent.lstat()
        require(stat.S_ISDIR(meta.st_mode) and meta.st_uid in (0, os.geteuid()))
        require(meta.st_mode & 0o022 == 0 or meta.st_mode & stat.S_ISVTX != 0)
    meta = path.lstat()
    require(meta.st_uid == os.geteuid() or meta.st_uid == 0)
    require(meta.st_mode & 0o022 == 0)
    require(stat.S_ISDIR(meta.st_mode) if directory else stat.S_ISREG(meta.st_mode))
    if directory:
        return path
    require(meta.st_nlink == 1 and (limit is None or meta.st_size <= limit))
    return path


def binding(manifest):
    require(not sys.flags.optimize)
    path = checked(manifest, limit=65536)
    require(path.stat().st_uid == os.geteuid() and path.stat().st_mode & 0o077 == 0)
    value = json.loads(path.read_bytes(), object_pairs_hook=pairs)
    require(set(value) == {'schema', 'agent', 'server', 'workspace', 'session', 'home', 'cwd',
                           'soulId', 'soulRoot', 'restore', 'restoreSha256', 'images', 'modes'})
    require(value['schema'] == 'treer.soul-continuation/v1')
    for env, key in [('TREER_AGENT_ID', 'agent'), ('TREER_SERVER_ID', 'server'),
                     ('TREER_WORKSPACE_ID', 'workspace')]:
        require(os.environ.get(env) == value[key])
    home = checked(value['home'], directory=True)
    cwd = checked(value['cwd'], directory=True)
    root = checked(value['soulRoot'], directory=True)
    require(root.is_relative_to(home) and cwd.is_relative_to(home))
    restore = checked(value['restore'], limit=65536)
    require(restore.is_relative_to(root))
    require(hashlib.sha256(restore.read_bytes()).hexdigest() == value['restoreSha256'])
    require(isinstance(value['images'], dict) and 1 <= len(value['images']) <= 32)
    for image, expected in value['images'].items():
        image = checked(image)
        with image.open('rb') as stream:
            digest = hashlib.sha256()
            for block in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(block)
        require(digest.hexdigest() == expected)
    require(set(value['modes']) == {'rust-import', 'rust', 'node-rollback'})
    require(isinstance(value['session'], str) and 0 < len(value['session']) <= 1024)
    require(value['modes']['rust-import']['executable'] == value['modes']['rust']['executable'])
    allowed = {'AGENT_UI_HARNESS', 'AGENT_UI_CWD', 'AGENT_UI_PUBLIC_DIR',
               'AGENT_UI_SESSION_ID', 'AGENT_UI_PORT', 'RUST_SPIKE_REGISTER',
               'RUST_CLAUDE_IMPORT_SNAPSHOT'}
    for name, command in value['modes'].items():
        require(set(command) == {'executable', 'args', 'env'})
        require(command['executable'] in value['images'])
        require(os.access(command['executable'], os.X_OK))
        require(isinstance(command['args'], list) and len(command['args']) <= 24)
        require(all(isinstance(arg, str) and '\0' not in arg and len(arg) <= 4096
                    for arg in command['args']))
        require(set(command['env']) <= allowed)
        require(all(isinstance(v, str) and '\0' not in v and len(v) <= 4096
                    for v in command['env'].values()))
        require(command['env'].get('AGENT_UI_HARNESS') == 'claude')
        require(command['env'].get('AGENT_UI_CWD') == str(cwd))
        require(command['env'].get('AGENT_UI_SESSION_ID') == value['session'])
        if name == 'node-rollback':
            require(command['args'] and command['args'][0] in value['images'])
            require(command['args'] == [command['args'][0], '--harness', 'claude',
                                       '--session-id', value['session'], '--cwd', str(cwd),
                                       '--name', 'Claude'])
        else:
            require(command['args'] == (['--import-claude-state'] if name == 'rust-import' else []))
        require(('--import-claude-state' in command['args']) == (name == 'rust-import'))
        require(('RUST_CLAUDE_IMPORT_SNAPSHOT' in command['env']) == (name == 'rust-import'))
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('manifest')
    parser.add_argument('mode', choices=['rust-import', 'rust', 'node-rollback'])
    parser.add_argument('--verify', action='store_true')
    args = parser.parse_args()
    value = binding(args.manifest)
    if args.verify:
        print('soul_continuation_binding_valid')
        return
    command = value['modes'][args.mode]
    env = dict(os.environ)
    # Keep Controller-issued identity/private-root/credentials inherited, never
    # record or manufacture them. Only the captured nonsecret Soul selectors
    # and selected launcher settings are overlaid.
    for name in ('CODEX_BIN', 'CLAUDE_BIN', 'RUST_CLAUDE_IMPORT_SNAPSHOT'):
        env.pop(name, None)
    env.update(HOME=value['home'], TREER_SOUL_ID=value['soulId'],
               TREER_SOUL_ROOT=value['soulRoot'], SOUL_RESTORE=value['restore'])
    env.update(command['env'])
    os.chdir(value['cwd'])
    os.execve(command['executable'], [command['executable'], *command['args']], env)


if __name__ == '__main__':
    try:
        main()
    except Exception:
        print('soul_continuation_refused', file=sys.stderr)
        sys.exit(2)
