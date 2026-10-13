#!/usr/bin/env python3
"""Treer Policy Provider v1 with a small browser control surface."""

from __future__ import annotations

import base64
import hashlib
import secrets
import stat
import sqlite3
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from http.cookies import SimpleCookie, CookieError
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
    if type(value.get("schema_version")) is not int or value["schema_version"] not in (1, 2):
        raise PolicyError(400, "unsupported_policy_schema", "policy schema must be version 1 or 2")
    schema = value["schema_version"]
    defaults = value.get("defaults", {})
    groups = value.get("groups", {})
    rules = value.get("rules", [])
    if not isinstance(defaults, dict) or not isinstance(groups, dict) or not isinstance(rules, list):
        raise PolicyError(400, "invalid_policy_document", "defaults, groups, and rules have invalid types")
    if len(rules) > 1000:
        raise PolicyError(400, "too_many_policy_rules", "policy may contain at most 1000 rules")
    for action, effect in defaults.items():
        validate_token(action, "default action")
        if effect not in ("allow", "deny"):
            raise PolicyError(400, "invalid_policy_document", "default effect must be allow or deny")
    for name, group in groups.items():
        validate_token(name, "group name")
        if not isinstance(group, dict) or set(group) != {"principals"} or not isinstance(group["principals"], list):
            raise PolicyError(400, "invalid_policy_document", "group principals are invalid")
        for principal in group["principals"]:
            if not isinstance(principal, dict) or set(principal) != {"kind", "id"}:
                raise PolicyError(400, "invalid_policy_document", "group principal is invalid")
            if principal["kind"] not in ("human", "agent", "machine", "service"):
                raise PolicyError(400, "invalid_policy_document", "principal kind is invalid")
            validate_token(principal["id"], "principal ID")
    seen: set[str] = set()
    for rule in rules:
        required = {"id", "priority", "effect", "subjects", "actions", "resources"}
        optional = {"bindings", "conditions"} if schema == 2 else set()
        if not isinstance(rule, dict) or not required <= set(rule) or set(rule) - required - optional:
            raise PolicyError(400, "invalid_policy_rule", "policy rule fields are invalid")
        rule_id = validate_token(rule["id"], "rule ID")
        if rule_id in seen:
            raise PolicyError(400, "duplicate_policy_rule", "policy rule IDs must be unique")
        seen.add(rule_id)
        if type(rule["priority"]) is not int or not -(2**31) <= rule["priority"] < 2**31 or rule["effect"] not in ("allow", "deny"):
            raise PolicyError(400, "invalid_policy_rule", "rule priority or effect is invalid")
        if not all(isinstance(rule[key], list) and rule[key] for key in ("subjects", "actions", "resources")):
            raise PolicyError(400, "invalid_policy_rule", "rules require subjects, actions, and resources")
        for action in rule["actions"]:
            validate_token(action, "rule action")
        for selector in rule["subjects"]:
            _validate_selector(selector, {"kind", "id", "machine_id", "group", "self"} | ({"tree"} if schema == 2 else set()), groups, True)
        for selector in rule["resources"]:
            _validate_selector(selector, {"kind", "id", "principal_group"} | ({"tree"} if schema == 2 else set()), groups, False)
        if schema == 2:
            validate_expressions(rule)
    document = {"schema_version": schema, "defaults": defaults, "groups": groups, "rules": rules}
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
    if selector.get("tree") is not None:
        validate_tree_selector(selector["tree"])
    for field in ("group", "principal_group"):
        group = selector.get(field)
        if group is not None and (not isinstance(group, str) or group not in groups):
            raise PolicyError(400, "unknown_policy_group", "selector references an unknown group")
    if "self" in selector and not isinstance(selector["self"], bool):
        raise PolicyError(400, "invalid_policy_rule", "selector self must be boolean")



def validate_path(value, glob=False):
    if not isinstance(value, str) or not value.startswith('/') or len(value.encode()) > 4096 or any(not c.isprintable() for c in value):
        raise PolicyError(400, 'invalid_policy_path', 'Policy paths must be canonical absolute paths of at most 4096 bytes')
    parts = value[1:].split('/') if value != '/' else []
    if len(parts) > 128 or any(p in ('', '.', '..') or p.strip() != p for p in parts):
        raise PolicyError(400, 'invalid_policy_path', 'Policy path has invalid components or depth')
    if glob:
        for index, part in enumerate(parts):
            if part == '**' and index != len(parts) - 1:
                raise PolicyError(400, 'invalid_policy_path', '** is allowed only once at the end')
            if part not in ('*', '**') and any(c in part for c in '*?[]\\'):
                raise PolicyError(400, 'invalid_policy_path', 'Globs support only whole-component * and terminal **')
    return value


def validate_tree_selector(value):
    fields = {'kind', 'name', 'folder_id', 'path_eq', 'path_glob', 'path_prefix'}
    if not isinstance(value, dict) or set(value) - fields:
        raise PolicyError(400, 'invalid_policy_tree', 'Tree selector fields are invalid')
    if value.get('kind') is not None and value['kind'] not in ('agent', 'app', 'folder'):
        raise PolicyError(400, 'invalid_policy_tree', 'Tree kind must be agent, app or folder')
    for field in ('name', 'folder_id'):
        if value.get(field) is not None: validate_token(value[field], 'Tree selector ' + field)
    paths = [key for key in ('path_eq', 'path_glob', 'path_prefix') if value.get(key) is not None]
    if len(paths) > 1:
        raise PolicyError(400, 'invalid_policy_tree', 'Use only one path selector')
    for key in paths:
        if key == 'path_prefix':
            prefix = value[key]
            if not isinstance(prefix, dict) or set(prefix) != {'path', 'include_self'} or type(prefix['include_self']) is not bool:
                raise PolicyError(400, 'invalid_policy_tree', 'Prefix requires path and explicit boolean include_self')
            validate_path(prefix['path'])
        else: validate_path(value[key], key == 'path_glob')


def validate_expressions(rule):
    bindings, conditions = rule.get('bindings', {}), rule.get('conditions', [])
    if not isinstance(bindings, dict) or not isinstance(conditions, list) or len(bindings) > 8 or len(conditions) > 32:
        raise PolicyError(400, 'policy_expression_too_large', 'At most 8 bindings and 32 conditions per rule')
    def folder_ref(ref, binding=False):
        if not isinstance(ref, dict) or len(ref) != 1:
            raise PolicyError(400, 'invalid_policy_binding', 'Folder references require one typed field')
        key, value = next(iter(ref.items()))
        if key == 'var':
            if binding or not isinstance(value, str) or value not in bindings:
                raise PolicyError(400, 'invalid_policy_binding', 'Binding chains and undeclared variables are not allowed')
        elif key == 'ref':
            if value not in ('subject.tree.folder', 'resource.tree.folder'):
                raise PolicyError(400, 'invalid_policy_binding', 'Unknown folder field')
        elif key == 'id': validate_token(value, 'folder ID')
        elif key == 'path': validate_path(value)
        else: raise PolicyError(400, 'invalid_policy_binding', 'Unknown folder reference')
    for name, ref in bindings.items():
        validate_token(name, 'binding name')
        folder_ref(ref, True)
    for condition in conditions:
        if not isinstance(condition, dict):
            raise PolicyError(400, 'invalid_policy_condition', 'Condition must be an object')
        op = condition.get('op')
        allowed = {'op'} if op == 'same_folder' else {'op', 'strict'}
        if op == 'resource_below_folder': allowed.add('folder')
        if op not in ('same_folder', 'resource_below_subject_folder', 'resource_below_folder') or set(condition) - allowed:
            raise PolicyError(400, 'invalid_policy_condition', 'Unknown condition or fields')
        if 'strict' in condition and type(condition['strict']) is not bool:
            raise PolicyError(400, 'invalid_policy_condition', 'strict must be boolean')
        if op == 'resource_below_folder': folder_ref(condition.get('folder'))


def safety_warnings(document):
    warnings = ['Current workspace owners retain Core Policy Provider and Tree recovery operations; this does not bypass other policy checks.']
    if document.get('defaults'):
        warnings.append('Default denials may also affect your own non-recovery operations. Simulate them before enforcing.')
    for rule in document['rules']:
        if rule['effect'] == 'deny' and any(s.get('kind') in (None, 'human') for s in rule['subjects']):
            warnings.append('Rule ' + rule['id'] + ' may deny your own human operations. Review selectors and simulate before enforcing.')
    return warnings


COLLAB_ACTIONS = ['agent.discover', 'agent.metadata.read', 'agent.output.read',
                  'agent.prompt', 'agent.input', 'agent.stop', 'message.send']
READ_ACTIONS = ['agent.discover', 'agent.metadata.read', 'agent.output.read',
                'agent.queue.read', 'agent.startup.read', 'agent.tree.read',
                'launch_profile.list', 'launch_profile.read', 'human.list',
                'service.list', 'virtual_host.list', 'ingress.list']
WRITE_ACTIONS = ['agent.prompt', 'agent.queue.cancel', 'agent.input', 'agent.stop',
                 'agent.abort', 'agent.update', 'agent.delete', 'agent.create',
                 'agent.startup.manage', 'agent.tree.manage', 'tree.move', 'tree.rename',
                 'launch_profile.create', 'launch_profile.update', 'launch_profile.delete',
                 'launch_profile.use', 'machine.update', 'machine.delete', 'machine.exec',
                 'machine.file.write', 'identity.token.issue', 'service.create',
                 'service.update', 'service.delete', 'service.probe', 'virtual_host.create',
                 'virtual_host.delete', 'ingress.create', 'ingress.update', 'ingress.delete',
                 'email.send', 'message.send', 'message.import', 'app.use', 'network.connect']
TEMPLATES = [
    {'id':'team-isolation', 'name':'Team isolation',
     'description':'Same-folder collaboration and parent-folder management. Cross-folder access needs explicit rules. Scoped to listed actions; unmatched actions remain Allow.'},
    {'id':'read-only-observer', 'name':'Read-only observer',
     'description':'Allow listed reads and deny listed writes for Agent/App actors under the selected path. Future/unlisted actions and OS access are not covered.'},
]


def template_document(name, options):
    if not isinstance(options, dict) or set(options) - {'actor_path', 'target_path'}:
        raise PolicyError(400, 'invalid_template_options', 'Template options are actor_path and target_path')
    actor_path = validate_path(options.get('actor_path', '/'))
    target_path = validate_path(options.get('target_path', '/'))
    actor = {'kind':'agent', 'tree':{'path_prefix':{'path':actor_path, 'include_self':True}}}
    resources = [{'tree':{'path_prefix':{'path':target_path, 'include_self':True}}}]
    document = {'schema_version':2, 'defaults':{}, 'groups':{}, 'rules':[]}
    if name == 'team-isolation':
        document['defaults'] = {action:'deny' for action in COLLAB_ACTIONS}
        document['rules'] = [
            {'id':'team-same-folder', 'priority':100, 'effect':'allow', 'subjects':[actor],
             'actions':COLLAB_ACTIONS[:], 'resources':resources, 'conditions':[{'op':'same_folder'}]},
            {'id':'team-parent-management', 'priority':90, 'effect':'allow', 'subjects':[actor],
             'actions':COLLAB_ACTIONS[:], 'resources':resources,
             'conditions':[{'op':'resource_below_subject_folder', 'strict':True}]},
        ]
    elif name == 'read-only-observer':
        document['rules'] = [
            {'id':'observer-read', 'priority':100, 'effect':'allow', 'subjects':[actor],
             'actions':READ_ACTIONS[:], 'resources':resources},
            {'id':'observer-deny-write', 'priority':200, 'effect':'deny', 'subjects':[actor],
             'actions':WRITE_ACTIONS[:], 'resources':[{}]},
        ]
    else: raise PolicyError(404, 'template_not_found', 'Unknown template')
    return {'document':validate_document(document), 'mode':'monitor',
            'warnings':['Template replaces the draft only. Review explicit human grants and action coverage before enforcement. Owner Policy/Tree rescue is provided by Core, not these rules.']}

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
            if value.get("mode") not in ("monitor", "enforce") or type(value.get("revision")) is not int or value["revision"] < 1:
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
                if item.get("mode") not in ("monitor", "enforce"):
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
            if body["mode"] not in ("monitor", "enforce"):
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


class AuthError(PolicyError):
    pass


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


class Auth:
    def __init__(self, store, proxy_url, service_id, workspace_id, public_url):
        self.store = store
        self.proxy_url = proxy_url.rstrip('/') + '/'
        self.service_id = service_id
        self.workspace_id = workspace_id
        self.public_url = public_url.rstrip('/') + '/'

    def request(self, method, path, token=None, body=None, form=None, extra_headers=None):
        headers = {'Accept': 'application/json', **(extra_headers or {})}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers['Content-Type'] = 'application/json'
        if form is not None:
            data = urllib.parse.urlencode(form).encode()
            headers['Content-Type'] = 'application/x-www-form-urlencoded'
        request = urllib.request.Request(urllib.parse.urljoin(self.proxy_url, path.lstrip('/')),
                                         data=data, headers=headers, method=method)
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect()).open(request, timeout=10) as response:
                result = json.loads(response.read(2 * 1024 * 1024))
                if not isinstance(result, dict):
                    raise ValueError()
                return result
        except urllib.error.HTTPError as exc:
            try:
                code = json.loads(exc.read(65536))['error']['code']
            except (ValueError, KeyError, TypeError):
                code = 'treer_request_failed'
            raise AuthError(exc.code if exc.code in (400, 401, 403, 404) else 502,
                        code, 'Treer rejected the request') from exc
        except (OSError, ValueError) as exc:
            raise AuthError(502, 'treer_unavailable', 'Treer API unavailable') from exc

    def diagnostic(self, operation, token, body=None):
        if operation not in ('validate', 'explain', 'decisions'):
            raise AuthError(400, 'invalid_diagnostic', 'Unknown diagnostic operation')
        runtime = os.environ.get('TREER_AGENT_ID')
        credential = os.environ.get('TREER_WORKLOAD_CREDENTIAL')
        if not runtime or not credential:
            raise AuthError(503, 'managed_runtime_required', 'Core diagnostics require the configured Managed Policy App runtime')
        # This restricted bridge requires BOTH current owner and current bound
        # Provider runtime. The runtime credential never substitutes for a human.
        return self.request('GET' if operation == 'decisions' else 'POST',
            f'/api/apps/{self.service_id}/policy/{operation}', body=body,
            extra_headers={'X-Treer-Identity-Token':token, 'X-Treer-Agent-Id':runtime,
                           'X-Treer-Workload-Credential':credential})

    def verify(self, token):
        if not token or len(token) > 8192:
            raise AuthError(401, 'authentication_required', 'Sign in with Treer or use a service token')
        result = self.request('POST', '/.treer/apps/identity/verify',
                              body={'token': token, 'audience': self.service_id})
        claims = result.get('claims') or {}
        if (result.get('active') is not True or claims.get('service_id') != self.service_id
                or claims.get('workspace_id') != self.workspace_id
                or claims.get('principal_kind') != 'human' or not claims.get('sub')
                or not isinstance(claims.get('exp'), (int, float)) or claims['exp'] <= time.time()):
            raise AuthError(401, 'identity_invalid', 'Identity is inactive or outside this App workspace')
        # Require current workspace directory membership in addition to verified claims.
        directory = self.directory(token)
        principal = next((p for p in directory if p.get('id') == claims['sub']
                          and p.get('kind') == claims['principal_kind']), None)
        if principal is None:
            raise AuthError(403, 'identity_unavailable', 'Caller is absent from the workspace directory')
        return principal, directory

    def authenticate(self, headers, raw_cookie):
        # Header presence, including an empty/invalid value, is authoritative:
        # never silently fall back to a different human session after rejection.
        cookie_auth = False
        if 'X-Treer-Identity-Token' in headers:
            token = headers.get('X-Treer-Identity-Token')
            if not token:
                raise AuthError(401, 'identity_invalid', 'Injected identity token is empty')
        elif 'Authorization' in headers:
            value = headers.get('Authorization', '')
            token = value[7:] if value.startswith('Bearer ') else None
        else:
            cookie_auth = True
            token = self.session_token(raw_cookie)
        principal, directory = self.verify(token)
        return token, principal, directory, cookie_auth

    def check_browser_mutation(self, headers, principal, cookie_auth):
        # Injected human headers originated in an ambient browser session too.
        # Their presence must not skip CSRF protection as Agent bearer calls do.
        site = headers.get('Sec-Fetch-Site')
        if site is not None and site not in ('same-origin', 'same-site'):
            raise AuthError(403, 'csrf_rejected', 'Cross-site browser mutations rejected')
        origin = headers.get('Origin')
        if origin is not None:
            allowed = {self.origin(self.public_url), self.origin(self.proxy_url)}
            if origin not in allowed:
                raise AuthError(403, 'csrf_rejected', 'Browser origin is not this App or its Treer tunnel')
        if (cookie_auth or principal['kind'] == 'human') and headers.get('X-Policy-Request') != '1':
            raise AuthError(403, 'csrf_rejected', 'Human mutations require X-Policy-Request: 1')

    @staticmethod
    def origin(value):
        url = urllib.parse.urlsplit(value)
        return f'{url.scheme}://{url.netloc}'

    def directory(self, token):
        return self.request('GET', f'/api/apps/{self.service_id}/directory', token)['principals']

    def start(self):
        state, verifier, browser = secrets.token_urlsafe(32), secrets.token_urlsafe(48), secrets.token_urlsafe(32)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
        with self.store.connect() as db:
            db.execute('DELETE FROM oauth WHERE expires <= ?', (time.time(),))
            db.execute('INSERT INTO oauth(state,verifier,expires,browser) VALUES(?,?,?,?)',
                       (digest(state), verifier, time.time() + 600, digest(browser)))
        query = urllib.parse.urlencode({'response_type': 'code', 'client_id': self.service_id,
                 'redirect_uri': self.public_url + 'api/auth/callback', 'state': state,
                 'code_challenge': challenge, 'code_challenge_method': 'S256'})
        return self.proxy_url + 'api/apps/oauth/authorize?' + query, browser

    def finish(self, code, state, browser):
        if not browser or len(browser) > 256 or not state or len(state) > 256:
            raise AuthError(401, 'oauth_state_invalid', 'OAuth browser correlation is missing or invalid')
        with self.store.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT * FROM oauth WHERE state=? AND browser=?',
                             (digest(state), digest(browser))).fetchone()
            expired = bool(row and row['expires'] <= time.time())
            if row:
                db.execute('DELETE FROM oauth WHERE state=? AND browser=?', (digest(state), digest(browser)))
            db.execute('DELETE FROM oauth WHERE expires<=?', (time.time(),))
        if not row or expired:
            error = AuthError(401, 'oauth_state_invalid', 'OAuth state invalid or expired')
            # A mismatch may be another valid flow in this browser. Preserve its
            # cookie as well as its DB state; clear only a matched expired flow.
            error.clear_oauth_cookie = expired
            raise error
        try:
            result = self.request('POST', '/api/apps/oauth/token', form={
                'grant_type': 'authorization_code', 'code': code, 'client_id': self.service_id,
                'redirect_uri': self.public_url + 'api/auth/callback', 'code_verifier': row['verifier']})
            token = result['access_token']
            principal, _ = self.verify(token)
            if principal['kind'] != 'human':
                raise AuthError(401, 'oauth_identity_invalid', 'Human OAuth identity required')
            expires = datetime.fromisoformat(result['expires_at'].replace('Z', '+00:00')).timestamp()
            raw = secrets.token_urlsafe(48)
            with self.store.connect() as db:
                db.execute('DELETE FROM sessions WHERE expires<=?', (time.time(),))
                db.execute('INSERT INTO sessions VALUES(?,?,?)', (digest(raw), token, expires))
            return raw, max(0, int(expires - time.time()))
        except AuthError as error:
            error.clear_oauth_cookie = True
            raise

    def session_token(self, raw):
        with self.store.connect() as db:
            row = db.execute('SELECT token FROM sessions WHERE secret=? AND expires>?',
                             (digest(raw), time.time())).fetchone()
        return row['token'] if row else None

    def logout(self, raw):
        with self.store.connect() as db:
            db.execute('DELETE FROM sessions WHERE secret=?', (digest(raw),))


class AuthStore:
    def __init__(self, path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        with self.connect() as db:
            db.executescript('CREATE TABLE IF NOT EXISTS oauth(state TEXT PRIMARY KEY,verifier TEXT,expires REAL,browser TEXT);'
                             'CREATE TABLE IF NOT EXISTS sessions(secret TEXT PRIMARY KEY,token TEXT,expires REAL);')
        path.chmod(0o600)

    def connect(self):
        db = sqlite3.connect(self.path, timeout=10)
        db.row_factory = sqlite3.Row
        return db


class PolicyServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, handler):
        super().__init__(address, handler)
        self.auth = None
        self.auth_lock = threading.Lock()

    def identity_provider(self):
        with self.auth_lock:
            if self.auth is None:
                proxy = (os.environ.get('POLICY_PROXY_URL') or os.environ.get('TREER_PROXY_URL', '')).rstrip('/')
                service = os.environ.get('POLICY_SERVICE_ID')
                public = os.environ.get('POLICY_PUBLIC_URL')
                workspace = self.RequestHandlerClass.store.workspace_id
                if not proxy:
                    raise PolicyError(503, 'identity_not_configured', 'Configure POLICY_PROXY_URL for Treer identity/OAuth')
                if not service or not public:
                    try:
                        result = subprocess.run([os.environ.get('TREER_BIN', 'treer'), 'app', 'list'],
                                                capture_output=True, text=True, timeout=10, check=True)
                        app = next(a for a in json.loads(result.stdout)['apps']
                                   if a['runtime_agent_id'] == os.environ.get('TREER_AGENT_ID'))
                        if app['workspace_id'] != workspace:
                            raise ValueError('Wrong workspace')
                        service = app['service_id']
                        public = public or app.get('public_url') or (proxy +
                            f"/api/workspaces/{workspace}/apps/{app['app_id']}/ui/")
                    except (OSError, ValueError, KeyError, TypeError, StopIteration, subprocess.SubprocessError):
                        raise PolicyError(503, 'identity_not_configured', 'Configure this Managed App service ID and public URL')
                for url in (proxy, public):
                    parsed = urlsplit(url)
                    if parsed.scheme not in ('http', 'https') or not parsed.netloc or parsed.username or parsed.password or parsed.query or parsed.fragment:
                        raise PolicyError(503, 'identity_not_configured', 'App identity URLs must be absolute HTTP(S) origins without credentials')
                auth_path = self.RequestHandlerClass.store.path.with_suffix('.auth.sqlite3')
                self.auth = Auth(AuthStore(auth_path), proxy, service, workspace, public)
            return self.auth

class PolicyHandler(BaseHTTPRequestHandler):
    store: PolicyStore
    server_version = "TreerPolicy/1"

    def do_GET(self) -> None:
        self.response_cookies = []
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
                self._json(200, {"protocol": PROTOCOL, "provider_name": "Treer Policy", "capabilities": [CAPABILITY, "policy.bundle.v2"]})
            elif parsed.path == "/v1/policy/bundle":
                workspace_id = parse_qs(parsed.query).get("workspace_id", [""])[0]
                if workspace_id != self.store.workspace_id:
                    raise PolicyError(404, "workspace_not_found", "Policy App does not serve this workspace")
                max_schema = parse_qs(parsed.query).get("max_schema_version", ["1"])[0]
                if self.store.bundle()["document"]["schema_version"] == 2 and max_schema != "2":
                    raise PolicyError(409, "unsupported_policy_schema", "This bundle requires a schema v2 capable Proxy")
                self._json(200, self.store.bundle())
            elif parsed.path == "/v1/session":
                if 'X-Treer-Identity-Token' in self.headers or self._raw_cookie('policy_session') or ('Authorization' in self.headers and self.headers.get('Authorization') != 'Bearer ' + str(self._operator_token())):
                    token, actor, _, _ = self._identity()
                    self._json(200, {'principal':actor, 'oauth_available':True})
                else:
                    self._json(200, {'principal':None, 'oauth_available':bool(os.environ.get('POLICY_PROXY_URL') or os.environ.get('TREER_PROXY_URL'))})
            elif parsed.path == "/api/auth/start":
                auth = self._auth()
                url, browser = auth.start()
                self._cookie('policy_oauth', browser, 600)
                self._redirect(url)
            elif parsed.path == "/api/auth/callback":
                auth = self._auth()
                query = parse_qs(parsed.query)
                raw, age = auth.finish(query.get('code',[''])[0], query.get('state',[''])[0], self._raw_cookie('policy_oauth'))
                self._cookie('policy_session', raw, age)
                self._cookie('policy_oauth', '', 0)
                self._redirect(auth.public_url)
            elif parsed.path == "/v1/policy/decisions":
                token, _, _, _ = self._identity(mutation=True)
                self._json(200, self._auth().diagnostic('decisions', token))
            elif parsed.path == "/v1/templates":
                self._json(200, {"templates":TEMPLATES})
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
        self.response_cookies = []
        try:
            if urlsplit(self.path).path == "/api/auth/logout":
                token, actor, _, cookie_auth = self._identity(mutation=True)
                self._auth().logout(self._raw_cookie("policy_session"))
                self._cookie("policy_session", "", 0)
                self._json(200, {"signed_out":True})
                return
            if urlsplit(self.path).path in ("/v1/policy/publish", "/v1/policy/invalidate", "/v1/policy/rollback"):
                self._require_operator()
                if self.headers.get("Sec-Fetch-Site") not in (None, "same-origin"):
                    raise PolicyError(403, "csrf_denied", "Cross-origin policy writes rejected")
                if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                    raise PolicyError(415, "invalid_content_type", "Policy writes require application/json")
            body = self._read_json()
            if urlsplit(self.path).path == "/v1/policy/validate":
                self._require_operator()
                document = validate_document(body.get('document') if isinstance(body, dict) else None)
                warnings = safety_warnings(document)
                result = {'valid':True, 'warnings':warnings, 'core_validated':False}
                if document['schema_version'] == 2:
                    token, _, _, _ = self._identity(mutation=True)
                    result['core'] = self._core_validate(token, document)
                    result['core_validated'] = True
                self._json(200, result)
            elif urlsplit(self.path).path == "/v1/policy/explain":
                token, _, _, _ = self._identity(mutation=True)
                self._json(200, self._auth().diagnostic('explain', token, body))
            elif urlsplit(self.path).path == "/v1/policy/publish":
                self._validate_publish(body)
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
                snapshot = self.store.revision(body["revision"])
                self._validate_publish(snapshot)
                bundle = self.store.rollback(body["expected_revision"], body["revision"])
                self._json(200, {"bundle": bundle, "proxy_sync": self.store.notify_proxy()})
            elif urlsplit(self.path).path.startswith("/v1/templates/"):
                self._json(200, template_document(urlsplit(self.path).path.rsplit("/", 1)[1], body))
            elif urlsplit(self.path).path == "/v1/policy/test":
                self._json(200, evaluate_request(body, self.store.bundle()))
            else:
                raise PolicyError(404, "not_found", "route not found")
        except PolicyError as error:
            self._error(error)

    def _validate_publish(self, body):
        if not isinstance(body, dict):
            raise PolicyError(400, 'invalid_publication', 'Publication requires an object')
        document = validate_document(body.get('document'))
        # Schema 1 remains compatible with existing operator-only automation.
        # Path policy publication requires the owner-authorized Core validator;
        # an unavailable/old Core cannot silently accept an unenforceable draft.
        if document['schema_version'] == 2:
            token, _, _, _ = self._identity(mutation=True)
            self._core_validate(token, document)

    def _core_validate(self, token, document):
        result = self._auth().diagnostic('validate', token, document)
        if result.get('valid') is not True or result.get('recovery_guaranteed') is not True or 'policy.path-rules.v2' not in result.get('capabilities', []):
            raise PolicyError(409, 'policy_recovery_unconfirmed', 'Core must confirm path-policy support and owner recovery before publishing')
        return result

    def _auth(self):
        if getattr(self.server, 'auth', None) is not None:
            return self.server.auth
        provider = getattr(self.server, 'identity_provider', None)
        if provider is None:
            raise PolicyError(503, 'identity_not_configured', 'Treer identity is not configured')
        return provider()

    def _raw_cookie(self, name):
        cookie = SimpleCookie()
        try: cookie.load(self.headers.get('Cookie', ''))
        except CookieError: return ''
        return cookie[name].value if name in cookie else ''

    def _operator_bearer(self):
        value = self.headers.get('Authorization', '')
        token = self._operator_token()
        if token and hmac.compare_digest(value.encode(), ('Bearer ' + token).encode()):
            return True
        return False

    def _identity(self, mutation=False):
        auth = self._auth()
        # Operator credential belongs to the App, not to Treer. Never pass it to
        # Core verification/diagnostics as a caller token.
        headers = copy.copy(self.headers)
        file_capability = getattr(self.server, 'admin_token_file', None) is not None
        browser_identity = 'X-Treer-Identity-Token' in headers or self._raw_cookie('policy_session')
        env_capability = getattr(self.server, 'admin_token', None)
        if getattr(self.server, 'admin_token_file', None) is not None or headers.get('Authorization') == 'Bearer ' + str(self._operator_token()):
            # Includes stale/rotated capabilities. In file mode, human callers
            # use injected identity or OAuth cookies, never this Bearer channel.
            del headers['Authorization']
        token, actor, directory, cookie_auth = auth.authenticate(headers, self._raw_cookie('policy_session'))
        if mutation: auth.check_browser_mutation(self.headers, actor, cookie_auth)
        return token, actor, directory, cookie_auth

    def _cookie(self, name, value, age):
        auth = self._auth()
        cookie = SimpleCookie()
        cookie[name] = value
        cookie[name]['httponly'] = True
        cookie[name]['samesite'] = 'Lax'
        cookie[name]['path'] = urlsplit(auth.public_url).path or '/'
        cookie[name]['max-age'] = age
        if urlsplit(auth.public_url).scheme == 'https': cookie[name]['secure'] = True
        self.response_cookies.append(cookie[name].OutputString())

    def _redirect(self, location):
        self.send_response(302)
        self.send_header('Location', location)
        self.send_header('Cache-Control', 'no-store')
        for value in self.response_cookies: self.send_header('Set-Cookie', value)
        self.send_header('Content-Length', '0')
        self.end_headers()

    def _operator_token(self):
        source = getattr(self.server, 'admin_token_file', None)
        if source is None: return getattr(self.server, 'admin_token', None)
        try: return source.read()
        except (OSError, ValueError): return None

    def _require_operator(self) -> None:
        if 'X-Treer-Identity-Token' in self.headers or self._raw_cookie('policy_session'):
            self._identity(mutation=self.command != 'GET')
        token = self._operator_token()
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
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
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
        if getattr(error, "clear_oauth_cookie", False):
            self._cookie("policy_oauth", "", 0)
        self._json(error.status, {"error": {"code": error.code, "message": error.message}})

    def _send_bytes(self, status: int, content_type: str, body: bytes, vary: bool = False) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'self'")
        for value in getattr(self, "response_cookies", []):
            self.send_header("Set-Cookie", value)
        if vary:
            self.send_header("Vary", "Accept, User-Agent")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, pattern: str, *args: object) -> None:
        # OAuth callback query strings may contain codes/state. Never log URLs.
        print("policy request completed")


def evaluate_request(body: object, bundle: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(body, dict) or set(body) != {"subject", "action", "resource"}:
        raise PolicyError(400, "invalid_test_request", "test requires subject, action, and resource")
    subject, action, resource = body["subject"], body["action"], body["resource"]
    if not isinstance(subject, dict) or not isinstance(resource, dict):
        raise PolicyError(400, "invalid_test_request", "subject and resource must be objects")
    for item, allowed in ((subject, {'kind','id','machine_id'}), (resource, {'kind','id','principal'})):
        if set(item) - allowed:
            raise PolicyError(400, 'invalid_test_request', 'Unknown dry-run fields')
        for key in ('kind','id','machine_id'):
            if item.get(key) is not None: validate_token(item[key], key)
    principal = resource.get('principal', {})
    if not isinstance(principal, dict) or set(principal) - {'kind','id'}:
        raise PolicyError(400, 'invalid_test_request', 'Invalid dry-run resource principal')
    for key in principal: validate_token(principal[key], key)
    validate_token(action, "action")
    document = bundle["document"]
    if document["schema_version"] != 1:
        raise PolicyError(503, "core_explain_required", "Path policies require the Core explain API")
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


class AdminTokenFile:
    """Machine-local capability. No secret appears in errors, URLs or output."""
    def __init__(self, path):
        self.path = Path(os.path.abspath(path))
        self.marker = self.path.name + '.initialized'

    def parent_fd(self):
        # Pin each directory and reject symlink ancestors as well as the leaf.
        fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
        try:
            for part in self.path.parent.parts[1:]:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = child
            return fd
        except BaseException:
            os.close(fd)
            raise

    @staticmethod
    def check_file(fd):
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o600:
            raise ValueError('Admin token file must be a private regular file owned by this machine account')

    def read(self):
        parent = self.parent_fd()
        try:
            fd = os.open(self.path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
            try:
                self.check_file(fd)
                raw = os.read(fd, 8193)
                if len(raw) > 8192:
                    raise ValueError('Invalid admin token file')
                token = raw.decode('ascii').removesuffix('\n')
                validate_admin_token(token)
                return token
            finally:
                os.close(fd)
        finally:
            os.close(parent)

    def write(self, parent, *, replace):
        name = '.policy-admin-' + secrets.token_hex(16)
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, 'wb') as handle:
                handle.write((secrets.token_urlsafe(32) + '\n').encode('ascii'))
                handle.flush()
                os.fsync(handle.fileno())
            if replace:
                os.replace(name, self.path.name, src_dir_fd=parent, dst_dir_fd=parent)
            else:
                # Publish a complete file exclusively; concurrent readers never
                # see a partially written capability.
                os.link(name, self.path.name, src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
                os.unlink(name, dir_fd=parent)
            os.fsync(parent)
        finally:
            try: os.unlink(name, dir_fd=parent)
            except FileNotFoundError: pass

    def initialize(self):
        try:
            self.read()
        except FileNotFoundError:
            pass
        else:
            self.record_initialized()
            return
        parent = self.parent_fd()
        try:
            try:
                fd = os.open(self.marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
            except FileExistsError:
                # Startup flag persists across restarts: loss must lock edits,
                # not silently provision a new capability.
                return
            with os.fdopen(fd, 'wb') as handle:
                os.fchmod(handle.fileno(), 0o600)
                handle.write(b'initialized\n');handle.flush();os.fsync(handle.fileno())
            os.fsync(parent)
            try: self.write(parent, replace=False)
            except FileExistsError: self.read()
        finally:
            os.close(parent)

    def record_initialized(self):
        parent = self.parent_fd()
        try:
            try: fd = os.open(self.marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
            except FileExistsError: return
            with os.fdopen(fd, 'wb') as handle:
                os.fchmod(handle.fileno(), 0o600)
                handle.write(b'initialized\n');handle.flush();os.fsync(handle.fileno())
            os.fsync(parent)
        finally: os.close(parent)

    def rotate(self):
        # Validate an existing leaf; refuse to overwrite symlinks or loose files.
        try: self.read()
        except FileNotFoundError: pass
        self.record_initialized()
        parent = self.parent_fd()
        try: self.write(parent, replace=True)
        finally: os.close(parent)


def validate_admin_token(token):
    if not isinstance(token, str) or not 32 <= len(token) <= 8192 or any(not c.isascii() or c.isspace() or not c.isprintable() for c in token):
        raise ValueError('Admin token must be 32–8192 printable non-whitespace ASCII characters')
    return token


def admin_token_source(*, initialize=False):
    token = os.environ.get('POLICY_ADMIN_TOKEN')
    if token:
        return validate_admin_token(token), None
    configured = os.environ.get('POLICY_ADMIN_TOKEN_FILE')
    if not configured and not BUNDLED_INSTALL and not initialize:
        return None, None
    source = AdminTokenFile(configured or APP_ROOT / '.treer-policy-admin-token')
    if initialize:
        try: source.initialize()
        except (OSError, ValueError):
            # Provisioning failure must not take bundle reads/recovery offline.
            # Every administration request still fails closed on this file.
            pass
    # Fail closed on unreadable/lost files, but keep bundle reads available.
    return None, source


def main() -> None:
    parser = argparse.ArgumentParser(description="Treer Policy Provider")
    parser.add_argument("--port", type=int, default=None)
    parser.add_argument('--initialize-admin-token-file', action='store_true', help='Provision a private token once on the machine; never print it')
    parser.add_argument('--rotate-admin-token-file', action='store_true', help='Locally rotate or recover the private token file, then exit without serving')
    args = parser.parse_args()
    if args.rotate_admin_token_file:
        if args.initialize_admin_token_file or os.environ.get('POLICY_ADMIN_TOKEN'):
            raise SystemExit('File rotation cannot be combined with initialization or an environment token')
        try:
            AdminTokenFile(os.environ.get('POLICY_ADMIN_TOKEN_FILE') or APP_ROOT / '.treer-policy-admin-token').rotate()
        except (OSError, ValueError):
            raise SystemExit('Admin token file rotation failed; check the private file and parent directory')
        print('Admin token file replaced locally; no token is returned')
        return
    try:
        admin_token, admin_token_file = admin_token_source(initialize=args.initialize_admin_token_file)
    except (OSError, ValueError):
        raise SystemExit('Admin token configuration invalid; check the private file and parent directory')
    workspace_id = os.environ.get("TREER_WORKSPACE_ID", "").strip()
    if not workspace_id:
        raise SystemExit("TREER_WORKSPACE_ID is required")
    listen = os.environ.get(
        "POLICY_LISTEN", f"127.0.0.1:{args.port}" if args.port is not None else "127.0.0.1:8787"
    )
    host, raw_port = listen.rsplit(":", 1)
    default_data_file = (
        APP_ROOT / ".treer-policy-data.json"
        if BUNDLED_INSTALL
        else APP_ROOT / "data" / "policy.json"
    )
    data_file = Path(os.environ.get("POLICY_DATA_FILE", str(default_data_file))).resolve()
    PolicyHandler.store = PolicyStore(data_file, workspace_id)
    server = PolicyServer((host, int(raw_port)), PolicyHandler)
    server.admin_token = admin_token
    server.admin_token_file = admin_token_file
    print(f"Treer Policy Provider listening on {listen} for {workspace_id}")
    server.serve_forever()


if __name__ == "__main__":
    main()
