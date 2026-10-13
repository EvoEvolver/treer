#!/usr/bin/env python3
"""Treer Policy Provider v1 with a small browser control surface."""

from __future__ import annotations

import argparse
import copy
import json
import hmac
import os
import threading
import tempfile
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import ProxyHandler, Request, build_opener, HTTPRedirectHandler


APP_ROOT = Path(__file__).resolve().parent
PROTOCOL = "treer.policy-provider/v1"
CAPABILITY = "policy.bundle.v1"
MAX_BODY_BYTES = 300 * 1024
MAX_HISTORY = 100
BUNDLED_INSTALL = (APP_ROOT / "treer-policy-index.html").is_file()


def bundled_path(source: Path, installed_name: str) -> Path:
    return APP_ROOT / installed_name if BUNDLED_INSTALL else source


ASSETS = {
    "/app.js": (
        "application/javascript; charset=utf-8",
        bundled_path(APP_ROOT / "web" / "app.js", "treer-policy-app.js"),
    ),
    "/app.css": (
        "text/css; charset=utf-8",
        bundled_path(APP_ROOT / "web" / "app.css", "treer-policy-app.css"),
    ),
}
INDEX_PATH = bundled_path(APP_ROOT / "web" / "index.html", "treer-policy-index.html")
AGENT_PATH = bundled_path(APP_ROOT / "AGENT.md", "treer-policy-agent.md")


class PolicyError(Exception):
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


def validate_token(value: object, field: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode()) > 128
        or value.strip() != value
        or any(not character.isprintable() for character in value)
    ):
        raise PolicyError(400, "invalid_policy_document", f"{field} is invalid")
    return value


def validate_document(value: object) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) - {"schema_version", "defaults", "groups", "rules"}:
        raise PolicyError(400, "invalid_policy_document", "policy document contains invalid fields")
    if value.get("schema_version") != 1:
        raise PolicyError(400, "unsupported_policy_schema", "only policy schema version 1 is supported")
    defaults = value.get("defaults", {})
    groups = value.get("groups", {})
    rules = value.get("rules", [])
    if not isinstance(defaults, dict) or not isinstance(groups, dict) or not isinstance(rules, list):
        raise PolicyError(400, "invalid_policy_document", "defaults, groups, and rules have invalid types")
    if len(rules) > 1000:
        raise PolicyError(400, "too_many_policy_rules", "policy may contain at most 1000 rules")
    for action, effect in defaults.items():
        validate_token(action, "default action")
        if effect not in {"allow", "deny"}:
            raise PolicyError(400, "invalid_policy_document", "default effect must be allow or deny")
    for name, group in groups.items():
        validate_token(name, "group name")
        if not isinstance(group, dict) or set(group) != {"principals"} or not isinstance(group["principals"], list):
            raise PolicyError(400, "invalid_policy_document", "group principals are invalid")
        for principal in group["principals"]:
            if not isinstance(principal, dict) or set(principal) != {"kind", "id"}:
                raise PolicyError(400, "invalid_policy_document", "group principal is invalid")
            if principal["kind"] not in {"human", "agent", "machine", "service"}:
                raise PolicyError(400, "invalid_policy_document", "principal kind is invalid")
            validate_token(principal["id"], "principal ID")
    seen: set[str] = set()
    for rule in rules:
        required = {"id", "priority", "effect", "subjects", "actions", "resources"}
        if not isinstance(rule, dict) or set(rule) != required:
            raise PolicyError(400, "invalid_policy_rule", "policy rule fields are invalid")
        rule_id = validate_token(rule["id"], "rule ID")
        if rule_id in seen:
            raise PolicyError(400, "duplicate_policy_rule", "policy rule IDs must be unique")
        seen.add(rule_id)
        if not isinstance(rule["priority"], int) or rule["effect"] not in {"allow", "deny"}:
            raise PolicyError(400, "invalid_policy_rule", "rule priority or effect is invalid")
        if not all(isinstance(rule[key], list) and rule[key] for key in ("subjects", "actions", "resources")):
            raise PolicyError(400, "invalid_policy_rule", "rules require subjects, actions, and resources")
        for action in rule["actions"]:
            validate_token(action, "rule action")
        for selector in rule["subjects"]:
            _validate_selector(selector, {"kind", "id", "machine_id", "group", "self"}, groups, True)
        for selector in rule["resources"]:
            _validate_selector(selector, {"kind", "id", "principal_group"}, groups, False)
    document = {"schema_version": 1, "defaults": defaults, "groups": groups, "rules": rules}
    if len(json.dumps(document, separators=(",", ":")).encode()) > 256 * 1024:
        raise PolicyError(400, "policy_document_too_large", "policy document exceeds 256 KiB")
    return document


def _validate_selector(selector: object, fields: set[str], groups: dict[str, Any], subject: bool) -> None:
    if not isinstance(selector, dict) or set(selector) - fields:
        raise PolicyError(400, "invalid_policy_rule", "rule selector is invalid")
    kind = selector.get("kind")
    if kind is not None:
        validate_token(kind, "selector kind")
        if subject and kind not in {"human", "agent", "machine", "service"}:
            raise PolicyError(400, "invalid_policy_rule", "subject kind is invalid")
    for field in ("id", "machine_id"):
        if selector.get(field) is not None:
            validate_token(selector[field], f"selector {field}")
    group = selector.get("group") or selector.get("principal_group")
    if group is not None and group not in groups:
        raise PolicyError(400, "unknown_policy_group", f"selector references unknown group {group}")
    if "self" in selector and not isinstance(selector["self"], bool):
        raise PolicyError(400, "invalid_policy_rule", "selector self must be boolean")


class PolicyStore:
    def __init__(self, path: Path, workspace_id: str) -> None:
        os.umask(0o077)
        self.path = path
        self.workspace_id = workspace_id
        self.lock = threading.RLock()
        self.last_sync: dict[str, Any] = {"status": "not_sent"}
        self.state = self._load()

    def _load(self) -> dict[str, Any]:
        if not self.path.exists():
            initial = {
                "revision": 1,
                "mode": "monitor",
                "document": {"schema_version": 1, "defaults": {}, "groups": {}, "rules": []},
                "published_at": utc_now(),
            }
            return {**initial, "history": [copy.deepcopy(initial)]}
        try:
            value = json.loads(self.path.read_text(encoding="utf-8"))
            document = validate_document(value.get("document"))
            if value.get("mode") not in {"monitor", "enforce"} or not isinstance(value.get("revision"), int) or value["revision"] < 1:
                raise ValueError("invalid persisted mode or revision")
            current = {key: value[key] for key in ("revision", "mode", "published_at")}
            current["document"] = document
            history = value.get("history", [copy.deepcopy(current)])
            if not isinstance(history, list) or not history or len(history) > MAX_HISTORY:
                raise ValueError("invalid revision history")
            previous = 0
            for item in history:
                if not isinstance(item, dict) or type(item.get("revision")) is not int or item["revision"] <= previous:
                    raise ValueError("revision history must increase")
                previous = item["revision"]
                validate_document(item.get("document"))
                if item.get("mode") not in {"monitor", "enforce"}:
                    raise ValueError("invalid historical mode")
            if history[-1]["revision"] != current["revision"] or any(history[-1].get(k) != current[k] for k in current):
                raise ValueError("current revision differs from history")
            if "history" not in value:
                # Exclusive private backup before the first v2 storage write. No
                # policy-language/mode change is made by this storage migration.
                backup = self.path.with_name(self.path.name + ".pre-history.bak")
                try:
                    with backup.open("xb") as handle:
                        os.chmod(backup, 0o600)
                        handle.write(self.path.read_bytes())
                except FileExistsError:
                    pass
            return {**current, "history": history}
        except (OSError, ValueError, json.JSONDecodeError, PolicyError) as error:
            raise RuntimeError(f"invalid Policy App state: {error}") from error

    def bundle(self) -> dict[str, Any]:
        with self.lock:
            return {
                "protocol": PROTOCOL,
                "workspace_id": self.workspace_id,
                "revision": self.state["revision"],
                "mode": self.state["mode"],
                "document": copy.deepcopy(self.state["document"]),
                "generated_at": self.state["published_at"],
            }

    def status(self) -> dict[str, Any]:
        with self.lock:
            return {"bundle": self.bundle(), "proxy_sync": dict(self.last_sync)}

    def history(self) -> dict[str, Any]:
        with self.lock:
            return {"revisions": [{key: copy.deepcopy(item.get(key)) for key in
                    ("revision", "mode", "published_at", "restored_from")}
                    for item in reversed(self.state["history"])], "retention": MAX_HISTORY}

    def revision(self, revision: int) -> dict[str, Any]:
        with self.lock:
            item = next((item for item in self.state["history"] if item["revision"] == revision), None)
            if item is None:
                raise PolicyError(404, "revision_not_found", "Revision is not retained")
            return copy.deepcopy(item)

    def rollback(self, expected_revision: int, revision: int) -> dict[str, Any]:
        with self.lock:
            item = self.revision(revision)
            return self.publish({"expected_revision": expected_revision,
                                 "mode": item["mode"], "document": item["document"]}, restored_from=revision)

    def publish(self, body: object, *, restored_from: int | None = None) -> dict[str, Any]:
        if not isinstance(body, dict) or set(body) != {"expected_revision", "mode", "document"}:
            raise PolicyError(400, "invalid_publish_request", "publish request fields are invalid")
        with self.lock:
            if type(body["expected_revision"]) is not int or body["expected_revision"] != self.state["revision"]:
                raise PolicyError(409, "policy_revision_conflict", "policy revision changed; reload before publishing")
            if body["mode"] not in {"monitor", "enforce"}:
                raise PolicyError(400, "invalid_policy_mode", "policy mode must be monitor or enforce")
            document = validate_document(body["document"])
            next_state = {
                "revision": self.state["revision"] + 1,
                "mode": body["mode"],
                "document": copy.deepcopy(document),
                "published_at": utc_now(),
            }
            if restored_from is not None:
                next_state["restored_from"] = restored_from
            next_state["history"] = (copy.deepcopy(self.state["history"]) + [copy.deepcopy(next_state)])[-MAX_HISTORY:]
            self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            descriptor, name = tempfile.mkstemp(prefix=".policy-", dir=self.path.parent)
            temporary = Path(name)
            try:
                with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                    handle.write(json.dumps(next_state, indent=2) + "\n")
                    handle.flush()
                    os.fsync(handle.fileno())
                temporary.replace(self.path)
            finally:
                temporary.unlink(missing_ok=True)
            self.state = next_state
            return self.bundle()

    def notify_proxy(self) -> dict[str, Any]:
        with self.lock:
            revision = self.state["revision"]
        agent_url = os.environ.get("TREER_AGENT_SERVER_URL", "").rstrip("/")
        agent_id = os.environ.get("TREER_AGENT_ID", "")
        credential = os.environ.get("TREER_WORKLOAD_CREDENTIAL", "")
        if not agent_url or not agent_id or not credential:
            result = {"status": "unavailable", "message": "Treer workload environment is incomplete", "attempted_at": utc_now()}
        else:
            request = Request(
                f"{agent_url}/api/policy-provider/invalidate",
                data=json.dumps({"revision": revision}).encode(),
                method="POST",
                headers={"content-type": "application/json", "x-treer-agent-id": agent_id, "x-treer-workload-credential": credential},
            )
            try:
                with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=5) as response:
                    payload = json.load(response)
                result = {"status": "synced", "accepted_revision": payload["accepted_revision"], "attempted_at": utc_now()}
            except (HTTPError, URLError, OSError, ValueError, KeyError) as error:
                result = {"status": "failed", "message": str(error), "attempted_at": utc_now()}
        with self.lock:
            self.last_sync = result
        return result


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class PolicyHandler(BaseHTTPRequestHandler):
    store: PolicyStore
    server_version = "TreerPolicy/1"

    def do_GET(self) -> None:
        parsed = urlsplit(self.path)
        try:
            if parsed.path == "/":
                self._root()
            elif parsed.path in ASSETS:
                content_type, path = ASSETS[parsed.path]
                self._send_bytes(200, content_type, path.read_bytes())
            elif parsed.path == "/health":
                self._json(200, {"status": "ok", "service": "treer-policy", "revision": self.store.bundle()["revision"]})
            elif parsed.path == "/v1/manifest":
                self._json(200, {"protocol": PROTOCOL, "provider_name": "Treer Policy", "capabilities": [CAPABILITY]})
            elif parsed.path == "/v1/policy/bundle":
                workspace_id = parse_qs(parsed.query).get("workspace_id", [""])[0]
                if workspace_id != self.store.workspace_id:
                    raise PolicyError(404, "workspace_not_found", "Policy App does not serve this workspace")
                self._json(200, self.store.bundle())
            elif parsed.path == "/v1/state":
                self._json(200, self.store.status())
            elif parsed.path == "/v1/policy/history":
                self._require_operator()
                self._json(200, self.store.history())
            elif parsed.path.startswith("/v1/policy/revisions/"):
                self._require_operator()
                try:
                    revision = int(parsed.path.rsplit("/", 1)[1])
                except ValueError as error:
                    raise PolicyError(400, "invalid_revision", "Revision must be an integer") from error
                self._json(200, self.store.revision(revision))
            else:
                raise PolicyError(404, "not_found", "route not found")
        except PolicyError as error:
            self._error(error)
        except OSError:
            self._error(PolicyError(500, "asset_unavailable", "browser asset is unavailable"))

    def do_POST(self) -> None:
        try:
            if urlsplit(self.path).path in ("/v1/policy/publish", "/v1/policy/invalidate", "/v1/policy/rollback"):
                self._require_operator()
                if self.headers.get("Sec-Fetch-Site") not in (None, "same-origin"):
                    raise PolicyError(403, "csrf_denied", "Cross-origin policy writes rejected")
                if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                    raise PolicyError(415, "invalid_content_type", "Policy writes require application/json")
            body = self._read_json()
            if urlsplit(self.path).path == "/v1/policy/publish":
                bundle = self.store.publish(body)
                sync = self.store.notify_proxy()
                self._json(200, {"bundle": bundle, "proxy_sync": sync})
            elif urlsplit(self.path).path == "/v1/policy/invalidate":
                if body != {}:
                    raise PolicyError(400, "invalid_invalidation_request", "invalidation request must be empty")
                self._json(200, {"proxy_sync": self.store.notify_proxy()})
            elif urlsplit(self.path).path == "/v1/policy/rollback":
                if not isinstance(body, dict) or set(body) != {"expected_revision", "revision"} or type(body["revision"]) is not int:
                    raise PolicyError(400, "invalid_rollback", "Rollback requires expected_revision and revision")
                bundle = self.store.rollback(body["expected_revision"], body["revision"])
                self._json(200, {"bundle": bundle, "proxy_sync": self.store.notify_proxy()})
            elif urlsplit(self.path).path == "/v1/policy/test":
                self._json(200, evaluate_request(body, self.store.bundle()))
            else:
                raise PolicyError(404, "not_found", "route not found")
        except PolicyError as error:
            self._error(error)

    def _require_operator(self) -> None:
        token = getattr(self.server, "admin_token", None)
        if not token:
            raise PolicyError(503, "policy_edits_locked", "Policy editing is disabled until an operator configures an admin token")
        supplied = self.headers.get("Authorization", "")
        if not hmac.compare_digest(supplied.encode(), ("Bearer " + token).encode()):
            raise PolicyError(401, "policy_admin_required", "Operator policy administration credential required")

    def _read_json(self) -> Any:
        try:
            length = int(self.headers.get("content-length", "0"))
        except ValueError as error:
            raise PolicyError(400, "invalid_json", "invalid Content-Length") from error
        if length <= 0 or length > MAX_BODY_BYTES:
            raise PolicyError(400, "invalid_json", "JSON body is empty or too large")
        try:
            return json.loads(self.rfile.read(length))
        except json.JSONDecodeError as error:
            raise PolicyError(400, "invalid_json", "request body is not valid JSON") from error

    def _root(self) -> None:
        representation = root_representation(self.headers.get("accept", ""), self.headers.get("user-agent", ""))
        if representation == "text/html":
            self._send_bytes(200, "text/html; charset=utf-8", INDEX_PATH.read_bytes(), vary=True)
        else:
            self._send_bytes(200, "text/markdown; charset=utf-8", AGENT_PATH.read_bytes(), vary=True)

    def _json(self, status: int, value: object) -> None:
        self._send_bytes(status, "application/json; charset=utf-8", json.dumps(value, separators=(",", ":")).encode())

    def _error(self, error: PolicyError) -> None:
        self._json(error.status, {"error": {"code": error.code, "message": error.message}})

    def _send_bytes(self, status: int, content_type: str, body: bytes, vary: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'")
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, pattern: str, *args: object) -> None:
        print(f"policy: {self.address_string()} {pattern % args}")


def evaluate_request(body: object, bundle: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(body, dict) or set(body) != {"subject", "action", "resource"}:
        raise PolicyError(400, "invalid_test_request", "test requires subject, action, and resource")
    subject, action, resource = body["subject"], body["action"], body["resource"]
    if not isinstance(subject, dict) or not isinstance(resource, dict):
        raise PolicyError(400, "invalid_test_request", "subject and resource must be objects")
    validate_token(action, "action")
    document = bundle["document"]
    groups = document["groups"]
    rules = sorted(document["rules"], key=lambda rule: (-rule["priority"], 0 if rule["effect"] == "deny" else 1))
    matched = next((rule for rule in rules if action in rule["actions"] and any(subject_matches(item, subject, resource, groups) for item in rule["subjects"]) and any(resource_matches(item, resource, groups) for item in rule["resources"])), None)
    effect = matched["effect"] if matched else document["defaults"].get(action, "allow")
    enforced = bundle["mode"] == "enforce" and effect == "deny"
    return {"revision": bundle["revision"], "mode": bundle["mode"], "effect": effect, "decision": "deny" if enforced else "allow", "matched_rule_id": matched["id"] if matched else None}


def group_contains(groups: dict[str, Any], name: str, kind: object, identifier: object) -> bool:
    return any(item.get("kind") == kind and item.get("id") == identifier for item in groups.get(name, {}).get("principals", []))


def subject_matches(selector: dict[str, Any], subject: dict[str, Any], resource: dict[str, Any], groups: dict[str, Any]) -> bool:
    return (selector.get("kind") in {None, subject.get("kind")} and selector.get("id") in {None, subject.get("id")} and selector.get("machine_id") in {None, subject.get("machine_id")} and (not selector.get("group") or group_contains(groups, selector["group"], subject.get("kind"), subject.get("id"))) and (not selector.get("self") or resource.get("id") == subject.get("id")))


def resource_matches(selector: dict[str, Any], resource: dict[str, Any], groups: dict[str, Any]) -> bool:
    principal = resource.get("principal", {})
    return (selector.get("kind") in {None, resource.get("kind")} and selector.get("id") in {None, resource.get("id")} and (not selector.get("principal_group") or group_contains(groups, selector["principal_group"], principal.get("kind"), principal.get("id"))))


def main() -> None:
    parser = argparse.ArgumentParser(description="Treer Policy Provider")
    parser.add_argument("--port", type=int, default=None)
    args = parser.parse_args()
    workspace_id = os.environ.get("TREER_WORKSPACE_ID", "").strip()
    if not workspace_id:
        raise SystemExit("TREER_WORKSPACE_ID is required")
    listen = os.environ.get(
        "POLICY_LISTEN", f"0.0.0.0:{args.port}" if args.port is not None else "0.0.0.0:8787"
    )
    host, raw_port = listen.rsplit(":", 1)
    default_data_file = (
        APP_ROOT / ".treer-policy-data.json"
        if BUNDLED_INSTALL
        else APP_ROOT / "data" / "policy.json"
    )
    data_file = Path(os.environ.get("POLICY_DATA_FILE", str(default_data_file))).resolve()
    PolicyHandler.store = PolicyStore(data_file, workspace_id)
    server = ThreadingHTTPServer((host, int(raw_port)), PolicyHandler)
    server.admin_token = os.environ.get("POLICY_ADMIN_TOKEN") or None
    if server.admin_token and (len(server.admin_token) < 32 or any(c.isspace() for c in server.admin_token)):
        raise SystemExit("POLICY_ADMIN_TOKEN must contain at least 32 non-whitespace characters")
    print(f"Treer Policy Provider listening on {listen} for {workspace_id}")
    server.serve_forever()


if __name__ == "__main__":
    main()
