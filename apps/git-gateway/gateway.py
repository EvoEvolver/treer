#!/usr/bin/env python3
"""Treer Git Gateway: one GitHub credential, used on behalf of every workspace Agent.

Agents reach this App through their workload proxy (``http://git-gateway.treer/``);
the Proxy injects a verified identity, the gateway checks its rules, injects
its own GitHub credential and streams Git Smart HTTP and REST traffic to
GitHub. Standard library only.
"""

from __future__ import annotations

import argparse
import base64
import http.client
import json
import os
import re
import secrets
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Iterator
from urllib.error import HTTPError
from urllib.parse import parse_qs, quote, urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

APP_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(APP_ROOT))

from gw_credentials import (  # noqa: E402
    CredentialError,
    Credentials,
    GhCredential,
    GitHubAppCredential,
    github_json,
)
from gw_rules import (  # noqa: E402
    OWNER,
    REPO,
    Caller,
    PushHead,
    RuleError,
    api_decision,
    parse_report_status,
    push_decisions,
    receive_pack_rejection,
    repo_allowed,
)
from gw_state import State  # noqa: E402

VERSION = "1"
GIT_ROUTE = re.compile(r"/github/([^/]+)/([^/]+?)(?:\.git)?/(info/refs|git-upload-pack|git-receive-pack)")
MAX_JSON_BYTES = 256 * 1024
MAX_API_BODY_BYTES = 25 * 1024 * 1024
MAX_UPLOAD_PACK_REQUEST_BYTES = 256 * 1024 * 1024
UPSTREAM_TIMEOUT = 600
CHUNK = 64 * 1024
GIT_REQUEST_HEADERS = ("content-type", "accept", "accept-encoding", "content-encoding", "git-protocol", "user-agent")
GIT_RESPONSE_HEADERS = ("content-type", "content-encoding", "cache-control", "expires", "pragma")
API_REQUEST_HEADERS = (
    "accept",
    "content-type",
    "if-none-match",
    "if-modified-since",
    "x-github-api-version",
    "user-agent",
)
API_RESPONSE_HEADERS = (
    "content-type",
    "content-encoding",
    "cache-control",
    "etag",
    "last-modified",
    "link",
    "location",
    "retry-after",
    "x-github-request-id",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
    "x-ratelimit-used",
    "x-github-api-version-selected",
)


class GatewayError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status, self.code, self.message = status, code, message


class TooLarge(Exception):
    pass


def root_representation(accept: str, user_agent: str) -> str:
    choices: list[tuple[float, int, str]] = []
    for order, raw_entry in enumerate(accept.split(",")):
        parts = [part.strip().lower() for part in raw_entry.split(";")]
        if parts[0] not in {"text/html", "text/markdown"}:
            continue
        quality = 1.0
        for parameter in parts[1:]:
            if parameter.startswith("q="):
                try:
                    quality = float(parameter[2:])
                except ValueError:
                    quality = 0.0
        if 0 < quality <= 1:
            choices.append((-quality, order, parts[0]))
    if choices:
        return min(choices)[2]
    return "text/html" if "mozilla/" in user_agent.lower() else "text/markdown"


def origin_of(url: str) -> str:
    parts = urlsplit(url)
    return f"{parts.scheme}://{parts.netloc}"


# --------------------------------------------------------------------------- identity


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args: Any, **_kwargs: Any) -> None:
        return None


@dataclass
class Settings:
    listen: tuple[str, int]
    data_dir: Path
    proxy_url: str | None
    service_id: str | None
    workspace_id: str | None
    public_url: str | None
    agent_url: str
    github_url: str
    github_api_url: str
    gh_bin: str
    gh_refresh_seconds: float
    treer_bin: str
    seed: dict[str, Any] = field(default_factory=dict)


class Identity:
    """Verifies ``X-Treer-Identity-Token`` against this App's own service audience via the Proxy."""

    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.lock = threading.Lock()
        self.names: dict[str, tuple[str, str | None, float]] = {}
        self.opener = build_opener(ProxyHandler({}), NoRedirect())

    def audience(self) -> tuple[str, str, str]:
        with self.lock:
            settings = self.settings
            if not settings.proxy_url:
                raise GatewayError(503, "identity_not_configured", "GATEWAY_PROXY_URL (or TREER_PROXY_URL) is required")
            if not settings.service_id or not settings.workspace_id or not settings.public_url:
                self._discover()
            if not settings.service_id or not settings.workspace_id:
                raise GatewayError(
                    503, "identity_not_configured", "set GATEWAY_SERVICE_ID and GATEWAY_WORKSPACE_ID"
                )
            return settings.proxy_url, settings.service_id, settings.workspace_id

    def _discover(self) -> None:
        """Find this Managed App's own service (never a caller-supplied audience)."""
        settings = self.settings
        agent_id = os.environ.get("TREER_AGENT_ID")
        if not agent_id:
            return
        try:
            completed = subprocess.run(
                [settings.treer_bin, "app", "list"], capture_output=True, text=True, timeout=15, check=True
            )
            app = next(item for item in json.loads(completed.stdout)["apps"] if item.get("runtime_agent_id") == agent_id)
        except (OSError, ValueError, KeyError, StopIteration, subprocess.SubprocessError):
            return
        settings.service_id = settings.service_id or app.get("service_id")
        settings.workspace_id = settings.workspace_id or app.get("workspace_id")
        if not settings.public_url and settings.proxy_url:
            settings.public_url = app.get("public_url") or (
                settings.proxy_url.rstrip("/") + f"/api/workspaces/{app.get('workspace_id')}/apps/{app.get('app_id')}/ui/"
            )

    def _post(self, url: str, body: dict[str, Any]) -> dict[str, Any]:
        request = Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"}, method="POST")
        with self.opener.open(request, timeout=10) as response:
            raw = response.read(65537)
        if len(raw) > 65536:
            raise ValueError("verify response too large")
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise ValueError("verify response is not an object")
        return value

    def verify(self, headers: Any) -> Caller:
        if "X-Treer-Identity-Token" in headers:
            token = headers.get("X-Treer-Identity-Token") or ""
        else:
            authorization = headers.get("Authorization") or ""
            token = authorization[7:] if authorization.startswith("Bearer ") else ""
        if not token or len(token) > 8192:
            raise GatewayError(
                401, "authentication_required", "call the gateway through the Treer workload proxy (http_proxy)"
            )
        proxy_url, service_id, workspace_id = self.audience()
        try:
            result = self._post(proxy_url.rstrip("/") + "/.treer/apps/identity/verify", {"token": token, "audience": service_id})
        except HTTPError as error:
            raise GatewayError(401, "identity_invalid", f"identity verification rejected (HTTP {error.code})") from error
        except (OSError, ValueError) as error:
            raise GatewayError(503, "identity_unavailable", "Treer identity verification is unavailable") from error
        claims = result.get("claims") if isinstance(result.get("claims"), dict) else {}
        if (
            result.get("active") is not True
            or claims.get("service_id") != service_id
            or claims.get("workspace_id") != workspace_id
            or claims.get("principal_kind") not in ("human", "agent")
            or not isinstance(claims.get("sub"), str)
            or not claims.get("sub")
            or (isinstance(claims.get("exp"), (int, float)) and claims["exp"] < time.time())
        ):
            raise GatewayError(401, "identity_invalid", "identity is inactive or not for this App")
        name, path = self._display(claims, token, proxy_url, service_id)
        return Caller(
            kind=claims["principal_kind"],
            id=claims["sub"],
            name=name,
            machine_id=claims.get("machine_id") if isinstance(claims.get("machine_id"), str) else None,
            role=claims.get("role") if isinstance(claims.get("role"), str) else None,
            path=path,
        )

    def _display(self, claims: dict[str, Any], token: str, proxy_url: str, service_id: str) -> tuple[str, str | None]:
        """Best-effort display name/path from Core's directory (cached 10 minutes); never used to authorize."""
        subject = claims["sub"]
        fallback = claims.get("name") if isinstance(claims.get("name"), str) and claims.get("name") else subject
        with self.lock:
            cached = self.names.get(subject)
            if cached and cached[2] > time.time():
                return cached[0], cached[1]
        name, path = fallback, None
        try:
            request = Request(
                proxy_url.rstrip("/") + f"/api/apps/{quote(service_id)}/directory",
                headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
            )
            with self.opener.open(request, timeout=3) as response:
                principals = json.loads(response.read(2 * 1024 * 1024)).get("principals") or []
            for principal in principals:
                if isinstance(principal, dict) and principal.get("id") == subject:
                    name = principal.get("name") or fallback
                    path = principal.get("path") if isinstance(principal.get("path"), str) else None
                    break
        except (OSError, ValueError, AttributeError):
            pass
        with self.lock:
            self.names[subject] = (name, path, time.time() + 600)
        return name, path


# --------------------------------------------------------------------------- application


class Gateway:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.state = State(settings.data_dir, settings.seed)
        self.identity = Identity(settings)
        self.gh = GhCredential(settings.gh_bin, urlsplit(settings.github_url).hostname or "github.com", settings.gh_refresh_seconds)
        self.app = GitHubAppCredential(settings.data_dir, settings.github_api_url)
        self.credentials = Credentials(self.gh, self.app, lambda: self.state.config()["credential_mode"])
        self.rate_lock = threading.Lock()
        self.rate_limit: dict[str, Any] | None = None
        self.rate_limit_at = -1e18

    def record_rate_limit(self, headers: dict[str, str]) -> None:
        if "x-ratelimit-remaining" not in headers:
            return
        with self.rate_lock:
            self.rate_limit = {
                "resource": headers.get("x-ratelimit-resource"),
                "limit": _int(headers.get("x-ratelimit-limit")),
                "remaining": _int(headers.get("x-ratelimit-remaining")),
                "reset": _int(headers.get("x-ratelimit-reset")),
                "observed_at": time.time(),
            }

    def status(self) -> dict[str, Any]:
        config = self.state.config()
        active, error = None, None
        try:
            _, active = self.credentials.get(config["repos"][0].split("/")[0] if config["repos"] else None)
        except (CredentialError, OSError) as failure:
            error = str(failure)
        rate = self._rate_limit()
        return {
            "service": "git-gateway",
            "version": VERSION,
            "credential": {
                "mode": config["credential_mode"],
                "active": active,
                "error": error,
                "gh": self.gh.describe(),
                "github_app": self.app.describe(),
            },
            "rate_limit": rate,
            "identity": {
                "proxy_url": self.settings.proxy_url,
                "service_id": self.settings.service_id,
                "workspace_id": self.settings.workspace_id,
            },
            "agent_url": self.settings.agent_url,
            "public_url": self.settings.public_url,
            "github_url": self.settings.github_url,
            "repos": config["repos"],
        }

    def _rate_limit(self) -> dict[str, Any] | None:
        with self.rate_lock:
            if time.time() - self.rate_limit_at < 60:
                return self.rate_limit
            self.rate_limit_at = time.time()
        try:
            token, _ = self.credentials.get(None)
            status, headers, value = github_json(
                "GET", f"{self.settings.github_api_url}/rate_limit", {"Authorization": f"Bearer {token}"}, timeout=5
            )
            if status == 200 and isinstance(value, dict):
                core = (value.get("resources") or {}).get("core") or {}
                with self.rate_lock:
                    self.rate_limit = {
                        "resource": "core",
                        "limit": core.get("limit"),
                        "remaining": core.get("remaining"),
                        "reset": core.get("reset"),
                        "observed_at": time.time(),
                    }
        except (CredentialError, OSError, ValueError):
            pass
        with self.rate_lock:
            return self.rate_limit

    def manifest(self, org: str | None, name: str | None) -> dict[str, Any]:
        self.identity.audience()
        public = self.settings.public_url
        if not public:
            raise GatewayError(503, "public_url_unknown", "set GATEWAY_PUBLIC_URL to the App's browser URL")
        public = public.rstrip("/") + "/"
        if org is not None and not OWNER.fullmatch(org):
            raise GatewayError(400, "invalid_org", "org must be a GitHub organization login")
        if name is not None and (not isinstance(name, str) or not 3 <= len(name) <= 34):
            raise GatewayError(400, "invalid_name", "name must be 3-34 characters")
        manifest = {
            "name": name or f"Treer Git Gateway {secrets.token_hex(3)}",
            "url": public,
            "redirect_url": public + "api/github-app/callback",
            "setup_url": public + "?github_app=installed",
            "setup_on_update": True,
            "public": False,
            "hook_attributes": {"url": public + "api/github-app/webhook", "active": False},
            "default_permissions": {
                "contents": "write",
                "metadata": "read",
                "pull_requests": "write",
                "issues": "write",
                "actions": "write",
                "checks": "read",
                "statuses": "read",
                "workflows": "write",
            },
            "default_events": [],
        }
        base = self.settings.github_url.rstrip("/")
        target = f"{base}/organizations/{org}/settings/apps/new" if org else f"{base}/settings/apps/new"
        return {"manifest": manifest, "post_url": target}


def _int(value: str | None) -> int | None:
    try:
        return int(value) if value is not None else None
    except ValueError:
        return None


# --------------------------------------------------------------------------- request bodies


class Body:
    """Streams a request body framed by Content-Length or chunked encoding, with a byte cap."""

    def __init__(self, rfile: Any, headers: Any, limit: int) -> None:
        self.rfile = rfile
        self.limit = limit
        self.total = 0
        self.finished = False
        self.chunked = "chunked" in (headers.get("Transfer-Encoding") or "").lower()
        raw_length = headers.get("Content-Length")
        self.length: int | None = None
        if not self.chunked:
            try:
                self.length = int(raw_length) if raw_length is not None else 0
            except ValueError as error:
                raise GatewayError(400, "invalid_content_length", "Content-Length is invalid") from error
            if self.length < 0:
                raise GatewayError(400, "invalid_content_length", "Content-Length is invalid")
            if self.length > limit:
                raise GatewayError(413, "request_too_large", "request body exceeds the configured limit")
        self.remaining = self.length or 0
        self.chunk_left = 0

    def _account(self, data: bytes) -> bytes:
        self.total += len(data)
        if self.total > self.limit:
            raise TooLarge()
        return data

    def __iter__(self) -> Iterator[bytes]:
        if not self.chunked:
            while self.remaining > 0:
                data = self.rfile.read(min(CHUNK, self.remaining))
                if not data:
                    raise GatewayError(400, "short_request", "request body ended early")
                self.remaining -= len(data)
                yield self._account(data)
            self.finished = True
            return
        while True:
            if self.chunk_left == 0:
                line = self.rfile.readline(1024)
                if not line:
                    raise GatewayError(400, "short_request", "chunked request ended early")
                try:
                    self.chunk_left = int(line.split(b";", 1)[0].strip(), 16)
                except ValueError as error:
                    raise GatewayError(400, "invalid_chunk", "invalid chunked encoding") from error
                if self.chunk_left == 0:
                    while True:
                        trailer = self.rfile.readline(8192)
                        if trailer in (b"\r\n", b"\n", b""):
                            break
                    self.finished = True
                    return
            data = self.rfile.read(min(CHUNK, self.chunk_left))
            if not data:
                raise GatewayError(400, "short_request", "chunked request ended early")
            self.chunk_left -= len(data)
            if self.chunk_left == 0:
                self.rfile.readline(16)
            yield self._account(data)

    def read_all(self) -> bytes:
        return b"".join(self)


# --------------------------------------------------------------------------- HTTP handler


class GatewayServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], gateway: Gateway, quiet: bool = False) -> None:
        self.gateway = gateway
        self.quiet = quiet
        super().__init__(address, Handler)

    def handle_error(self, request: Any, client_address: Any) -> None:
        if isinstance(sys.exc_info()[1], (ConnectionResetError, BrokenPipeError, TimeoutError)):
            return  # a client hung up; nothing to report
        super().handle_error(request, client_address)


class Handler(BaseHTTPRequestHandler):
    server: GatewayServer
    protocol_version = "HTTP/1.1"

    # entry points ----------------------------------------------------------------
    def do_GET(self) -> None:  # noqa: N802
        self._dispatch()

    def do_HEAD(self) -> None:  # noqa: N802
        self._dispatch()

    def do_POST(self) -> None:  # noqa: N802
        self._dispatch()

    def do_PUT(self) -> None:  # noqa: N802
        self._dispatch()

    def do_PATCH(self) -> None:  # noqa: N802
        self._dispatch()

    def do_DELETE(self) -> None:  # noqa: N802
        self._dispatch()

    @property
    def gw(self) -> Gateway:
        return self.server.gateway

    def _dispatch(self) -> None:
        self.started = False
        self.git_route = False
        try:
            parsed = urlsplit(self.path)
            path = parsed.path
            if path.startswith("/github/"):
                self.git_route = True
                self._git(parsed)
            elif path.startswith("/github-api/"):
                self._api(parsed)
            else:
                self._app(parsed)
        except GatewayError as error:
            self._error(error)
        except (BrokenPipeError, ConnectionResetError):
            self.close_connection = True
        except Exception as error:  # noqa: BLE001
            print(f"git-gateway request failed: {type(error).__name__}: {error}", file=sys.stderr)
            self._error(GatewayError(500, "internal_error", "internal server error"))

    # App routes ------------------------------------------------------------------
    def _app(self, parsed: Any) -> None:
        path = parsed.path
        method = self.command
        if method in ("GET", "HEAD"):
            if path == "/":
                representation = root_representation(self.headers.get("Accept", ""), self.headers.get("User-Agent", ""))
                if representation == "text/html":
                    self._file(APP_ROOT / "web" / "index.html", "text/html; charset=utf-8", vary=True)
                else:
                    text = (APP_ROOT / "AGENT.md").read_text(encoding="utf-8")
                    text = text.replace("http://git-gateway.treer", self.gw.settings.agent_url.rstrip("/"))
                    self._bytes(200, text.encode(), "text/markdown; charset=utf-8", vary=True)
                return
            if path in ("/app.js", "/app.css"):
                kind = "text/javascript" if path.endswith(".js") else "text/css"
                self._file(APP_ROOT / "web" / path[1:], f"{kind}; charset=utf-8")
                return
            if path == "/health":
                self._json(200, {"status": "ok", "service": "git-gateway"})
                return
        if not path.startswith("/api/"):
            raise GatewayError(404, "not_found", "route not found")
        caller = self.gw.identity.verify(self.headers)
        query = parse_qs(parsed.query, max_num_fields=20)
        if method == "GET" and path == "/api/status":
            self._json(200, {**self.gw.status(), "caller": caller.audit()})
        elif method == "GET" and path == "/api/config":
            self._json(200, {"config": self.gw.state.config()})
        elif method == "PUT" and path == "/api/config":
            self._admin(caller)
            change = self._json_body()
            try:
                config = self.gw.state.update_config(change)
            except RuleError as error:
                raise GatewayError(400, error.code, error.message) from error
            self.gw.state.audit({"op": "config", "caller": caller.audit(), "result": "ok", "fields": sorted(change)})
            self._json(200, {"config": config})
        elif method == "GET" and path == "/api/audit":
            try:
                limit = max(1, min(1000, int(query.get("limit", ["100"])[0])))
            except ValueError as error:
                raise GatewayError(400, "invalid_limit", "limit must be an integer") from error
            filters = {key: query.get(key, [""])[0] for key in ("repo", "op", "result", "caller", "kind")}
            self._json(200, {"entries": self.gw.state.read_audit(limit, filters)})
        elif method == "POST" and path == "/api/credentials/refresh":
            self._admin(caller)
            self.gw.gh.invalidate()
            self.gw.app.invalidate(None)
            if self.gw.app.configured:
                try:
                    self.gw.app.list_installations(force=True)
                except (CredentialError, OSError):
                    pass
            self.gw.rate_limit_at = -1e18
            self._json(200, self.gw.status())
        elif method == "POST" and path == "/api/github-app/manifest":
            self._admin(caller)
            if caller.kind != "human":
                raise GatewayError(403, "human_required", "creating a GitHub App needs the owner's browser")
            body = self._json_body()
            unknown = set(body) - {"org", "name"}
            if unknown:
                raise GatewayError(400, "unknown_field", f"unknown field: {sorted(unknown)[0]}")
            result = self.gw.manifest(body.get("org") or None, body.get("name") or None)
            state = self.gw.state.new_manifest_state(caller.id)
            result["post_url"] += f"?state={state}"
            result["manifest"] = json.dumps(result["manifest"])
            self._json(200, result)
        elif method == "GET" and path == "/api/github-app/callback":
            self._github_app_callback(caller, query)
        elif method == "DELETE" and path == "/api/github-app":
            self._admin(caller)
            self.gw.app.forget()
            self.gw.state.audit({"op": "github_app", "action": "forget", "caller": caller.audit(), "result": "ok"})
            self._json(200, {"github_app": self.gw.app.describe()})
        else:
            raise GatewayError(404, "not_found", "route not found")

    def _admin(self, caller: Caller) -> None:
        if not caller.privileged(self.gw.state.config()):
            raise GatewayError(403, "admin_required", "only humans and trusted_agents may change the gateway")
        if caller.kind == "human":
            site = self.headers.get("Sec-Fetch-Site")
            if site is not None and site not in ("same-origin", "same-site", "none"):
                raise GatewayError(403, "csrf_rejected", "cross-site browser mutation rejected")
            origin = self.headers.get("Origin")
            allowed = {origin_of(url) for url in (self.gw.settings.public_url, self.gw.settings.proxy_url) if url}
            if origin is not None and origin not in allowed:
                raise GatewayError(403, "csrf_rejected", "browser origin is not this App or its Treer tunnel")
            if self.headers.get("X-Gateway-Request") != "1":
                raise GatewayError(403, "csrf_rejected", "human mutations require X-Gateway-Request: 1")

    def _github_app_callback(self, caller: Caller, query: dict[str, list[str]]) -> None:
        code = query.get("code", [""])[0]
        state = query.get("state", [""])[0]
        if caller.kind != "human" or not state or not self.gw.state.take_manifest_state(state, caller.id):
            raise GatewayError(403, "manifest_state_invalid", "start Create GitHub App again from the gateway UI")
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,200}", code):
            raise GatewayError(400, "invalid_code", "GitHub did not return a manifest code")
        status, _, value = github_json(
            "POST", f"{self.gw.settings.github_api_url}/app-manifests/{code}/conversions"
        )
        if status != 201 or not isinstance(value, dict):
            raise GatewayError(502, "manifest_conversion_failed", f"GitHub manifest conversion failed (HTTP {status})")
        try:
            metadata = self.gw.app.store(value)
        except (CredentialError, ValueError) as error:
            raise GatewayError(502, "manifest_conversion_failed", "GitHub returned an unusable App key") from error
        self.gw.state.audit(
            {"op": "github_app", "action": "create", "caller": caller.audit(), "result": "ok", "app_id": metadata["id"]}
        )
        self.send_response(303)
        self.send_header("Location", "../../?github_app=created")
        self.send_header("Content-Length", "0")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

    # Git Smart HTTP --------------------------------------------------------------
    def _git(self, parsed: Any) -> None:
        match = GIT_ROUTE.fullmatch(parsed.path)
        if not match or not OWNER.fullmatch(match.group(1)) or not REPO.fullmatch(match.group(2)):
            raise GatewayError(404, "not_found", "use /github/OWNER/REPO.git with Git Smart HTTP")
        owner, repo, endpoint = match.groups()
        query = parse_qs(parsed.query)
        if endpoint == "info/refs":
            service = query.get("service", [""])[0]
            if self.command != "GET" or service not in ("git-upload-pack", "git-receive-pack"):
                raise GatewayError(403, "smart_http_required", "only Git Smart HTTP is supported")
            op = "advertise-push" if service == "git-receive-pack" else "advertise-fetch"
        else:
            if self.command != "POST":
                raise GatewayError(405, "method_not_allowed", "use POST")
            op = "push" if endpoint == "git-receive-pack" else "fetch"
        caller = self.gw.identity.verify(self.headers)
        config = self.gw.state.config()
        audit: dict[str, Any] = {"op": op, "caller": caller.audit(), "repo": f"{owner}/{repo}"}
        if not repo_allowed(config, owner, repo):
            self.gw.state.audit({**audit, "result": "denied", "reason": "repo_not_allowed"})
            raise GatewayError(403, "repo_not_allowed", f"{owner}/{repo} is not in the gateway repo allowlist")
        upstream_path = f"/{owner}/{repo}.git/{endpoint}" + (f"?{parsed.query}" if parsed.query else "")
        headers = {name: self.headers[name] for name in GIT_REQUEST_HEADERS if self.headers.get(name)}
        headers.setdefault("User-Agent", "git/treer-git-gateway")
        body_iter: Iterator[bytes] | None = None
        body: Body | None = None
        capabilities: set[str] = set()
        if self.command == "POST":
            limit = config["max_push_bytes"] if op == "push" else MAX_UPLOAD_PACK_REQUEST_BYTES
            body = Body(self.rfile, self.headers, limit)
            body_iter = iter(body)
            if op == "push":
                body_iter, updates, capabilities = self._push_head(body, body_iter, audit, config, caller)
                if body_iter is None:
                    return
                audit["refs"] = [update.audit() for update in updates]
                if not updates:
                    audit["probe"] = True
        self._forward_git(owner, upstream_path, headers, body, body_iter, audit, capabilities)

    def _push_head(self, body: Body, chunks: Iterator[bytes], audit: dict[str, Any], config: dict[str, Any], caller: Caller) -> tuple[Any, Any, set[str]]:
        head = PushHead("gzip" in (self.headers.get("Content-Encoding") or "").lower())
        buffered: list[bytes] = []
        parsed = None
        try:
            for chunk in chunks:
                buffered.append(chunk)
                parsed = head.feed(chunk)
                if parsed is not None:
                    break
        except RuleError as error:
            self.close_connection = True
            self.gw.state.audit({**audit, "result": "denied", "reason": error.code})
            raise GatewayError(400, error.code, error.message) from error
        except TooLarge:
            self.close_connection = True
            raise GatewayError(413, "push_too_large", "push exceeds max_push_bytes") from None
        if parsed is None:
            self.gw.state.audit({**audit, "result": "denied", "reason": "invalid_push_command"})
            raise GatewayError(400, "invalid_push_command", "receive-pack request has no command list")
        updates, capabilities = parsed
        reasons = push_decisions(config, caller, updates)
        if any(reasons):
            try:
                for _ in chunks:
                    pass
            except TooLarge:
                self.close_connection = True
            self.gw.state.audit(
                {
                    **audit,
                    "refs": [{**update.audit(), "result": f"ng {reason}" if reason else "not attempted"} for update, reason in zip(updates, reasons)],
                    "result": "denied",
                    "reason": next(reason for reason in reasons if reason),
                }
            )
            if "report-status" in capabilities or "report-status-v2" in capabilities:
                payload = receive_pack_rejection(updates, reasons, capabilities)
                self._bytes(200, payload, "application/x-git-receive-pack-result")
            else:
                raise GatewayError(403, "push_denied", next(reason for reason in reasons if reason) or "push denied")
            return None, updates, capabilities

        def joined() -> Iterator[bytes]:
            yield from buffered
            yield from chunks

        return joined(), updates, capabilities

    def _forward_git(
        self,
        owner: str,
        upstream_path: str,
        headers: dict[str, str],
        body: Body | None,
        body_iter: Iterator[bytes] | None,
        audit: dict[str, Any],
        capabilities: set[str],
    ) -> None:
        settings = self.gw.settings
        for attempt in (1, 2):
            try:
                token, source = self.gw.credentials.get(owner)
            except (CredentialError, OSError) as error:
                self.gw.state.audit({**audit, "result": "error", "reason": "no_credential"})
                raise GatewayError(503, "no_credential", str(error)) from error
            basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
            request_headers = {**headers, "Authorization": f"Basic {basic}"}
            length = body.length if body is not None and not body.chunked else None
            try:
                connection, response = upstream(settings.github_url, self.command, upstream_path, request_headers, body_iter, length)
            except TooLarge:
                self.close_connection = True
                self.gw.state.audit({**audit, "result": "denied", "reason": "push_too_large"})
                raise GatewayError(413, "push_too_large", "push exceeds max_push_bytes") from None
            except OSError as error:
                self.close_connection = body is not None and not body.finished
                self.gw.state.audit({**audit, "result": "error", "reason": "upstream_unavailable"})
                raise GatewayError(502, "upstream_unavailable", f"GitHub is unreachable: {type(error).__name__}") from error
            audit["credential"] = source
            if response.status == 401:
                connection.close()
                self.gw.credentials.invalidate(source, owner)
                if attempt == 1 and body_iter is None:
                    continue
                self.gw.state.audit({**audit, "result": "error", "status": 401, "reason": "credential_rejected"})
                raise GatewayError(502, "credential_rejected", "GitHub rejected the gateway credential; retry")
            break
        tee = bytearray() if audit["op"] == "push" else None
        try:
            sent = self._relay(response, GIT_RESPONSE_HEADERS, tee=tee)
        finally:
            connection.close()
        result = "ok" if 200 <= response.status < 300 else "error"
        entry: dict[str, Any] = {**audit, "status": response.status, "bytes_in": body.total if body else 0, "bytes_out": sent}
        if tee is not None:
            report = parse_report_status(bytes(tee), capabilities)
            for ref in entry.get("refs", []):
                ref["result"] = report.get(ref["ref"], "unknown")
            if any(ref["result"].startswith("ng") for ref in entry.get("refs", [])):
                result = "rejected"
        self.gw.state.audit({**entry, "result": result})

    # REST passthrough ------------------------------------------------------------
    def _api(self, parsed: Any) -> None:
        path = parsed.path[len("/github-api") :] or "/"
        caller = self.gw.identity.verify(self.headers)
        config = self.gw.state.config()
        method = self.command
        reason, owner, repo = api_decision(config, caller, method, path)
        audit: dict[str, Any] = {
            "op": "api",
            "caller": caller.audit(),
            "method": method,
            "path": path,
            "repo": f"{owner}/{repo}" if owner else None,
        }
        if reason:
            self.close_connection = True
            self.gw.state.audit({**audit, "result": "denied", "reason": reason})
            raise GatewayError(403, reason, f"gateway rule denied {method} {path}")
        body = Body(self.rfile, self.headers, MAX_API_BODY_BYTES)
        try:
            raw = body.read_all()
        except TooLarge:
            self.close_connection = True
            raise GatewayError(413, "request_too_large", "API request body is too large") from None
        headers = {name: self.headers[name] for name in API_REQUEST_HEADERS if self.headers.get(name)}
        headers.setdefault("User-Agent", "treer-git-gateway")
        headers.setdefault("Accept", "application/vnd.github+json")
        target = path + (f"?{parsed.query}" if parsed.query else "")
        for attempt in (1, 2):
            try:
                token, source = self.gw.credentials.get(owner)
            except (CredentialError, OSError) as error:
                self.gw.state.audit({**audit, "result": "error", "reason": "no_credential"})
                raise GatewayError(503, "no_credential", str(error)) from error
            try:
                connection, response = upstream(
                    self.gw.settings.github_api_url,
                    method,
                    target,
                    {**headers, "Authorization": f"Bearer {token}"},
                    iter([raw]) if raw or method in ("POST", "PUT", "PATCH") else None,
                    len(raw),
                )
            except OSError as error:
                self.gw.state.audit({**audit, "result": "error", "reason": "upstream_unavailable"})
                raise GatewayError(502, "upstream_unavailable", f"GitHub API is unreachable: {type(error).__name__}") from error
            if response.status == 401 and attempt == 1:
                connection.close()
                self.gw.credentials.invalidate(source, owner)
                continue
            break
        response_headers = {name.lower(): value for name, value in response.getheaders()}
        self.gw.record_rate_limit(response_headers)
        try:
            self._relay(response, API_RESPONSE_HEADERS, rewrite=True)
        finally:
            connection.close()
        self.gw.state.audit(
            {**audit, "credential": source, "status": response.status, "result": "ok" if response.status < 400 else "error"}
        )

    # plumbing --------------------------------------------------------------------
    def _relay(self, response: http.client.HTTPResponse, allowed: tuple[str, ...], tee: bytearray | None = None, rewrite: bool = False) -> int:
        api_base = self.gw.settings.github_api_url.rstrip("/") + "/"
        public_api = self.gw.settings.agent_url.rstrip("/") + "/github-api/"
        status = response.status
        if status == 401:
            status = 502  # never let a client prompt for credentials the gateway owns
        self.started = True
        self.send_response(status)
        for name, value in response.getheaders():
            lowered = name.lower()
            if lowered in allowed:
                if rewrite and lowered in ("link", "location"):
                    value = value.replace(api_base, public_api).replace("https://api.github.com/", public_api)
                self.send_header(name, value)
        self.send_header("X-Content-Type-Options", "nosniff")
        length = response.getheader("Content-Length")
        no_body = self.command == "HEAD" or status in (204, 304) or 100 <= status < 200
        chunked = not no_body and (response.chunked or length is None)
        if no_body:
            self.send_header("Content-Length", "0" if self.command != "HEAD" else (length or "0"))
        elif chunked:
            self.send_header("Transfer-Encoding", "chunked")
        else:
            self.send_header("Content-Length", length or "0")
        self.end_headers()
        sent = 0
        if no_body:
            return 0
        while True:
            data = response.read1(CHUNK) if hasattr(response, "read1") else response.read(CHUNK)
            if not data:
                break
            sent += len(data)
            if tee is not None and len(tee) < 1024 * 1024:
                tee.extend(data[: 1024 * 1024 - len(tee)])
            if chunked:
                self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
            else:
                self.wfile.write(data)
        if chunked:
            self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()
        return sent

    def _json_body(self) -> dict[str, Any]:
        if (self.headers.get("Content-Type") or "").split(";")[0].strip() != "application/json":
            raise GatewayError(415, "unsupported_media_type", "request must use application/json")
        try:
            raw = Body(self.rfile, self.headers, MAX_JSON_BYTES).read_all()
        except TooLarge:
            self.close_connection = True
            raise GatewayError(413, "request_too_large", "request body is too large") from None
        try:
            value = json.loads(raw or b"{}")
        except ValueError as error:
            raise GatewayError(400, "invalid_json", "request body must be a JSON object") from error
        if not isinstance(value, dict):
            raise GatewayError(400, "invalid_json", "request body must be a JSON object")
        return value

    def _file(self, path: Path, content_type: str, vary: bool = False) -> None:
        try:
            body = path.read_bytes()
        except FileNotFoundError as error:
            raise GatewayError(404, "not_found", "file not found") from error
        self._bytes(200, body, content_type, vary=vary)

    def _json(self, status: int, value: dict[str, Any]) -> None:
        self._bytes(status, json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode(), "application/json; charset=utf-8")

    def _error(self, error: GatewayError) -> None:
        if self.started:
            self.close_connection = True
            return
        if self.command in ("POST", "PUT", "PATCH", "DELETE"):
            # The request body may be unread; never reuse the connection after an early error.
            self.close_connection = True
        if self.git_route:
            # 401 would make Git run credential helpers or prompt; the gateway owns the credential.
            body = f"gateway: {error.message}\n".encode()
            self._bytes(403 if error.status == 401 else error.status, body, "text/plain; charset=utf-8")
            return
        self._json(error.status, {"error": {"code": error.code, "message": error.message}})

    def _bytes(self, status: int, body: bytes, content_type: str, vary: bool = False) -> None:
        self.started = True
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
        if content_type.startswith("text/html"):
            github = origin_of(self.gw.settings.github_url)
            self.send_header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; "
                f"base-uri 'none'; form-action 'self' {github}; frame-ancestors 'none'",
            )
            self.send_header("Referrer-Policy", "no-referrer")
        if self.close_connection:
            self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002
        if self.server.quiet:
            return
        print(f"git-gateway {self.address_string()} {format % args}", file=sys.stderr)


def upstream(
    base_url: str,
    method: str,
    path: str,
    headers: dict[str, str],
    body: Iterator[bytes] | None,
    length: int | None,
) -> tuple[http.client.HTTPConnection, http.client.HTTPResponse]:
    """Send one request to GitHub, streaming ``body``; redirects are returned, never followed."""
    parts = urlsplit(base_url)
    connection_type = http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
    connection = connection_type(parts.hostname, parts.port, timeout=UPSTREAM_TIMEOUT)
    prefix = parts.path.rstrip("/")
    try:
        connection.putrequest(method, prefix + path, skip_accept_encoding=True)
        for name, value in headers.items():
            connection.putheader(name, value)
        chunked = body is not None and length is None
        if chunked:
            connection.putheader("Transfer-Encoding", "chunked")
        elif body is not None:
            connection.putheader("Content-Length", str(length))
        connection.endheaders()
        if body is not None:
            for data in body:
                if not data:
                    continue
                connection.send(f"{len(data):x}\r\n".encode() + data + b"\r\n" if chunked else data)
            if chunked:
                connection.send(b"0\r\n\r\n")
        return connection, connection.getresponse()
    except BaseException:
        connection.close()
        raise


# --------------------------------------------------------------------------- main


def parse_listen(value: str) -> tuple[str, int]:
    host, _, port = value.rpartition(":")
    if not host or not port.isdigit() or not 0 <= int(port) <= 65535:
        raise ValueError("listen address must be HOST:PORT")
    return host, int(port)


def settings_from(argv: list[str] | None = None) -> Settings:
    env = os.environ
    parser = argparse.ArgumentParser(description="Treer Git Gateway (configuration also via GATEWAY_* env)")
    parser.add_argument("--listen", default=env.get("GATEWAY_LISTEN", "127.0.0.1:9450"))
    parser.add_argument("--data-dir", default=env.get("GATEWAY_DATA_DIR", ".treer/apps/git-gateway"))
    parser.add_argument("--proxy-url", default=env.get("GATEWAY_PROXY_URL") or env.get("TREER_PROXY_URL"))
    parser.add_argument("--service-id", default=env.get("GATEWAY_SERVICE_ID"))
    parser.add_argument("--workspace-id", default=env.get("GATEWAY_WORKSPACE_ID") or env.get("TREER_WORKSPACE_ID"))
    parser.add_argument("--public-url", default=env.get("GATEWAY_PUBLIC_URL"))
    parser.add_argument("--agent-url", default=env.get("GATEWAY_AGENT_URL", "http://git-gateway.treer"))
    parser.add_argument("--github-url", default=env.get("GATEWAY_GITHUB_URL", "https://github.com"))
    parser.add_argument("--github-api-url", default=env.get("GATEWAY_GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--gh-bin", default=env.get("GATEWAY_GH_BIN", "gh"))
    parser.add_argument("--gh-refresh-seconds", type=float, default=float(env.get("GATEWAY_GH_REFRESH_SECONDS", "600")))
    parser.add_argument("--seed-repos", default=env.get("GATEWAY_SEED_REPOS"), help="comma list; first start only")
    parser.add_argument("--seed-trusted-agents", default=env.get("GATEWAY_SEED_TRUSTED_AGENTS"), help="comma list; first start only")
    args = parser.parse_args(argv)
    for name in ("proxy_url", "public_url", "agent_url", "github_url", "github_api_url"):
        value = getattr(args, name)
        if value is not None:
            parts = urlsplit(value)
            if parts.scheme not in ("http", "https") or not parts.netloc or parts.username or parts.password:
                raise ValueError(f"--{name.replace('_', '-')} must be an http(s) URL without credentials")
    seed: dict[str, Any] = {}
    if args.seed_repos:
        seed["repos"] = [item.strip() for item in args.seed_repos.split(",") if item.strip()]
    if args.seed_trusted_agents:
        seed["trusted_agents"] = [item.strip() for item in args.seed_trusted_agents.split(",") if item.strip()]
    return Settings(
        listen=parse_listen(args.listen),
        data_dir=Path(args.data_dir).expanduser().resolve(),
        proxy_url=args.proxy_url,
        service_id=args.service_id,
        workspace_id=args.workspace_id,
        public_url=args.public_url,
        agent_url=args.agent_url,
        github_url=args.github_url.rstrip("/"),
        github_api_url=args.github_api_url.rstrip("/"),
        gh_bin=args.gh_bin,
        gh_refresh_seconds=args.gh_refresh_seconds,
        treer_bin=env.get("TREER_BIN", "treer"),
        seed=seed,
    )


def main(argv: list[str] | None = None) -> int:
    os.umask(0o077)
    settings = settings_from(argv)
    gateway = Gateway(settings)
    server = GatewayServer(settings.listen, gateway)
    print(f"git-gateway listening on http://{settings.listen[0]}:{server.server_port} (state {settings.data_dir})", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
