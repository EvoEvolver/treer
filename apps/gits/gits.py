#!/usr/bin/env python3
"""Small workspace-local Git hosting service over Git Smart HTTP."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlsplit


APP_ROOT = Path(__file__).resolve().parent
REPOSITORY_NAME = re.compile(r"[a-z0-9][a-z0-9._-]{0,62}")
GIT_ROUTE = re.compile(r"/git/([a-z0-9][a-z0-9._-]{0,62})\.git(/.*)")
API_REPOSITORY_ROUTE = re.compile(r"/v1/repos/([a-z0-9][a-z0-9._-]{0,62})")
MAX_JSON_BYTES = 64 * 1024
MAX_DESCRIPTION_LENGTH = 240
GIT_TIMEOUT_SECONDS = 300


class GitsError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def validate_name(value: object) -> str:
    if not isinstance(value, str) or not REPOSITORY_NAME.fullmatch(value):
        raise GitsError(
            400,
            "invalid_repository_name",
            "repository name must use 1-63 lowercase letters, digits, dots, dashes, or underscores",
        )
    if value in {".", ".."} or value.endswith(".git"):
        raise GitsError(400, "invalid_repository_name", "repository name is reserved")
    return value


def validate_description(value: object) -> str:
    if value is None:
        return ""
    if not isinstance(value, str) or len(value) > MAX_DESCRIPTION_LENGTH:
        raise GitsError(
            400,
            "invalid_description",
            f"description must be a string with at most {MAX_DESCRIPTION_LENGTH} characters",
        )
    if "\n" in value or "\r" in value:
        raise GitsError(400, "invalid_description", "description must be one line")
    return value.strip()


class RepositoryStore:
    def __init__(self, root: Path, public_url: str, git_bin: str) -> None:
        self.root = root
        self.public_url = public_url.rstrip("/")
        self.git_bin = git_bin
        self.lock = threading.Lock()
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)

    def create(self, name_value: object, description_value: object) -> dict[str, Any]:
        name = validate_name(name_value)
        description = validate_description(description_value)
        path = self.root / f"{name}.git"
        with self.lock:
            if path.exists():
                raise GitsError(409, "repository_exists", "repository already exists")
            try:
                self._run(["init", "--bare", "--initial-branch=main", str(path)])
                self._run(["--git-dir", str(path), "config", "http.receivepack", "true"])
                self._run(["--git-dir", str(path), "config", "http.uploadpack", "true"])
                (path / "description").write_text(description + "\n", encoding="utf-8")
                self._write_metadata(path, {"schema_version": 1, "created_at": utc_now()})
            except Exception:
                if path.exists():
                    shutil.rmtree(path)
                raise
        return self.get(name)

    def list(self) -> list[dict[str, Any]]:
        repositories = []
        for path in sorted(self.root.glob("*.git"), key=lambda item: item.name):
            if path.is_dir() and REPOSITORY_NAME.fullmatch(path.name[:-4]):
                repositories.append(self._describe(path, include_commits=False))
        return repositories

    def get(self, name_value: object) -> dict[str, Any]:
        name = validate_name(name_value)
        path = self.root / f"{name}.git"
        if not path.is_dir():
            raise GitsError(404, "repository_not_found", "repository not found")
        return self._describe(path, include_commits=True)

    def path(self, name_value: object) -> Path:
        name = validate_name(name_value)
        path = self.root / f"{name}.git"
        if not path.is_dir():
            raise GitsError(404, "repository_not_found", "repository not found")
        return path

    def _describe(self, path: Path, include_commits: bool) -> dict[str, Any]:
        name = path.name[:-4]
        metadata = self._read_metadata(path)
        branches = self._branches(path)
        created_at = metadata.get("created_at")
        if not isinstance(created_at, str):
            created_at = datetime.fromtimestamp(path.stat().st_mtime, timezone.utc).isoformat()
        description_path = path / "description"
        description = description_path.read_text(encoding="utf-8").strip()
        updated_at = branches[0]["committed_at"] if branches else created_at
        result: dict[str, Any] = {
            "repo_id": f"repo_{hashlib.sha256(name.encode()).hexdigest()[:16]}",
            "name": name,
            "description": description,
            "default_branch": "main",
            "clone_url": f"{self.public_url}/git/{name}.git",
            "branch_count": len(branches),
            "branches": branches,
            "created_at": created_at,
            "updated_at": updated_at,
        }
        if include_commits:
            result["recent_commits"] = self._recent_commits(path)
        return result

    def _branches(self, path: Path) -> list[dict[str, str]]:
        output = self._run(
            [
                "--git-dir",
                str(path),
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)%00%(objectname)%00%(committerdate:iso-strict)",
                "refs/heads",
            ]
        ).stdout
        branches = []
        for line in output.splitlines():
            parts = line.split("\0")
            if len(parts) == 3:
                branches.append({"name": parts[0], "commit": parts[1], "committed_at": parts[2]})
        return branches

    def _recent_commits(self, path: Path) -> list[dict[str, str]]:
        completed = self._run(
            [
                "--git-dir",
                str(path),
                "log",
                "--all",
                "--max-count=20",
                "--format=%H%x00%h%x00%an%x00%aI%x00%s",
            ],
            check=False,
        )
        if completed.returncode != 0:
            return []
        commits = []
        for line in completed.stdout.splitlines():
            parts = line.split("\0", 4)
            if len(parts) == 5:
                commits.append(
                    {
                        "commit": parts[0],
                        "short_commit": parts[1],
                        "author": parts[2],
                        "committed_at": parts[3],
                        "subject": parts[4],
                    }
                )
        return commits

    def _run(self, arguments: list[str], check: bool = True) -> subprocess.CompletedProcess[str]:
        completed = subprocess.run(
            [self.git_bin, *arguments],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if check and completed.returncode != 0:
            detail = completed.stderr.strip() or "git command failed"
            raise RuntimeError(detail)
        return completed

    @staticmethod
    def _metadata_path(path: Path) -> Path:
        return path / "gits-metadata.json"

    def _write_metadata(self, path: Path, value: dict[str, Any]) -> None:
        destination = self._metadata_path(path)
        temporary = destination.with_name(f".{destination.name}.{uuid.uuid4().hex}.tmp")
        temporary.write_text(json.dumps(value, separators=(",", ":")) + "\n", encoding="utf-8")
        os.replace(temporary, destination)

    def _read_metadata(self, path: Path) -> dict[str, Any]:
        try:
            value = json.loads(self._metadata_path(path).read_text(encoding="utf-8"))
            return value if isinstance(value, dict) else {}
        except (FileNotFoundError, ValueError):
            return {}


class GitsServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        store: RepositoryStore,
        max_git_body: int,
    ) -> None:
        self.store = store
        self.max_git_body = max_git_body
        super().__init__(address, GitsHandler)


class GitsHandler(BaseHTTPRequestHandler):
    server: GitsServer

    def do_HEAD(self) -> None:  # noqa: N802
        self._dispatch(head=True)

    def do_GET(self) -> None:  # noqa: N802
        self._dispatch(head=False)

    def do_POST(self) -> None:  # noqa: N802
        self._dispatch(head=False)

    def _dispatch(self, head: bool) -> None:
        try:
            parsed = urlsplit(self.path)
            path = unquote(parsed.path).rstrip("/") or "/"
            if self.command in {"GET", "HEAD"} and path == "/":
                self._file(APP_ROOT / "AGENT.md", "text/markdown; charset=utf-8", head)
                return
            if self.command in {"GET", "HEAD"} and path == "/_human":
                self._file(APP_ROOT / "web" / "index.html", "text/html; charset=utf-8", head)
                return
            if self.command in {"GET", "HEAD"} and path == "/_human/app.css":
                self._file(APP_ROOT / "web" / "app.css", "text/css; charset=utf-8", head)
                return
            if self.command in {"GET", "HEAD"} and path == "/_human/app.js":
                self._file(APP_ROOT / "web" / "app.js", "text/javascript; charset=utf-8", head)
                return
            if self.command in {"GET", "HEAD"} and path == "/health":
                self._json(200, {"status": "ok", "service": "gits"}, head=head)
                return
            if self.command in {"GET", "HEAD"} and path == "/v1/repos":
                self._json(200, {"repos": self.server.store.list()}, head=head)
                return
            if self.command == "POST" and path == "/v1/repos":
                request = self._json_body()
                unknown = set(request) - {"name", "description"}
                if unknown:
                    raise GitsError(400, "unknown_field", f"unknown field: {sorted(unknown)[0]}")
                repository = self.server.store.create(request.get("name"), request.get("description"))
                self._json(201, {"repo": repository})
                return
            api_match = API_REPOSITORY_ROUTE.fullmatch(path)
            if self.command in {"GET", "HEAD"} and api_match:
                self._json(200, {"repo": self.server.store.get(api_match.group(1))}, head=head)
                return
            git_match = GIT_ROUTE.fullmatch(unquote(parsed.path))
            if self.command in {"GET", "HEAD", "POST"} and git_match:
                self.server.store.path(git_match.group(1))
                self._git_backend(parsed.path.removeprefix("/git"), parsed.query, head)
                return
            raise GitsError(404, "not_found", "route not found")
        except GitsError as error:
            self._error(error, head=head)
        except subprocess.TimeoutExpired:
            self._error(GitsError(504, "git_timeout", "git operation timed out"), head=head)
        except Exception as error:  # noqa: BLE001
            print(f"gits request failed: {type(error).__name__}: {error}", file=sys.stderr)
            self._error(GitsError(500, "internal_error", "internal server error"), head=head)

    def _git_backend(self, path_info: str, query: str, head: bool) -> None:
        body = b""
        if self.command == "POST":
            body = self._request_body(self.server.max_git_body)
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_PROJECT_ROOT": str(self.server.store.root),
                "GIT_HTTP_EXPORT_ALL": "1",
                "PATH_INFO": path_info,
                "QUERY_STRING": query,
                "REQUEST_METHOD": self.command,
                "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                "CONTENT_LENGTH": str(len(body)),
                "REMOTE_ADDR": self.client_address[0],
                "SERVER_PROTOCOL": self.protocol_version,
            }
        )
        git_protocol = self.headers.get("Git-Protocol")
        if git_protocol:
            environment["HTTP_GIT_PROTOCOL"] = git_protocol
        completed = subprocess.run(
            [self.server.store.git_bin, "http-backend"],
            input=body,
            capture_output=True,
            env=environment,
            timeout=GIT_TIMEOUT_SECONDS,
            check=False,
        )
        header_bytes, separator, response_body = completed.stdout.partition(b"\r\n\r\n")
        if not separator:
            header_bytes, separator, response_body = completed.stdout.partition(b"\n\n")
        if not separator:
            raise RuntimeError(completed.stderr.decode(errors="replace").strip() or "invalid git response")
        status = 200
        headers: list[tuple[str, str]] = []
        for raw_line in header_bytes.replace(b"\r\n", b"\n").splitlines():
            name_bytes, colon, value_bytes = raw_line.partition(b":")
            if not colon:
                continue
            name = name_bytes.decode("ascii", errors="ignore").strip()
            value = value_bytes.decode("latin-1").strip()
            if name.lower() == "status":
                status = int(value.split(" ", 1)[0])
            elif name.lower() not in {"content-length", "connection", "transfer-encoding"}:
                headers.append((name, value))
        self.send_response(status)
        self.send_header("X-Content-Type-Options", "nosniff")
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        if not head:
            self.wfile.write(response_body)

    def _json_body(self) -> dict[str, Any]:
        if self.headers.get_content_type() != "application/json":
            raise GitsError(415, "unsupported_media_type", "request must use application/json")
        body = self._request_body(MAX_JSON_BYTES)
        try:
            value = json.loads(body)
        except (UnicodeDecodeError, ValueError) as error:
            raise GitsError(400, "invalid_json", "request body must be a JSON object") from error
        if not isinstance(value, dict):
            raise GitsError(400, "invalid_json", "request body must be a JSON object")
        return value

    def _request_body(self, limit: int) -> bytes:
        raw_length = self.headers.get("Content-Length")
        if raw_length is None:
            raise GitsError(411, "content_length_required", "Content-Length is required")
        try:
            length = int(raw_length)
        except ValueError as error:
            raise GitsError(400, "invalid_content_length", "Content-Length is invalid") from error
        if length < 0 or length > limit:
            raise GitsError(413, "request_too_large", "request body exceeds the configured limit")
        body = self.rfile.read(length)
        if len(body) != length:
            raise GitsError(400, "short_request", "request body ended before Content-Length")
        return body

    def _file(self, path: Path, content_type: str, head: bool) -> None:
        try:
            body = path.read_bytes()
        except FileNotFoundError as error:
            raise GitsError(404, "not_found", "file not found") from error
        self._bytes(200, body, content_type, head=head, html=content_type.startswith("text/html"))

    def _json(self, status: int, value: dict[str, Any], head: bool = False) -> None:
        body = json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode()
        self._bytes(status, body, "application/json; charset=utf-8", head=head)

    def _error(self, error: GitsError, head: bool = False) -> None:
        self._json(error.status, {"error": {"code": error.code, "message": error.message}}, head=head)

    def _bytes(self, status: int, body: bytes, content_type: str, head: bool, html: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        if html:
            self.send_header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            )
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print(f"gits {self.address_string()} {format % args}", file=sys.stderr)


def parse_listen(value: str) -> tuple[str, int]:
    try:
        host, raw_port = value.rsplit(":", 1)
        port = int(raw_port)
    except ValueError as error:
        raise ValueError("GITS_LISTEN must use HOST:PORT") from error
    if not host or port < 1 or port > 65535:
        raise ValueError("GITS_LISTEN must contain a valid host and port")
    return host, port


def validate_public_url(value: str) -> str:
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password:
        raise ValueError("GITS_PUBLIC_URL must be an HTTP(S) origin without credentials")
    if parsed.path not in {"", "/"} or parsed.query or parsed.fragment:
        raise ValueError("GITS_PUBLIC_URL must contain only an origin")
    return value.rstrip("/")


def main() -> int:
    listen = parse_listen(os.environ.get("GITS_LISTEN", "127.0.0.1:9430"))
    data_dir = Path(os.environ.get("GITS_DATA_DIR", ".treer/apps/gits")).expanduser().resolve()
    public_url = validate_public_url(os.environ.get("GITS_PUBLIC_URL", "http://gits.internal"))
    git_bin = os.environ.get("GITS_GIT_BIN", "git")
    max_git_body = int(os.environ.get("GITS_MAX_PUSH_BYTES", str(256 * 1024 * 1024)))
    if max_git_body < 1:
        raise ValueError("GITS_MAX_PUSH_BYTES must be positive")
    subprocess.run([git_bin, "--version"], capture_output=True, timeout=10, check=True)
    server = GitsServer(listen, RepositoryStore(data_dir, public_url, git_bin), max_git_body)
    print(f"gits listening on http://{listen[0]}:{listen[1]} with data in {data_dir}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
