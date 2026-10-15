"""Treer App identity verification (the contract the Issues App uses), without local sessions or OAuth.

Every request must carry a short-lived Treer service-audience token: the Proxy-injected `X-Treer-Identity-Token`
(humans in the workspace UI, forwarded per request) or an explicit Bearer token (Agents). The token is verified by
Treer's trusted Proxy for THIS service and workspace and the principal must still be in the workspace directory. Nothing
is stored: no cookies, no sessions, no token copies.
"""
from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any


class Error(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        self.status, self.code, self.message = status, code, message
        super().__init__(message)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args: Any, **kwargs: Any) -> None:
        return None


class Auth:
    def __init__(self, proxy_url: str, service_id: str, workspace_id: str, public_url: str) -> None:
        self.proxy_url = proxy_url.rstrip("/") + "/"
        self.service_id, self.workspace_id = service_id, workspace_id
        self.public_url = public_url.rstrip("/") + "/"

    def request(self, method: str, path: str, token: str | None = None, body: Any = None) -> dict[str, Any]:
        headers = {"Accept": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(urllib.parse.urljoin(self.proxy_url, path.lstrip("/")), data=data, headers=headers, method=method)
        try:
            with urllib.request.build_opener(_NoRedirect()).open(request, timeout=10) as response:
                result = json.loads(response.read(2 * 1024 * 1024))
                if not isinstance(result, dict):
                    raise ValueError()
                return result
        except urllib.error.HTTPError as exc:
            raise Error(exc.code if exc.code in (400, 401, 403, 404) else 502, "treer_request_failed", "Treer rejected the request") from exc
        except (OSError, ValueError) as exc:
            raise Error(502, "treer_unavailable", "Treer API unavailable") from exc

    def verify(self, token: str | None) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        if not token or len(token) > 8192:
            raise Error(401, "authentication_required", "Open this page from the Treer workspace (or send a Treer service token)")
        result = self.request("POST", "/.treer/apps/identity/verify", body={"token": token, "audience": self.service_id})
        claims = result.get("claims") or {}
        if (result.get("active") is not True or claims.get("service_id") != self.service_id or claims.get("workspace_id") != self.workspace_id
                or claims.get("principal_kind") not in ("human", "agent") or not claims.get("sub")):
            raise Error(401, "identity_invalid", "Identity is inactive or outside this App's workspace")
        directory = self.request("GET", f"/api/apps/{self.service_id}/directory", token).get("principals")
        if not isinstance(directory, list):
            raise Error(502, "treer_unavailable", "Treer API unavailable")
        principal = next((p for p in directory if p.get("id") == claims["sub"] and p.get("kind") == claims["principal_kind"]), None)
        if principal is None:
            raise Error(403, "identity_unavailable", "Caller is absent from the workspace directory")
        return principal, directory

    def authenticate(self, headers: Any) -> dict[str, Any]:
        # Header presence (even empty or invalid) is authoritative: never fall back to another credential after a rejection.
        if "X-Treer-Identity-Token" in headers:
            token = headers.get("X-Treer-Identity-Token")
            if not token:
                raise Error(401, "identity_invalid", "Injected identity token is empty")
        elif "Authorization" in headers:
            value = headers.get("Authorization", "")
            token = value[7:] if value.startswith("Bearer ") else None
        else:
            token = None
        principal, _ = self.verify(token)
        return principal

    def check_browser_mutation(self, headers: Any, principal: dict[str, Any], header: str) -> None:
        """CSRF defence for writes: same-site Fetch Metadata, the exact App/Proxy origin, and a custom request header
        (required for humans - their identity token is injected by the browser's ambient session)."""
        site = headers.get("Sec-Fetch-Site")
        if site is not None and site not in ("same-origin", "same-site"):
            raise Error(403, "csrf_rejected", "Cross-site browser requests are rejected")
        origin = headers.get("Origin")
        if origin is not None:
            allowed = {self.origin(self.public_url), self.origin(self.proxy_url)}
            if origin not in allowed:
                raise Error(403, "csrf_rejected", "Browser origin is not this App or its Treer tunnel")
        if principal["kind"] == "human" and headers.get(header) != "1":
            raise Error(403, "csrf_rejected", f"Human writes require {header}: 1")

    @staticmethod
    def origin(value: str) -> str:
        url = urllib.parse.urlsplit(value)
        return f"{url.scheme}://{url.netloc}"
