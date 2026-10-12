#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
import re
import shlex
import shutil
import sys
import io
import sqlite3
import threading
import tarfile
import tempfile
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit, parse_qs, unquote
from http.cookies import SimpleCookie
import subprocess
from soul_auth import Auth, Error as AuthError


APP_ROOT = Path(__file__).resolve().parent
SOUL_ID_PATTERN = re.compile(r"soul_[0-9a-f]{32}")
ENV_NAME_PATTERN = re.compile(r"[A-Za-z_][A-Za-z0-9_]{0,127}")
SESSION_ID_PATTERN = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
)
MAX_MANIFEST_BYTES = 256 * 1024
MAX_ARCHIVE_MEMBERS = 256
MAX_JSON_BYTES = 64 * 1024
MAX_EXPANDED_BYTES = 128 * 1024 * 1024
PROTECTED_ENVIRONMENT = {
    "BASH_ENV",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "PYTHONSTARTUP",
    "RUBYOPT",
    "PERL5OPT",
    "ENV",
    "HOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NODE_OPTIONS",
    "PATH",
    "PYTHONPATH",
    "SHELL",
    "TMPDIR",
    "TREER_AGENT_ID",
    "TREER_AGENT_SERVER_URL",
    "TREER_BIN",
    "TREER_SERVER_ID",
    "TREER_SOUL_ID",
    "TREER_SOUL_ROOT",
    "TREER_WORKLOAD_CREDENTIAL",
    "TREER_WORKSPACE_ID",
}


class SoulError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def root_representation(accept: str, user_agent: str) -> str:
    choices: list[tuple[float, int, str]] = []
    for order, raw_entry in enumerate(accept.split(",")):
        parts = [part.strip().lower() for part in raw_entry.split(";")]
        media_type = parts[0]
        if media_type not in {"text/html", "text/markdown"}:
            continue
        quality = 1.0
        for parameter in parts[1:]:
            if parameter.startswith("q="):
                try:
                    quality = float(parameter[2:])
                except ValueError:
                    quality = 0.0
        if 0 < quality <= 1:
            choices.append((-quality, order, media_type))
    if choices:
        return min(choices)[2]
    return "text/html" if "mozilla/" in user_agent.lower() else "text/markdown"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_member_path(value: object) -> str:
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise SoulError(400, "invalid_path", "archive paths must be non-empty POSIX paths")
    path = PurePosixPath(value)
    if any(part in {"", ".", ".."} for part in value.rstrip("/").split("/")) or path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise SoulError(400, "invalid_path", f"unsafe archive path: {value}")
    return str(path)


def validate_manifest(value: object, archive_files: set[str]) -> dict[str, object]:
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise SoulError(400, "invalid_manifest", "manifest schema_version must be 1")
    name = value.get("name", "Soul")
    if not isinstance(name, str) or not name.strip() or len(name.strip()) > 120:
        raise SoulError(400, "invalid_manifest", "manifest name must contain 1-120 characters")
    environment = value.get("environment", {})
    if not isinstance(environment, dict) or len(environment) > 64:
        raise SoulError(400, "invalid_manifest", "environment must be an object with at most 64 entries")
    clean_environment: dict[str, str] = {}
    for key, file_path in environment.items():
        if not isinstance(key, str) or not ENV_NAME_PATTERN.fullmatch(key):
            raise SoulError(400, "invalid_environment", f"invalid environment variable name: {key}")
        if key in PROTECTED_ENVIRONMENT or key.startswith(("TREER_", "AGENT_UI_", "CODEX_UI_", "LD_", "DYLD_")):
            raise SoulError(400, "invalid_environment", f"protected environment variable: {key}")
        clean_path = validate_member_path(file_path)
        if clean_path not in archive_files:
            raise SoulError(400, "missing_file", f"environment path is absent from archive: {clean_path}")
        clean_environment[key] = clean_path

    adapter = value.get("adapter")
    clean_adapter = None
    if adapter is not None:
        if not isinstance(adapter, dict) or adapter.get('name') not in ('codex','claude','generic'):
            raise SoulError(400, 'invalid_adapter', 'Adapter must be codex, claude or generic')
        clean_adapter = {'name': adapter['name']}
        if adapter['name'] != 'generic':
            session_id = adapter.get('session_id')
            if not isinstance(session_id, str) or not SESSION_ID_PATTERN.fullmatch(session_id):
                raise SoulError(400, 'invalid_adapter', 'session_id must be a UUID')
            clean_adapter['session_id'] = session_id
            session_file = validate_member_path(adapter.get('session_file'))
            if session_file not in archive_files:
                raise SoulError(400, 'invalid_adapter', 'Session file is missing')
            clean_adapter['session_file'] = session_file
        for field in ('session_relative_path','shell_snapshot_file','project_instructions_file'):
            if field in adapter:
                path = validate_member_path(adapter[field])
                if field != 'session_relative_path' and path not in archive_files:
                    raise SoulError(400, 'invalid_adapter', 'Adapter file is missing')
                clean_adapter[field] = path
        if adapter['name'] == 'codex' and not clean_adapter.get('session_relative_path', '').endswith('.jsonl'):
            raise SoulError(400, 'invalid_adapter', 'Codex session_relative_path must end in .jsonl')
        if 'memory_files' in adapter:
            memory = adapter['memory_files']
            if not isinstance(memory, list) or len(memory) > 128:
                raise SoulError(400, 'invalid_adapter', 'Invalid memory files')
            clean_adapter['memory_files'] = []
            for item in memory:
                path = validate_member_path(item)
                if path not in archive_files or not path.startswith('files/memory/'):
                    raise SoulError(400, 'invalid_adapter', 'Memory file missing or outside files/memory')
                clean_adapter['memory_files'].append(path)
        for field in ('cli_version','cwd','shell_snapshot_name'):
            if field in adapter:
                text = adapter[field]
                if not isinstance(text, str) or len(text) > 4096 or '\x00' in text:
                    raise SoulError(400, 'invalid_adapter', 'Invalid adapter metadata')
                if field == 'shell_snapshot_name' and Path(text).name != text:
                    raise SoulError(400, 'invalid_adapter', 'Invalid snapshot name')
                clean_adapter[field] = text

    clean: dict[str, object] = {
        "schema_version": 1,
        "name": name.strip(),
        "environment": clean_environment,
    }
    if clean_adapter is not None:
        clean["adapter"] = clean_adapter
    return clean


def inspect_archive(path: Path) -> tuple[dict[str, object], list[dict[str, object]]]:
    try:
        archive = tarfile.open(path, "r:*")
    except (tarfile.TarError, OSError) as error:
        raise SoulError(400, "invalid_archive", "cannot read tar archive") from error
    with archive:
        files: dict[str, tarfile.TarInfo] = {}
        expanded_size = 0
        count = 0
        for member in archive:
            count += 1
            if count > MAX_ARCHIVE_MEMBERS:
                raise SoulError(400, 'too_many_files', 'Archive contains too many entries')
            if member.size < 0:
                raise SoulError(400, 'invalid_archive', 'Archive entry has invalid size')
            name = validate_member_path(member.name)
            if member.isdir():
                continue
            if not member.isfile():
                raise SoulError(400, "invalid_archive", f"archive entry must be a regular file: {name}")
            if name in files:
                raise SoulError(400, "invalid_archive", f"duplicate archive path: {name}")
            expanded_size += member.size
            if expanded_size > MAX_EXPANDED_BYTES:
                raise SoulError(413, "archive_too_large", f"expanded archive exceeds {MAX_EXPANDED_BYTES} bytes")
            files[name] = member
        manifest_member = files.get("manifest.json")
        if manifest_member is None or manifest_member.size > MAX_MANIFEST_BYTES:
            raise SoulError(400, "invalid_manifest", "archive must contain a small manifest.json")
        stream = archive.extractfile(manifest_member)
        if stream is None:
            raise SoulError(400, "invalid_manifest", "cannot read manifest.json")
        try:
            manifest_value = json.loads(stream.read().decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise SoulError(400, "invalid_manifest", "manifest.json is invalid") from error
        manifest = validate_manifest(manifest_value, set(files) - {"manifest.json"})
        file_records = [
            {"path": name, "size": member.size}
            for name, member in sorted(files.items())
            if name != "manifest.json"
        ]
        return manifest, file_records


# Scan raw bytes, including manifest and unbound files. Only aggregate counts escape.
from soul_secrets import scan_secrets


class SoulStore:
    """Single-process SQLite index plus immutable content-addressed file blobs."""
    def __init__(self, root: Path, max_count=10000, max_bytes=10 * 1024**3):
        import fcntl
        self.root = root
        self.max_count, self.max_bytes = max_count, max_bytes
        if max_count < 1 or max_bytes < 1:
            raise ValueError("owner quotas must be positive")
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.lock_file = (root / ".server.lock").open("a+")
        fcntl.flock(self.lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.lock = threading.RLock()
        self.souls = root / "souls"
        self.uploads = root / "uploads"
        self.blobs = root / "blobs"
        self.database = root / "index.sqlite3"
        legacy = sorted(self.souls.glob("soul_*.json")) if self.souls.exists() else []
        self.backup = None
        if legacy and not self.database.exists():
            self.backup = root.with_name(root.name + ".pre-v2-" + uuid.uuid4().hex)
            shutil.copytree(root, self.backup)
            print(f"Soul migration backup: {self.backup}", flush=True)
        self.uploads.mkdir(exist_ok=True, mode=0o700)
        self.blobs.mkdir(exist_ok=True, mode=0o700)
        with self.connect() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS snapshots (
                  id TEXT PRIMARY KEY, owner TEXT NOT NULL, name TEXT NOT NULL,
                  version INTEGER NOT NULL, metadata TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
                  UNIQUE(owner,name,version));
                CREATE TABLE IF NOT EXISTS refs (soul_id TEXT, digest TEXT, PRIMARY KEY(soul_id,digest));
                CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT);
                CREATE TABLE IF NOT EXISTS oauth (state TEXT PRIMARY KEY, verifier TEXT, expires REAL);
                CREATE TABLE IF NOT EXISTS sessions (secret TEXT PRIMARY KEY, token TEXT, expires REAL);
            ''')
        os.chmod(self.database, 0o600)
        try:
            with self.connect() as db:
                if not db.execute("SELECT 1 FROM state WHERE key='migration_complete'").fetchone():
                    db.execute("BEGIN IMMEDIATE")
                    for metadata_path in legacy:
                        old = json.loads(metadata_path.read_text())
                        soul_id = old['soul_id']
                        if not SOUL_ID_PATTERN.fullmatch(soul_id) or metadata_path.stem != soul_id:
                            raise ValueError("invalid legacy metadata")
                        self._save(db, self.souls / f"{soul_id}.tar", {'id': 'legacy'},
                                   {'visibility': 'workspace', 'allow_secrets': True}, soul_id, old['created_at'])
                    db.execute("INSERT INTO state VALUES('migration_complete','1')")
        except Exception:
            # Original archive/metadata files remain untouched on failed migration.
            # The operator can restore the announced complete backup before retrying.
            self.lock_file.close()
            raise
        # SQLite commit precedes removal: an interrupted cleanup is safe to resume.
        for p in legacy:
            p.unlink(missing_ok=True)
            p.with_suffix('.tar').unlink(missing_ok=True)
        self._gc()
        for p in self.uploads.iterdir():
            if p.is_file():
                p.unlink()

    def connect(self):
        db = sqlite3.connect(self.database, timeout=30)
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA secure_delete=ON")
        return db

    def close(self):
        self.lock_file.close()

    def temporary_upload(self):
        return tempfile.mkstemp(prefix='upload-', suffix='.tar', dir=self.uploads)

    def _put_blob(self, data):
        digest = hashlib.sha256(data).hexdigest()
        path = self.blobs / digest
        if not path.exists():
            descriptor, tmp_name = tempfile.mkstemp(dir=self.blobs, prefix='.blob-')
            try:
                with os.fdopen(descriptor, 'wb') as stream:
                    stream.write(data)
                    stream.flush()
                    os.fsync(stream.fileno())
                os.replace(tmp_name, path)
            finally:
                Path(tmp_name).unlink(missing_ok=True)
        return digest

    def _record(self, db, soul_id, include_deleted=False):
        row = db.execute('SELECT metadata, deleted FROM snapshots WHERE id=?', (soul_id,)).fetchone()
        if not row or (row['deleted'] and not include_deleted):
            raise SoulError(404, 'soul_not_found', 'soul not found')
        return json.loads(row['metadata']), bool(row['deleted'])

    @staticmethod
    def visible(metadata, actor):
        return (metadata['owner'] == actor['id'] or metadata['visibility'] == 'workspace'
                or actor['id'] in metadata['readers'])

    def _save(self, db, temporary, actor, options, soul_id=None, created_at=None):
        manifest, files = inspect_archive(temporary)
        name = options.get('name', manifest['name'])
        parent = options.get('parent_soul_id')
        if parent:
            parent_record, _ = self._record(db, parent)
            if not self.visible(parent_record, actor):
                raise SoulError(404, 'soul_not_found', 'soul not found')
            if parent_record['owner'] not in (actor['id'], 'legacy'):
                raise SoulError(403, 'forbidden', 'Parent must belong to caller or legacy lineage')
        contents, findings = [], {}
        with tarfile.open(temporary, 'r:*') as archive:
            for member in sorted(archive.getmembers(), key=lambda m: m.name):
                if not member.isfile():
                    continue
                data = archive.extractfile(member).read()
                for category, count in scan_secrets(data).items():
                    findings[category] = findings.get(category, 0) + count
                contents.append((validate_member_path(member.name), data))
        if findings and not options.get('allow_secrets', False):
            error = SoulError(422, 'secrets_detected', 'Upload contains possible secrets; redact or explicitly allow_secrets')
            error.details = {'findings': findings, 'count': sum(findings.values())}
            raise error
        size = sum(len(data) for _, data in contents)
        usage = self._usage(db, actor['id'])
        if actor['id'] != 'legacy' and (usage['count'] >= self.max_count or usage['bytes'] + size > self.max_bytes):
            raise SoulError(409, 'quota_exceeded', 'Owner snapshot quota exceeded')
        version = db.execute('SELECT COALESCE(MAX(version),0)+1 FROM snapshots WHERE owner=? AND name=?',
                             (actor['id'], name)).fetchone()[0]
        adapter = manifest.get('adapter', {})
        entries = [{'path': path, 'size': len(data), 'sha256': self._put_blob(data)} for path, data in contents]
        metadata = dict(soul_id=soul_id or 'soul_' + uuid.uuid4().hex, name=name, version=version,
                        owner=actor['id'], created_by=actor['id'], created_at=created_at or utc_now(),
                        visibility=options.get('visibility', 'private'), readers=options.get('readers', []),
                        parent_soul_id=parent, labels=options.get('labels', []),
                        harness=options.get('harness', adapter.get('name', 'generic')),
                        session_id=options.get('session_id', adapter.get('session_id')),
                        source_machine=options.get('source_machine'), size_bytes=size,
                        contains_secrets=bool(findings), secret_findings=findings,
                        manifest=manifest, files=files, blob_entries=entries)
        raw = self._archive_bytes(metadata)
        metadata['archive_size'] = len(raw)
        metadata['archive_sha256'] = hashlib.sha256(raw).hexdigest()
        db.execute('INSERT INTO snapshots(id,owner,name,version,metadata) VALUES(?,?,?,?,?)',
                   (metadata['soul_id'], actor['id'], name, version, json.dumps(metadata)))
        db.executemany('INSERT INTO refs VALUES(?,?)', [(metadata['soul_id'], digest)
                        for digest in {entry['sha256'] for entry in entries}])
        return self.public(metadata)

    @staticmethod
    def public(metadata):
        return {key: value for key, value in metadata.items() if key != 'blob_entries'}

    def save(self, temporary, actor, options):
        with self.lock:
            try:
                with self.connect() as db:
                    db.execute('BEGIN IMMEDIATE')
                    return self._save(db, temporary, actor, options)
            finally:
                self._gc()

    def get(self, soul_id, actor):
        with self.lock, self.connect() as db:
            metadata, _ = self._record(db, soul_id)
            if not self.visible(metadata, actor):
                raise SoulError(404, 'soul_not_found', 'soul not found')
            return self.public(metadata)

    def _usage(self, db, owner):
        row = db.execute("SELECT COUNT(*), COALESCE(SUM(json_extract(metadata,'$.size_bytes')),0) FROM snapshots WHERE owner=? AND deleted=0", (owner,)).fetchone()
        return dict(count=row[0], bytes=row[1], max_count=self.max_count, max_bytes=self.max_bytes)

    def usage(self, actor):
        with self.lock, self.connect() as db:
            return self._usage(db, actor['id'])

    def list(self, actor, filters):
        with self.lock, self.connect() as db:
            # Bound response size and never include invisible metadata/counts.
            values = [json.loads(row[0]) for row in db.execute('SELECT metadata FROM snapshots WHERE deleted=0')]
            values = [self.public(v) for v in values if self.visible(v, actor)
                      and all(v.get(k) == filters[k] for k in ('name', 'harness', 'owner') if k in filters)]
            values.sort(key=lambda v: (v['created_at'], v['soul_id']), reverse=True)
            offset, limit = filters.get('offset', 0), filters.get('limit', 50)
            return dict(souls=values[offset:offset+limit], total=len(values),
                        next_offset=offset+limit if offset+limit < len(values) else None,
                        usage=self._usage(db, actor['id']))

    def by_name(self, actor, name, filters):
        with self.lock, self.connect() as db:
            values = [json.loads(row[0]) for row in db.execute('SELECT metadata FROM snapshots WHERE name=? AND deleted=0', (name,))]
            values = [v for v in values if self.visible(v, actor)
                      and ('owner' not in filters or v['owner'] == filters['owner'])]
            version = filters.get('version', 'latest')
            if version != 'latest':
                values = [v for v in values if v['version'] == int(version)]
            # Within each owner prefer highest version, then most recent among owners.
            if version == 'latest':
                latest = {}
                for v in values:
                    if v['owner'] not in latest or v['version'] > latest[v['owner']]['version']:
                        latest[v['owner']] = v
                values = list(latest.values())
            if not values:
                raise SoulError(404, 'soul_not_found', 'soul not found')
            return self.public(max(values, key=lambda v: (v['created_at'], v['soul_id'])))

    def _archive_bytes(self, metadata):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w', format=tarfile.PAX_FORMAT) as archive:
            for entry in metadata['blob_entries']:
                data = (self.blobs / entry['sha256']).read_bytes()
                if hashlib.sha256(data).hexdigest() != entry['sha256']:
                    raise SoulError(500, 'blob_corrupt', 'Snapshot integrity check failed')
                info = tarfile.TarInfo(entry['path'])
                info.size, info.mode, info.mtime = len(data), 0o600, 0
                archive.addfile(info, io.BytesIO(data))
        return stream.getvalue()

    def archive(self, soul_id, actor):
        # Materialize under the lock; deletion cannot race blob reads.
        with self.lock, self.connect() as db:
            metadata, _ = self._record(db, soul_id)
            if not self.visible(metadata, actor):
                raise SoulError(404, 'soul_not_found', 'soul not found')
            return self._archive_bytes(metadata)

    def _gc(self):
        with self.connect() as db:
            referenced = {row[0] for row in db.execute('SELECT DISTINCT digest FROM refs')}
        for path in self.blobs.iterdir():
            if path.name not in referenced:
                path.unlink()

    def delete(self, soul_id, actor):
        with self.lock:
            with self.connect() as db:
                db.execute('BEGIN IMMEDIATE')
                metadata, deleted = self._record(db, soul_id, True)
                if metadata['owner'] != actor['id']:
                    if deleted or not self.visible(metadata, actor):
                        raise SoulError(404, 'soul_not_found', 'soul not found')
                    raise SoulError(403, 'forbidden', 'Only owner may delete a Soul')
                tombstone = {k: metadata[k] for k in ('soul_id', 'name', 'owner', 'version')}
                tombstone['deleted_at'] = metadata.get('deleted_at', utc_now())
                db.execute('UPDATE snapshots SET metadata=?,deleted=1 WHERE id=?', (json.dumps(tombstone), soul_id))
                db.execute('DELETE FROM refs WHERE soul_id=?', (soul_id,))
            self._gc()
            return {'soul_id': soul_id, 'deleted': True}

def validate_public_url(value: str) -> str:
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise ValueError("SOUL_PUBLIC_URL must be an http(s) URL without credentials")
    if parsed.query or parsed.fragment:
        raise ValueError("SOUL_PUBLIC_URL must not contain a query or fragment")
    return value.rstrip("/")


def bundled_client(client_path, public_url):
    """Serve one executable artifact, with small adapter modules embedded."""
    prelude = '#!/usr/bin/env python3\nfrom __future__ import annotations\nimport sys, types, os\n'
    for name in ('soul_secrets', 'soul_sessions'):
        source = (APP_ROOT / (name + '.py')).read_text()
        prelude += f'module = types.ModuleType({name!r})\nsys.modules[{name!r}] = module\nexec({source!r}, module.__dict__)\n'
    prelude += f'os.environ.setdefault("TREER_SOUL_URL", {public_url!r})\n'
    source = client_path.read_text().replace('from __future__ import annotations', '')
    return (prelude + source).encode()


def installer_script(public_url, client_sha256):
    # Download atomically to a private temporary directory; installed file is standalone.
    return f'''#!/bin/sh
set -eu
install_dir="${{TREER_SOUL_INSTALL_DIR:-${{HOME}}/.local/bin}}"
temporary="$(mktemp -d "${{TMPDIR:-/tmp}}/treer-soul-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT INT TERM
base_url={shlex.quote(public_url)}
expected_sha={shlex.quote(client_sha256)}
curl -fsSL "$base_url/client.py" -o "$temporary/treer-soul" </dev/null
actual_sha="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$temporary/treer-soul" </dev/null)"
[ "$actual_sha" = "$expected_sha" ] || {{ echo 'Soul client checksum mismatch' >&2; exit 1; }}
mkdir -p "$install_dir"
chmod 755 "$temporary/treer-soul"
# Same-directory temporary file makes the final rename atomic across filesystems.
installed_tmp="$(mktemp "$install_dir/.treer-soul.XXXXXX")"
cp "$temporary/treer-soul" "$installed_tmp"
chmod 755 "$installed_tmp"
mv "$installed_tmp" "$install_dir/treer-soul"
"$install_dir/treer-soul" --version </dev/null
'''


class SoulHTTPServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        store: SoulStore,
        public_url: str,
        client_path: Path,
        treer_bin: str,
        max_upload_bytes: int,
    ) -> None:
        self.store = store
        self.public_url = public_url
        self.explicit_public_url = bool(os.environ.get("SOUL_PUBLIC_URL"))
        self.client_path = client_path
        self.treer_bin = treer_bin
        self.auth = None
        self.auth_lock = threading.Lock()
        self.max_upload_bytes = max_upload_bytes
        self.client_bytes = bundled_client(client_path, public_url)
        self.client_sha256 = hashlib.sha256(self.client_bytes).hexdigest()
        super().__init__(address, SoulHandler)

    def identity_provider(self):
        with self.auth_lock:
            if self.auth is None:
                proxy = os.environ.get('SOUL_PROXY_URL') or os.environ.get('TREER_PROXY_URL')
                service = os.environ.get('SOUL_SERVICE_ID')
                workspace = os.environ.get('SOUL_WORKSPACE_ID') or os.environ.get('TREER_WORKSPACE_ID')
                public_url = self.public_url
                if not service:
                    try:
                        result = subprocess.run([self.treer_bin, 'app', 'list'], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10, check=True)
                        app = next(a for a in json.loads(result.stdout)['apps']
                                   if a['runtime_agent_id'] == os.environ.get('TREER_AGENT_ID'))
                        service, workspace = app['service_id'], app['workspace_id']
                        public_url = os.environ.get('SOUL_PUBLIC_URL') or app.get('public_url') or (proxy.rstrip('/') +
                            f"/api/workspaces/{workspace}/apps/{app['app_id']}/ui/")
                    except (OSError, ValueError, KeyError, TypeError, StopIteration, subprocess.SubprocessError):
                        raise SoulError(503, 'identity_not_configured', 'Configure Soul Proxy, service and workspace identity')
                if not proxy or not workspace:
                    raise SoulError(503, 'identity_not_configured', 'Configure Soul Proxy and workspace identity')
                self.auth = Auth(self.store, proxy, service, workspace, public_url)
                if not self.explicit_public_url:
                    self.public_url = public_url.rstrip('/')
                    self.client_bytes = bundled_client(self.client_path, self.public_url)
                    self.client_sha256 = hashlib.sha256(self.client_bytes).hexdigest()
            return self.auth

    def server_close(self):
        super().server_close()
        self.store.close()


class SoulHandler(BaseHTTPRequestHandler):
    server: SoulHTTPServer

    def _actor(self, mutation=False):
        auth = self.server.identity_provider()
        cookie = SimpleCookie()
        try:
            cookie.load(self.headers.get('Cookie', ''))
            raw = cookie['soul_session'].value if 'soul_session' in cookie else ''
        except Exception:
            raw = ''
        token, principal, directory, cookie_auth = auth.authenticate(self.headers, raw)
        if mutation:
            auth.check_browser_mutation(self.headers, principal, cookie_auth)
        return principal, directory

    def _options(self, kind):
        try:
            query = parse_qs(urlsplit(self.path).query, keep_blank_values=True, max_num_fields=100)
        except ValueError:
            raise SoulError(400, 'invalid_request', 'Too many query parameters')
        allowed = {'upload': {'name','visibility','reader','label','parent_soul_id','harness','session_id','source_machine','allow_secrets'},
                   'list': {'name','harness','owner','offset','limit'}, 'latest': {'version','owner'}}[kind]
        if set(query) - allowed:
            raise SoulError(400, 'invalid_request', 'Unknown query option')
        result = {}
        for key, values in query.items():
            if key not in ('reader','label') and len(values) != 1:
                raise SoulError(400, 'invalid_request', 'Duplicate query option')
            if any(not v or len(v) > 256 for v in values):
                raise SoulError(400, 'invalid_request', 'Query values must contain 1-256 characters')
            result[key] = values if key in ('reader','label') else values[0]
        if kind == 'upload':
            if result.get('visibility', 'private') not in ('private','workspace'):
                raise SoulError(400, 'invalid_request', 'Invalid visibility')
            if result.get('allow_secrets', 'false') not in ('true','false'):
                raise SoulError(400, 'invalid_request', 'allow_secrets must be true or false')
            result['allow_secrets'] = result.get('allow_secrets') == 'true'
            result['readers'] = sorted(set(result.pop('reader', [])))
            result['labels'] = sorted(set(result.pop('label', [])))
            if len(result['readers']) > 64 or len(result['labels']) > 32 or len(result.get('name', '')) > 120:
                raise SoulError(400, 'invalid_request', 'Too many readers/labels or name too long')
            if 'parent_soul_id' in result and not SOUL_ID_PATTERN.fullmatch(result['parent_soul_id']):
                raise SoulError(400, 'invalid_request', 'Invalid parent Soul ID')
        for key in ('offset','limit','version'):
            if key in result and result[key] != 'latest':
                try:
                    result[key] = int(result[key])
                    if result[key] < (0 if key == 'offset' else 1) or (key == 'limit' and result[key] > 100):
                        raise ValueError()
                except ValueError:
                    raise SoulError(400, 'invalid_request', 'Invalid pagination/version')
            elif key in result and key != 'version':
                raise SoulError(400, 'invalid_request', 'Invalid pagination')
        return result

    def _redirect(self, target, cookie=None):
        self.send_response(303)
        self.send_header('Location', target)
        if cookie:
            self.send_header('Set-Cookie', cookie)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', '0')
        self.end_headers()

    def do_DELETE(self):
        try:
            actor, _ = self._actor(True)
            match = re.fullmatch(r'/v1/souls/(soul_[0-9a-f]{32})/?', urlsplit(self.path).path)
            if not match:
                raise SoulError(404, 'not_found', 'route not found')
            self._json(200, self.server.store.delete(match[1], actor))
        except (SoulError, AuthError) as error:
            self._error(error)

    def do_HEAD(self) -> None:  # noqa: N802
        self._dispatch(head=True)

    def do_GET(self) -> None:  # noqa: N802
        self._dispatch(head=False)

    def do_POST(self) -> None:  # noqa: N802
        try:
            path = urlsplit(self.path).path.rstrip("/") or "/"
            if path == "/v1/souls":
                self._upload()
                return
            match = re.fullmatch(r"/v1/souls/(soul_[0-9a-f]{32})/incarnations", path)
            if match:
                self._incarnate(match.group(1))
                return
            raise SoulError(404, "not_found", "route not found")
        except (SoulError, AuthError) as error:
            self._error(error)
        except Exception as error:  # noqa: BLE001
            print("soul request failed", file=sys.stderr)
            self._error(SoulError(500, "internal_error", "internal server error"))

    def _dispatch(self, head: bool) -> None:
        try:
            path = urlsplit(self.path).path.rstrip("/") or "/"
            if path == "/":
                representation = root_representation(
                    self.headers.get("Accept", ""), self.headers.get("User-Agent", "")
                )
                target = APP_ROOT / ("web/index.html" if representation == "text/html" else "AGENT.md")
                self._file(target, f"{representation}; charset=utf-8", head=head, vary=True)
                return
            if path == "/app.css":
                self._file(APP_ROOT / "web" / "app.css", "text/css; charset=utf-8", head=head)
                return
            if path == "/app.js":
                self._file(APP_ROOT / "web" / "app.js", "text/javascript; charset=utf-8", head=head)
                return
            if path == "/health":
                self._json(200, {"ok": True}, head=head)
                return
            if path in ("/install.sh", "/client.py", "/client.py.sha256") and self.server.auth is None:
                self.server.identity_provider()
            if path == "/install.sh":
                self._bytes(
                    200,
                    installer_script(self.server.public_url, self.server.client_sha256).encode(),
                    "text/x-shellscript; charset=utf-8",
                    head=head,
                )
                return
            if path == "/client.py":
                self._bytes(200, self.server.client_bytes, "text/x-python; charset=utf-8", head=head)
                return
            if path == "/client.py.sha256":
                self._bytes(200, f"{self.server.client_sha256}  client.py\n".encode(), "text/plain", head=head)
                return
            if path == '/api/auth/login':
                self._redirect(self.server.identity_provider().start())
                return
            if path == '/api/auth/callback':
                auth = self.server.identity_provider()
                query = parse_qs(urlsplit(self.path).query)
                raw, age = auth.finish(query.get('code', [''])[0], query.get('state', [''])[0])
                cookie_path = urlsplit(auth.public_url).path.rstrip('/') + '/'
                secure = '; Secure' if auth.public_url.startswith('https://') else ''
                self._redirect(auth.public_url, f'soul_session={raw}; Path={cookie_path}; HttpOnly; SameSite=Lax; Max-Age={age}{secure}')
                return
            if path.startswith('/v1/'):
                actor, _ = self._actor()
            if path == '/v1/me':
                self._json(200, {'principal': actor, 'usage': self.server.store.usage(actor)}, head=head)
                return
            if path.startswith('/v1/souls/by-name/'):
                name = unquote(path[len('/v1/souls/by-name/'):])
                self._json(200, self.server.store.by_name(actor, name, self._options('latest')), head=head)
                return
            if path == "/v1/souls":
                self._json(200, self.server.store.list(actor, self._options('list')), head=head)
                return
            archive_match = re.fullmatch(r"/v1/souls/(soul_[0-9a-f]{32})/archive", path)
            if archive_match:
                self._bytes(200, self.server.store.archive(archive_match.group(1), actor), "application/x-tar", head=head)
                return
            soul_match = re.fullmatch(r"/v1/souls/(soul_[0-9a-f]{32})", path)
            if soul_match:
                self._json(200, self.server.store.get(soul_match.group(1), actor), head=head)
                return
            raise SoulError(404, "not_found", "route not found")
        except (SoulError, AuthError) as error:
            self._error(error, head=head)
        except Exception as error:  # noqa: BLE001
            print("soul request failed", file=sys.stderr)
            self._error(SoulError(500, "internal_error", "internal server error"), head=head)

    def _upload(self) -> None:
        actor, directory = self._actor(True)
        options = self._options('upload')
        if not set(options['readers']) <= {p['id'] for p in directory}:
            raise SoulError(400, 'reader_unavailable', 'Readers must be current workspace principals')
        if self.headers.get_content_type() not in {"application/x-tar", "application/octet-stream"}:
            raise SoulError(415, "unsupported_media_type", "upload an application/x-tar body")
        length = self._content_length(self.server.max_upload_bytes)
        descriptor, temporary_name = self.server.store.temporary_upload()
        temporary = Path(temporary_name)
        try:
            with os.fdopen(descriptor, "wb") as stream:
                remaining = length
                while remaining:
                    chunk = self.rfile.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise SoulError(400, "short_upload", "request body ended before Content-Length")
                    stream.write(chunk)
                    remaining -= len(chunk)
                stream.flush()
                os.fsync(stream.fileno())
            metadata = self.server.store.save(temporary, actor, options)
        finally:
            temporary.unlink(missing_ok=True)
        self._json(201, metadata)

    def _incarnate(self, soul_id):
        self._actor(True)
        raise SoulError(410, 'incarnate_deprecated',
                        'Use the v2 client: treer agent admin create must run with caller authority')

    def _content_length(self, limit: int) -> int:
        try:
            length = int(self.headers.get("Content-Length", ""))
        except ValueError as error:
            raise SoulError(411, "content_length_required", "valid Content-Length is required") from error
        if length <= 0:
            raise SoulError(411, "content_length_required", "positive Content-Length is required")
        if length > limit:
            raise SoulError(413, "upload_too_large", f"request exceeds {limit} bytes")
        return length

    def _read_json(self) -> dict[str, object]:
        length = self._content_length(MAX_JSON_BYTES)
        try:
            value = json.loads(self.rfile.read(length))
        except json.JSONDecodeError as error:
            raise SoulError(400, "invalid_json", f"invalid JSON: {error}") from error
        if not isinstance(value, dict):
            raise SoulError(400, "invalid_json", "request body must be an object")
        return value

    def _file(self, path: Path, content_type: str, head: bool = False, vary: bool = False) -> None:
        try:
            size = path.stat().st_size
        except FileNotFoundError as error:
            raise SoulError(404, "not_found", "file not found") from error
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(size))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
        if content_type.startswith("text/html"):
            self.send_header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'",
            )
        self.end_headers()
        if not head:
            with path.open("rb") as stream:
                shutil.copyfileobj(stream, self.wfile)

    def _bytes(self, status: int, body: bytes, content_type: str, head: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def _json(self, status: int, value: object, head: bool = False) -> None:
        self._bytes(status, (json.dumps(value, sort_keys=True) + "\n").encode(), "application/json", head=head)

    def _error(self, error: SoulError, head: bool = False) -> None:
        self._json(error.status, {"error": {"code": error.code, "message": error.message, **getattr(error, "details", {})}}, head=head)

    def log_message(self, format: str, *args: object) -> None:
        print(f"soul {self.address_string()} {format % args}", file=sys.stderr)


def parse_listen(value: str) -> tuple[str, int]:
    host, separator, port_text = value.rpartition(":")
    if not separator or not host:
        raise ValueError("SOUL_LISTEN must be HOST:PORT")
    port = int(port_text)
    if not 0 <= port <= 65535:
        raise ValueError("SOUL_LISTEN port is invalid")
    return host, port


def main() -> None:
    listen = parse_listen(os.environ.get("SOUL_LISTEN", "127.0.0.1:9420"))
    data_dir = Path(os.environ.get("SOUL_DATA_DIR", ".treer/apps/soul")).expanduser().resolve()
    public_url = validate_public_url(os.environ.get("SOUL_PUBLIC_URL", "http://soul.internal"))
    client_path = Path(os.environ.get("SOUL_CLIENT_PATH", APP_ROOT / "client.py")).resolve()
    treer_bin = os.environ.get("TREER_BIN", "treer")
    max_upload_bytes = int(os.environ.get("SOUL_MAX_UPLOAD_BYTES", str(64 * 1024 * 1024)))
    server = SoulHTTPServer(listen, SoulStore(data_dir, int(os.environ.get("SOUL_OWNER_MAX_COUNT", "10000")),
                                        int(os.environ.get("SOUL_OWNER_MAX_BYTES", str(10 * 1024**3)))), public_url, client_path, treer_bin, max_upload_bytes)
    print(f"Treer Soul listening on http://{server.server_address[0]}:{server.server_address[1]}")
    print(f"workspace URL: {public_url}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
