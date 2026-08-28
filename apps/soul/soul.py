#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit


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
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
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
        if key in PROTECTED_ENVIRONMENT or key.startswith("TREER_"):
            raise SoulError(400, "invalid_environment", f"protected environment variable: {key}")
        clean_path = validate_member_path(file_path)
        if clean_path not in archive_files:
            raise SoulError(400, "missing_file", f"environment path is absent from archive: {clean_path}")
        clean_environment[key] = clean_path

    adapter = value.get("adapter")
    clean_adapter: dict[str, object] | None = None
    if adapter is not None:
        if not isinstance(adapter, dict) or adapter.get("name") != "codex":
            raise SoulError(400, "invalid_adapter", "only the codex adapter is supported")
        session_id = adapter.get("session_id")
        if not isinstance(session_id, str) or not SESSION_ID_PATTERN.fullmatch(session_id):
            raise SoulError(400, "invalid_adapter", "codex session_id must be a UUID")
        session_file = validate_member_path(adapter.get("session_file"))
        session_relative_path = validate_member_path(adapter.get("session_relative_path"))
        if session_file not in archive_files or not session_relative_path.endswith(".jsonl"):
            raise SoulError(400, "invalid_adapter", "codex session file is missing or invalid")
        clean_adapter = {
            "name": "codex",
            "session_id": session_id,
            "session_file": session_file,
            "session_relative_path": session_relative_path,
        }
        for optional in ("cli_version", "cwd"):
            item = adapter.get(optional)
            if isinstance(item, str) and item:
                clean_adapter[optional] = item
        shell_file = adapter.get("shell_snapshot_file")
        shell_name = adapter.get("shell_snapshot_name")
        if shell_file is not None or shell_name is not None:
            clean_shell_file = validate_member_path(shell_file)
            if clean_shell_file not in archive_files:
                raise SoulError(400, "invalid_adapter", "codex shell snapshot is missing")
            if not isinstance(shell_name, str) or Path(shell_name).name != shell_name:
                raise SoulError(400, "invalid_adapter", "codex shell snapshot name is invalid")
            clean_adapter["shell_snapshot_file"] = clean_shell_file
            clean_adapter["shell_snapshot_name"] = shell_name

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
        raise SoulError(400, "invalid_archive", f"cannot read tar archive: {error}") from error
    with archive:
        members = archive.getmembers()
        if len(members) > MAX_ARCHIVE_MEMBERS:
            raise SoulError(400, "too_many_files", f"archive may contain at most {MAX_ARCHIVE_MEMBERS} entries")
        files: dict[str, tarfile.TarInfo] = {}
        expanded_size = 0
        for member in members:
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
            raise SoulError(400, "invalid_manifest", f"manifest.json is invalid: {error}") from error
        manifest = validate_manifest(manifest_value, set(files) - {"manifest.json"})
        file_records = [
            {"path": name, "size": member.size}
            for name, member in sorted(files.items())
            if name != "manifest.json"
        ]
        return manifest, file_records


class SoulStore:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.souls = root / "souls"
        self.uploads = root / "uploads"
        self.souls.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.uploads.mkdir(parents=True, exist_ok=True, mode=0o700)

    def temporary_upload(self) -> tuple[int, Path]:
        return tempfile.mkstemp(prefix="upload-", suffix=".tar", dir=self.uploads)

    def save(self, temporary: Path) -> dict[str, object]:
        manifest, files = inspect_archive(temporary)
        soul_id = f"soul_{uuid.uuid4().hex}"
        archive_path = self.souls / f"{soul_id}.tar"
        metadata_path = self.souls / f"{soul_id}.json"
        metadata: dict[str, object] = {
            "soul_id": soul_id,
            "created_at": utc_now(),
            "archive_size": temporary.stat().st_size,
            "archive_sha256": sha256_file(temporary),
            "manifest": manifest,
            "files": files,
        }
        os.chmod(temporary, 0o600)
        os.replace(temporary, archive_path)
        temporary_metadata = metadata_path.with_suffix(f".{uuid.uuid4().hex}.tmp")
        temporary_metadata.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        os.chmod(temporary_metadata, 0o600)
        os.replace(temporary_metadata, metadata_path)
        return metadata

    def get(self, soul_id: str) -> dict[str, object]:
        if not SOUL_ID_PATTERN.fullmatch(soul_id):
            raise SoulError(404, "soul_not_found", "soul not found")
        path = self.souls / f"{soul_id}.json"
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError as error:
            raise SoulError(404, "soul_not_found", "soul not found") from error

    def list(self) -> list[dict[str, object]]:
        values = []
        for path in self.souls.glob("soul_*.json"):
            try:
                values.append(json.loads(path.read_text(encoding="utf-8")))
            except (OSError, json.JSONDecodeError):
                continue
        values.sort(key=lambda value: str(value.get("created_at", "")), reverse=True)
        return values

    def archive(self, soul_id: str) -> Path:
        self.get(soul_id)
        return self.souls / f"{soul_id}.tar"


def validate_public_url(value: str) -> str:
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise ValueError("SOUL_PUBLIC_URL must be an http(s) URL without credentials")
    if parsed.query or parsed.fragment or parsed.path not in {"", "/"}:
        raise ValueError("SOUL_PUBLIC_URL must contain only an origin")
    return value.rstrip("/")


def installer_script(public_url: str, client_sha256: str) -> str:
    quoted_url = shlex.quote(public_url)
    quoted_sha = shlex.quote(client_sha256)
    return f"""#!/bin/sh
set -eu

install_dir="${{TREER_SOUL_INSTALL_DIR:-${{HOME}}/.local/bin}}"
libexec_dir="${{TREER_SOUL_LIBEXEC_DIR:-${{HOME}}/.local/libexec/treer-soul}}"
temporary="$(mktemp -d "${{TMPDIR:-/tmp}}/treer-soul-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT INT TERM

command -v curl >/dev/null 2>&1 || {{ echo 'curl is required' >&2; exit 1; }}
command -v python3 >/dev/null 2>&1 || {{ echo 'python3 is required' >&2; exit 1; }}

base_url={quoted_url}
expected_sha={quoted_sha}
curl -fsSL "$base_url/client.py" -o "$temporary/client.py"
if command -v sha256sum >/dev/null 2>&1; then
  actual_sha="$(sha256sum "$temporary/client.py" | awk '{{print $1}}')"
elif command -v shasum >/dev/null 2>&1; then
  actual_sha="$(shasum -a 256 "$temporary/client.py" | awk '{{print $1}}')"
else
  echo 'sha256sum or shasum is required' >&2
  exit 1
fi
if [ "$actual_sha" != "$expected_sha" ]; then
  echo 'treer-soul client checksum mismatch' >&2
  exit 1
fi

mkdir -p "$install_dir" "$libexec_dir"
chmod 700 "$libexec_dir"
cp "$temporary/client.py" "$libexec_dir/client.py"
chmod 755 "$libexec_dir/client.py"
{{
  echo '#!/bin/sh'
  printf '%s\n' "DEFAULT_SOUL_URL=$base_url"
  echo 'export TREER_SOUL_URL="${{TREER_SOUL_URL:-$DEFAULT_SOUL_URL}}"'
  printf 'exec python3 "%s/client.py" "$@"\n' "$libexec_dir"
}} > "$temporary/treer-soul"
chmod 755 "$temporary/treer-soul"
mv "$temporary/treer-soul" "$install_dir/treer-soul"
echo "installed treer-soul to $install_dir/treer-soul"
"$install_dir/treer-soul" --version
"""


LAUNCH_SCRIPT = """set -eu
base_url=$1
soul_id=$2
shift 2
client_file=$(mktemp "${TMPDIR:-/tmp}/treer-soul-client.XXXXXX.py")
trap 'rm -f "$client_file"' EXIT INT TERM
curl -fsSL "$base_url/client.py" -o "$client_file"
python3 "$client_file" --server "$base_url" run "$soul_id" -- "$@"
"""


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
        self.client_path = client_path
        self.treer_bin = treer_bin
        self.max_upload_bytes = max_upload_bytes
        self.client_bytes = client_path.read_bytes()
        self.client_sha256 = hashlib.sha256(self.client_bytes).hexdigest()
        super().__init__(address, SoulHandler)


class SoulHandler(BaseHTTPRequestHandler):
    server: SoulHTTPServer

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
        except SoulError as error:
            self._error(error)
        except Exception as error:  # noqa: BLE001
            print(f"soul request failed: {error}", file=sys.stderr)
            self._error(SoulError(500, "internal_error", "internal server error"))

    def _dispatch(self, head: bool) -> None:
        try:
            path = urlsplit(self.path).path.rstrip("/") or "/"
            if path == "/":
                self._file(APP_ROOT / "web" / "index.html", "text/html; charset=utf-8", head=head)
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
            if path == "/v1/souls":
                self._json(200, {"souls": self.server.store.list()}, head=head)
                return
            archive_match = re.fullmatch(r"/v1/souls/(soul_[0-9a-f]{32})/archive", path)
            if archive_match:
                self._file(self.server.store.archive(archive_match.group(1)), "application/x-tar", head=head)
                return
            soul_match = re.fullmatch(r"/v1/souls/(soul_[0-9a-f]{32})", path)
            if soul_match:
                self._json(200, self.server.store.get(soul_match.group(1)), head=head)
                return
            raise SoulError(404, "not_found", "route not found")
        except SoulError as error:
            self._error(error, head=head)
        except Exception as error:  # noqa: BLE001
            print(f"soul request failed: {error}", file=sys.stderr)
            self._error(SoulError(500, "internal_error", "internal server error"), head=head)

    def _upload(self) -> None:
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
            metadata = self.server.store.save(temporary)
        finally:
            temporary.unlink(missing_ok=True)
        self._json(201, metadata)

    def _incarnate(self, soul_id: str) -> None:
        metadata = self.server.store.get(soul_id)
        body = self._read_json()
        machine = body.get("machine", "self")
        name = body.get("name", f"{metadata['manifest']['name']}-reborn")
        cwd = body.get("cwd", ".")
        command = body.get("command", [])
        if not isinstance(machine, str) or not machine or len(machine) > 160:
            raise SoulError(400, "invalid_request", "machine must be a non-empty string")
        if not isinstance(name, str) or not name.strip() or len(name.strip()) > 80:
            raise SoulError(400, "invalid_request", "name must contain 1-80 characters")
        if not isinstance(cwd, str) or not cwd or len(cwd) > 4096:
            raise SoulError(400, "invalid_request", "cwd must be a non-empty string")
        if not isinstance(command, list) or len(command) > 128 or any(
            not isinstance(item, str) or "\x00" in item or len(item) > 8192 for item in command
        ):
            raise SoulError(400, "invalid_request", "command must be an array of strings")
        adapter = metadata.get("manifest", {}).get("adapter")
        if not command and not (isinstance(adapter, dict) and adapter.get("name") == "codex"):
            raise SoulError(400, "command_required", "generic souls require an incarnation command")
        argv = [
            self.server.treer_bin,
            "agent",
            "admin",
            "create",
            "--machine",
            machine,
            "--kind",
            "command",
            "--name",
            name.strip(),
            "--cwd",
            cwd,
            "--",
            "sh",
            "-c",
            LAUNCH_SCRIPT,
            "treer-soul-incarnation",
            self.server.public_url,
            soul_id,
            *command,
        ]
        try:
            result = subprocess.run(argv, capture_output=True, text=True, timeout=30, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise SoulError(502, "incarnation_failed", f"failed to invoke treer: {error}") from error
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip()[-2000:]
            raise SoulError(502, "incarnation_failed", detail or "treer agent creation failed")
        try:
            agent = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise SoulError(502, "incarnation_failed", "treer returned invalid JSON") from error
        self._json(201, {"soul_id": soul_id, "agent": agent})

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

    def _file(self, path: Path, content_type: str, head: bool = False) -> None:
        try:
            size = path.stat().st_size
        except FileNotFoundError as error:
            raise SoulError(404, "not_found", "file not found") from error
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(size))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
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
        self._json(error.status, {"error": {"code": error.code, "message": error.message}}, head=head)

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
    server = SoulHTTPServer(listen, SoulStore(data_dir), public_url, client_path, treer_bin, max_upload_bytes)
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
