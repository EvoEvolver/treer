"""The only place Mail talks to the Core App email route.

Core sends email on behalf of a Managed App: `POST /api/apps/{service_id}/email`
with `{to, subject, text, idempotency_key}`, authenticated by the App's own
runtime identity (a 60-second token from `treer token create SERVICE_ID`).
The Proxy holds the provider credentials and advertises the route in
`/api/health` `capabilities`. Every outcome is classified here so the caller
only decides what to do with `sent`, `retryable`, `permanent` and `uncertain`.
"""

from __future__ import annotations

import json
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Callable

TIMEOUT_SECONDS = 30
CAPABILITY_TTL_SECONDS = 60
# Core rejected the request and certainly sent nothing; retrying cannot help.
PERMANENT_STATUSES = {400, 403, 404, 405, 409, 413, 422}
# Core certainly sent nothing yet; the same request may succeed later.
RETRYABLE_STATUSES = {401, 408, 425, 429, 503}


@dataclass(frozen=True)
class Outcome:
    result: str  # sent | retryable | permanent | uncertain
    code: str
    email_id: str | None = None


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def treer_token(treer_bin: str, service_id: str) -> str:
    result = subprocess.run([treer_bin, "token", "create", service_id], capture_output=True,
                            text=True, timeout=15, check=True)
    token = result.stdout.strip()
    if not token:
        raise RuntimeError("treer token create returned no token")
    return token


class EmailRoute:
    def __init__(self, proxy_url: str, service_id: str, capability: str,
                 token: Callable[[], str]) -> None:
        self.proxy_url = proxy_url
        self.service_id = service_id
        self.capability = capability
        self.token = token
        self._checked_at = 0.0
        self._available = False

    def available(self) -> bool:
        """True when the Proxy advertises the email capability (cached briefly)."""
        if time.time() - self._checked_at < CAPABILITY_TTL_SECONDS:
            return self._available
        try:
            status, value = self._call("GET", "api/health", None, None)
            self._available = status == 200 and self.capability in (value.get("capabilities") or [])
        except OSError:
            self._available = False
        self._checked_at = time.time()
        return self._available

    def send(self, to: str, subject: str, text: str, key: str) -> Outcome:
        try:
            token = self.token()
        except (OSError, subprocess.SubprocessError, RuntimeError):
            return Outcome("retryable", "runtime_token_unavailable")
        body = {"to": to, "subject": subject, "text": text, "idempotency_key": key}
        try:
            status, value = self._call("POST", f"api/apps/{self.service_id}/email", body, token)
        except urllib.error.URLError as error:
            # Refused/unresolvable connections never reached Core; anything else
            # (timeouts, resets) may have been processed.
            if isinstance(error.reason, (ConnectionRefusedError, socket.gaierror)):
                return Outcome("retryable", "email_route_unreachable")
            return Outcome("uncertain", "email_route_no_response")
        except OSError:
            return Outcome("uncertain", "email_route_no_response")
        code = str((value.get("error") or {}).get("code") or f"http_{status}")
        if 200 <= status < 300:
            if value.get("status") == "sent":
                return Outcome("sent", "sent", str(value.get("email_id") or "") or None)
            return Outcome("uncertain", "email_route_unexpected_response")
        if status in PERMANENT_STATUSES:
            return Outcome("permanent", code)
        if status in RETRYABLE_STATUSES:
            return Outcome("retryable", code)
        # 500/502/504 and friends: the provider may already have accepted it.
        return Outcome("uncertain", code)

    def lookup(self, key: str) -> str | None:
        """Ask Core what happened to an idempotency key: `sent`, `not_sent` or
        None when Core offers no such check (or it failed)."""
        try:
            status, value = self._call(
                "GET", f"api/apps/{self.service_id}/email/{urllib.parse.quote(key, safe='')}",
                None, self.token())
        except (OSError, subprocess.SubprocessError, RuntimeError, urllib.error.URLError):
            return None
        if status == 200 and value.get("status") == "sent":
            return "sent"
        if status == 200 and value.get("status") == "failed":
            return "not_sent"
        if status == 404 and (value.get("error") or {}).get("code") == "email_not_found":
            return "not_sent"
        return None

    def _call(self, method: str, path: str, body: dict | None, token: str | None) -> tuple[int, dict]:
        headers = {"Accept": "application/json"}
        data = None
        if token:
            headers["Authorization"] = f"Bearer {token}"
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        request = urllib.request.Request(urllib.parse.urljoin(self.proxy_url, path), data=data,
                                         headers=headers, method=method)
        try:
            with urllib.request.build_opener(_NoRedirect()).open(request, timeout=TIMEOUT_SECONDS) as response:
                status, payload = response.status, response.read(65536)
        except urllib.error.HTTPError as error:
            status, payload = error.code, error.read(65536)
        try:
            value = json.loads(payload or b"{}")
        except ValueError:
            value = {}
        return status, value if isinstance(value, dict) else {}
