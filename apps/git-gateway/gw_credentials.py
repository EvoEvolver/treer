"""GitHub credentials for the gateway: `gh auth token` or a GitHub App (stdlib only).

The gateway is the only holder of a GitHub credential. Two providers exist:

* ``GhCredential`` re-reads ``gh auth token`` from the gateway machine on a
  timer and after GitHub rejects the cached token.
* ``GitHubAppCredential`` signs an RS256 App JWT with the App's private key
  (pure-Python PKCS#1 v1.5, no third-party package) and exchanges it for
  short-lived installation tokens, cached until shortly before expiry.
"""

from __future__ import annotations

import base64
import hashlib
import http.client
import json
import os
import re
import subprocess
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit


class CredentialError(Exception):
    """No usable GitHub credential; the message never contains a token."""


# --------------------------------------------------------------------------- RS256


def _der_read(data: bytes, offset: int) -> tuple[int, int, int]:
    """Return (tag, value_start, value_end) of the DER TLV at ``offset``."""
    if offset + 2 > len(data):
        raise ValueError("truncated DER")
    tag = data[offset]
    length = data[offset + 1]
    offset += 2
    if length & 0x80:
        count = length & 0x7F
        if count == 0 or count > 4 or offset + count > len(data):
            raise ValueError("unsupported DER length")
        length = int.from_bytes(data[offset : offset + count], "big")
        offset += count
    if offset + length > len(data):
        raise ValueError("truncated DER value")
    return tag, offset, offset + length


def _der_children(data: bytes, start: int, end: int) -> list[tuple[int, int, int]]:
    children = []
    while start < end:
        tag, value_start, value_end = _der_read(data, start)
        children.append((tag, value_start, value_end))
        start = value_end
    return children


@dataclass(frozen=True)
class RsaPrivateKey:
    n: int
    e: int
    d: int
    p: int
    q: int
    dp: int
    dq: int
    qinv: int

    @property
    def size(self) -> int:
        return (self.n.bit_length() + 7) // 8


def load_rsa_private_key(pem: str) -> RsaPrivateKey:
    """Parse a PKCS#1 (``RSA PRIVATE KEY``) or PKCS#8 (``PRIVATE KEY``) PEM."""
    lines = [line.strip() for line in pem.strip().splitlines()]
    if not lines or not lines[0].startswith("-----BEGIN ") or not lines[-1].startswith("-----END "):
        raise ValueError("private key must be PEM")
    label = lines[0][len("-----BEGIN ") :].rstrip("-")
    der = base64.b64decode("".join(line for line in lines[1:-1] if ":" not in line), validate=True)
    tag, start, end = _der_read(der, 0)
    if tag != 0x30:
        raise ValueError("private key is not a DER sequence")
    if label == "PRIVATE KEY":
        fields = _der_children(der, start, end)
        if len(fields) < 3 or fields[2][0] != 0x04:
            raise ValueError("PKCS#8 key has no private key octet string")
        rsa_oid = bytes.fromhex("2a864886f70d010101")
        if rsa_oid not in der[fields[1][1] : fields[1][2]]:
            raise ValueError("PKCS#8 key is not RSA")
        der = der[fields[2][1] : fields[2][2]]
        tag, start, end = _der_read(der, 0)
    elif label != "RSA PRIVATE KEY":
        raise ValueError("unsupported private key type")
    integers = [
        int.from_bytes(der[value_start:value_end], "big")
        for item_tag, value_start, value_end in _der_children(der, start, end)
        if item_tag == 0x02
    ]
    if len(integers) < 9 or integers[0] != 0:
        raise ValueError("not a two-prime RSA private key")
    key = RsaPrivateKey(*integers[1:9])
    if key.p * key.q != key.n or key.n.bit_length() < 2048:
        raise ValueError("RSA key is inconsistent or shorter than 2048 bits")
    return key


_SHA256_DIGEST_INFO = bytes.fromhex("3031300d060960864801650304020105000420")


def rsa_sha256_sign(key: RsaPrivateKey, message: bytes) -> bytes:
    """RSASSA-PKCS1-v1_5 with SHA-256 (the JWS ``RS256`` algorithm)."""
    digest_info = _SHA256_DIGEST_INFO + hashlib.sha256(message).digest()
    padding = key.size - len(digest_info) - 3
    if padding < 8:
        raise ValueError("RSA key too small")
    encoded = int.from_bytes(b"\x00\x01" + b"\xff" * padding + b"\x00" + digest_info, "big")
    # Chinese remainder theorem; then verify to catch arithmetic faults
    # before a signature could leak key material.
    m1 = pow(encoded, key.dp, key.p)
    m2 = pow(encoded, key.dq, key.q)
    signature = m2 + ((key.qinv * (m1 - m2)) % key.p) * key.q
    if pow(signature, key.e, key.n) != encoded:
        raise ValueError("RSA signature self-check failed")
    return signature.to_bytes(key.size, "big")


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def github_app_jwt(app_id: str | int, key: RsaPrivateKey, now: float | None = None) -> str:
    """App JWT: issued 60 s in the past (clock skew), valid 9 minutes (GitHub max 10)."""
    issued = int(now if now is not None else time.time()) - 60
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
    claims = b64url(
        json.dumps({"iat": issued, "exp": issued + 600, "iss": str(app_id)}, separators=(",", ":")).encode()
    )
    signing_input = f"{header}.{claims}".encode("ascii")
    return f"{header}.{claims}.{b64url(rsa_sha256_sign(key, signing_input))}"


# --------------------------------------------------------------------------- HTTP


def github_json(
    method: str,
    url: str,
    headers: dict[str, str] | None = None,
    body: dict[str, Any] | None = None,
    timeout: float = 20,
) -> tuple[int, dict[str, str], Any]:
    """One JSON call to GitHub; never follows redirects, never uses env proxies."""
    parts = urlsplit(url)
    connection_type = http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
    connection = connection_type(parts.hostname, parts.port, timeout=timeout)
    request_headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "treer-git-gateway",
        "X-GitHub-Api-Version": "2022-11-28",
        **(headers or {}),
    }
    raw_body = None
    if body is not None:
        raw_body = json.dumps(body).encode()
        request_headers["Content-Type"] = "application/json"
    path = parts.path + (f"?{parts.query}" if parts.query else "")
    try:
        connection.request(method, path or "/", body=raw_body, headers=request_headers)
        response = connection.getresponse()
        raw = response.read(4 * 1024 * 1024 + 1)
        response_headers = {name.lower(): value for name, value in response.getheaders()}
    finally:
        connection.close()
    try:
        value = json.loads(raw) if raw else None
    except ValueError:
        value = None
    return response.status, response_headers, value


def parse_time(value: str) -> float:
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def iso(timestamp: float | None) -> str | None:
    if timestamp is None:
        return None
    return datetime.fromtimestamp(timestamp, timezone.utc).isoformat().replace("+00:00", "Z")


# --------------------------------------------------------------------------- gh


class GhCredential:
    """``gh auth token`` from the gateway machine's existing GitHub CLI login."""

    def __init__(
        self,
        gh_bin: str = "gh",
        hostname: str = "github.com",
        refresh_seconds: float = 600,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self.gh_bin = gh_bin
        self.hostname = hostname
        self.refresh_seconds = refresh_seconds
        self.clock = clock
        self.lock = threading.Lock()
        self.token_value: str | None = None
        self.fetched_at: float | None = None
        self.last_error: str | None = None
        self.reads = 0

    def _read(self) -> str:
        self.reads += 1
        try:
            completed = subprocess.run(
                [self.gh_bin, "auth", "token", "--hostname", self.hostname],
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
                env={**os.environ, "GH_PROMPT_DISABLED": "1", "NO_COLOR": "1"},
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise CredentialError(f"gh is unavailable: {type(error).__name__}") from error
        token = completed.stdout.strip()
        if completed.returncode != 0 or not token or any(char.isspace() for char in token):
            raise CredentialError("gh auth token failed; run `gh auth login` on the gateway machine")
        return token

    def token(self) -> str:
        with self.lock:
            now = self.clock()
            if self.token_value and self.fetched_at is not None and now - self.fetched_at < self.refresh_seconds:
                return self.token_value
            try:
                self.token_value = self._read()
                self.fetched_at = now
                self.last_error = None
            except CredentialError as error:
                self.last_error = str(error)
                self.token_value = None
                raise
            return self.token_value

    def invalidate(self) -> None:
        """GitHub rejected the token: the next ``token()`` re-reads ``gh``."""
        with self.lock:
            self.token_value = None
            self.fetched_at = None

    def describe(self) -> dict[str, Any]:
        with self.lock:
            return {
                "kind": "gh",
                "hostname": self.hostname,
                "available": self.token_value is not None,
                "token_type": self.token_value.split("_", 1)[0] if self.token_value and re.match(r"gh[a-z]_", self.token_value) else None,
                "fetched_at": iso(self.fetched_at),
                "refresh_seconds": self.refresh_seconds,
                "next_refresh_at": iso(self.fetched_at + self.refresh_seconds) if self.fetched_at else None,
                "error": self.last_error,
            }


# --------------------------------------------------------------------------- GitHub App


class GitHubAppCredential:
    """A GitHub App created through the manifest flow and stored in App state."""

    METADATA = "github-app.json"
    PRIVATE_KEY = "github-app.pem"

    def __init__(self, state_dir: Path, api_url: str, clock: Callable[[], float] = time.time) -> None:
        self.state_dir = state_dir
        self.api_url = api_url.rstrip("/")
        self.clock = clock
        self.lock = threading.Lock()
        self.metadata: dict[str, Any] | None = None
        self.key: RsaPrivateKey | None = None
        self.installations: list[dict[str, Any]] = []
        self.installations_at = -1e18
        self.tokens: dict[int, tuple[str, float]] = {}
        self.last_error: str | None = None
        self._load()

    # state -----------------------------------------------------------------
    def _load(self) -> None:
        try:
            metadata = json.loads((self.state_dir / self.METADATA).read_text(encoding="utf-8"))
            key = load_rsa_private_key((self.state_dir / self.PRIVATE_KEY).read_text(encoding="utf-8"))
        except FileNotFoundError:
            return
        except (OSError, ValueError) as error:
            self.last_error = f"stored GitHub App is unreadable: {type(error).__name__}"
            return
        if isinstance(metadata, dict) and metadata.get("id"):
            self.metadata, self.key = metadata, key

    @property
    def configured(self) -> bool:
        return self.metadata is not None and self.key is not None

    def store(self, conversion: dict[str, Any]) -> dict[str, Any]:
        """Persist the manifest conversion result: metadata 0600 JSON and key 0600 PEM."""
        pem = conversion.get("pem")
        if not isinstance(pem, str) or not conversion.get("id"):
            raise CredentialError("GitHub returned an incomplete App conversion")
        key = load_rsa_private_key(pem)
        owner = conversion.get("owner") if isinstance(conversion.get("owner"), dict) else {}
        metadata = {
            "id": conversion["id"],
            "slug": conversion.get("slug"),
            "name": conversion.get("name"),
            "html_url": conversion.get("html_url"),
            "client_id": conversion.get("client_id"),
            "owner": owner.get("login"),
            "owner_type": owner.get("type"),
            "created_at": iso(self.clock()),
        }
        # The client and webhook secrets are not needed and are deliberately not kept.
        write_private(self.state_dir / self.PRIVATE_KEY, pem.encode())
        write_private(self.state_dir / self.METADATA, json.dumps(metadata, indent=2).encode())
        with self.lock:
            self.metadata, self.key = metadata, key
            self.installations, self.installations_at, self.tokens = [], -1e18, {}
            self.last_error = None
        return metadata

    def forget(self) -> None:
        for name in (self.PRIVATE_KEY, self.METADATA):
            try:
                (self.state_dir / name).unlink()
            except FileNotFoundError:
                pass
        with self.lock:
            self.metadata = self.key = None
            self.installations, self.tokens = [], {}

    # tokens ----------------------------------------------------------------
    def jwt(self) -> str:
        if not self.configured:
            raise CredentialError("no GitHub App is configured")
        return github_app_jwt(self.metadata["id"], self.key, self.clock())  # type: ignore[index,arg-type]

    def list_installations(self, force: bool = False) -> list[dict[str, Any]]:
        with self.lock:
            fresh = self.clock() - self.installations_at < 300
            if fresh and not force:
                return list(self.installations)
        status, _, value = github_json(
            "GET", f"{self.api_url}/app/installations?per_page=100", {"Authorization": f"Bearer {self.jwt()}"}
        )
        if status != 200 or not isinstance(value, list):
            self.last_error = f"listing installations failed with HTTP {status}"
            raise CredentialError(self.last_error)
        installations = [
            {
                "id": item.get("id"),
                "account": (item.get("account") or {}).get("login"),
                "account_type": (item.get("account") or {}).get("type"),
                "repository_selection": item.get("repository_selection"),
                "html_url": item.get("html_url"),
            }
            for item in value
            if isinstance(item, dict) and isinstance(item.get("id"), int)
        ]
        with self.lock:
            self.installations, self.installations_at, self.last_error = installations, self.clock(), None
        return list(installations)

    def installation_for(self, owner: str | None) -> int | None:
        if not self.configured:
            return None
        try:
            installations = self.list_installations()
        except (CredentialError, OSError):
            return None
        if owner:
            for item in installations:
                if str(item.get("account", "")).lower() == owner.lower():
                    return int(item["id"])
            return None
        return int(installations[0]["id"]) if len(installations) == 1 else None

    def token(self, owner: str | None) -> str:
        installation = self.installation_for(owner)
        if installation is None:
            raise CredentialError(f"the GitHub App is not installed for {owner or 'this request'}")
        with self.lock:
            cached = self.tokens.get(installation)
            if cached and cached[1] - self.clock() > 300:
                return cached[0]
        status, _, value = github_json(
            "POST",
            f"{self.api_url}/app/installations/{installation}/access_tokens",
            {"Authorization": f"Bearer {self.jwt()}"},
        )
        if status != 201 or not isinstance(value, dict) or not isinstance(value.get("token"), str):
            self.last_error = f"installation token request failed with HTTP {status}"
            raise CredentialError(self.last_error)
        expires = parse_time(value["expires_at"]) if value.get("expires_at") else self.clock() + 3600
        with self.lock:
            self.tokens[installation] = (value["token"], expires)
        return value["token"]

    def invalidate(self, owner: str | None) -> None:
        with self.lock:
            self.tokens.clear()
            self.installations_at = -1e18

    def describe(self) -> dict[str, Any]:
        with self.lock:
            metadata = dict(self.metadata) if self.metadata else None
            expiry = min((expires for _, expires in self.tokens.values()), default=None)
            installations = list(self.installations)
        return {
            "kind": "github_app",
            "configured": metadata is not None,
            "app": metadata,
            "install_url": f"{metadata['html_url']}/installations/new" if metadata and metadata.get("html_url") else None,
            "installations": installations,
            "token_expires_at": iso(expiry),
            "error": self.last_error,
        }


def write_private(path: Path, data: bytes) -> None:
    """Atomically replace ``path`` with a 0600 file."""
    temporary = path.with_name(f".{path.name}.{os.getpid()}.{threading.get_ident()}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        raise


# --------------------------------------------------------------------------- selection


class Credentials:
    """Selects the provider for a request according to ``credential_mode``."""

    def __init__(self, gh: GhCredential, app: GitHubAppCredential, mode: Callable[[], str]) -> None:
        self.gh = gh
        self.app = app
        self.mode = mode

    def get(self, owner: str | None) -> tuple[str, str]:
        mode = self.mode()
        if mode in ("github_app", "auto") and self.app.configured:
            try:
                return self.app.token(owner), "github_app"
            except (CredentialError, OSError):
                if mode == "github_app":
                    raise
        if mode == "github_app":
            raise CredentialError("credential_mode is github_app but no GitHub App is configured")
        return self.gh.token(), "gh"

    def invalidate(self, source: str, owner: str | None) -> None:
        if source == "github_app":
            self.app.invalidate(owner)
        else:
            self.gh.invalidate()
