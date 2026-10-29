"""Gateway rules: configuration, path patterns, pkt-line push commands and API rules."""

from __future__ import annotations

import re
import zlib
from dataclasses import dataclass
from typing import Any

ZERO_IDS = {"0" * 40, "0" * 64}
OWNER = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})")
REPO = re.compile(r"[A-Za-z0-9._-]{1,100}")
AGENT_ID = re.compile(r"[A-Za-z0-9_.:-]{1,128}")
API_METHODS = ("GET", "HEAD", "POST", "PATCH", "PUT", "DELETE")
CREDENTIAL_MODES = ("auto", "gh", "github_app")

DEFAULT_CONFIG: dict[str, Any] = {
    "repos": ["EvoEvolver/treer"],
    "agent_push_refs": ["refs/heads/agent/**"],
    "protected_refs": ["refs/heads/main", "refs/heads/master", "refs/heads/release/**", "refs/tags/**"],
    "trusted_agents": [],
    "credential_mode": "auto",
    "max_push_bytes": 1024 * 1024 * 1024,
    "api_global_reads": ["/rate_limit", "/user", "/search/*"],
    "agent_api_writes": [
        "POST /repos/{repo}/issues",
        "PATCH /repos/{repo}/issues/*",
        "POST /repos/{repo}/issues/*/comments",
        "POST /repos/{repo}/issues/*/labels",
        "PATCH /repos/{repo}/issues/comments/*",
        "POST /repos/{repo}/pulls",
        "PATCH /repos/{repo}/pulls/*",
        "POST /repos/{repo}/pulls/*/comments",
        "POST /repos/{repo}/pulls/*/reviews",
        "POST /repos/{repo}/pulls/*/requested_reviewers",
        "POST /repos/{repo}/actions/runs/*/rerun",
        "POST /repos/{repo}/actions/runs/*/rerun-failed-jobs",
        "DELETE /repos/{repo}/git/refs/heads/agent/**",
    ],
}


class RuleError(ValueError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


# --------------------------------------------------------------------------- patterns


def _pattern_regex(pattern: str) -> re.Pattern[str]:
    out = []
    index = 0
    while index < len(pattern):
        if pattern.startswith("**", index):
            out.append(".+")
            index += 2
        elif pattern[index] == "*":
            out.append("[^/]+")
            index += 1
        else:
            out.append(re.escape(pattern[index]))
            index += 1
    return re.compile("".join(out))


def match(pattern: str, value: str) -> bool:
    """``*`` matches one non-empty path segment part, ``**`` one or more characters across segments."""
    return _pattern_regex(pattern).fullmatch(value) is not None


def match_any(patterns: list[str], value: str) -> bool:
    return any(match(pattern, value) for pattern in patterns)


# --------------------------------------------------------------------------- configuration


def _string_list(value: Any, key: str, check: re.Pattern[str] | None = None, limit: int = 200) -> list[str]:
    if not isinstance(value, list) or len(value) > limit:
        raise RuleError("invalid_config", f"{key} must be a list of at most {limit} strings")
    result = []
    for item in value:
        if not isinstance(item, str) or not item or len(item) > 300 or any(c in item for c in "\0\r\n\t "):
            raise RuleError("invalid_config", f"{key} entries must be non-empty strings without whitespace")
        if check is not None and not check.fullmatch(item):
            raise RuleError("invalid_config", f"{key} entry is invalid: {item}")
        if item not in result:
            result.append(item)
    return result


REPO_PATTERN = re.compile(r"[A-Za-z0-9*][A-Za-z0-9*-]{0,38}/[A-Za-z0-9._*-]{1,100}")
REF_PATTERN = re.compile(r"refs/[A-Za-z0-9._/*+@-]{1,250}")
API_PATH_PATTERN = re.compile(r"/[A-Za-z0-9._/*{}+@-]{0,250}")


def validate_config(value: Any, base: dict[str, Any] | None = None) -> dict[str, Any]:
    """Merge a (partial) config into ``base`` and validate every field."""
    if not isinstance(value, dict):
        raise RuleError("invalid_config", "config must be a JSON object")
    unknown = set(value) - set(DEFAULT_CONFIG)
    if unknown:
        raise RuleError("unknown_field", f"unknown config field: {sorted(unknown)[0]}")
    config = {**(base or DEFAULT_CONFIG), **value}
    config["repos"] = _string_list(config["repos"], "repos", REPO_PATTERN)
    config["agent_push_refs"] = _string_list(config["agent_push_refs"], "agent_push_refs", REF_PATTERN)
    config["protected_refs"] = _string_list(config["protected_refs"], "protected_refs", REF_PATTERN)
    config["trusted_agents"] = _string_list(config["trusted_agents"], "trusted_agents", AGENT_ID)
    config["api_global_reads"] = _string_list(config["api_global_reads"], "api_global_reads", API_PATH_PATTERN)
    writes = _string_list_spaces(config["agent_api_writes"])
    config["agent_api_writes"] = writes
    if config["credential_mode"] not in CREDENTIAL_MODES:
        raise RuleError("invalid_config", f"credential_mode must be one of {', '.join(CREDENTIAL_MODES)}")
    size = config["max_push_bytes"]
    if not isinstance(size, int) or isinstance(size, bool) or not 1024 <= size <= 8 * 1024**3:
        raise RuleError("invalid_config", "max_push_bytes must be an integer between 1024 and 8 GiB")
    return config


def _string_list_spaces(value: Any) -> list[str]:
    if not isinstance(value, list) or len(value) > 200:
        raise RuleError("invalid_config", "agent_api_writes must be a list of at most 200 strings")
    result = []
    for item in value:
        parts = item.split(" ") if isinstance(item, str) else []
        if (
            len(parts) != 2
            or parts[0] not in API_METHODS[2:]
            or not API_PATH_PATTERN.fullmatch(parts[1])
        ):
            raise RuleError("invalid_config", "agent_api_writes entries must be 'METHOD /path/pattern'")
        if item not in result:
            result.append(item)
    return result


# --------------------------------------------------------------------------- callers


@dataclass(frozen=True)
class Caller:
    kind: str  # "human" | "agent"
    id: str
    name: str
    machine_id: str | None = None
    role: str | None = None
    path: str | None = None

    def privileged(self, config: dict[str, Any]) -> bool:
        """Humans and listed Agents may push any ref, call any API route and edit rules."""
        return self.kind == "human" or self.id in config["trusted_agents"]

    def audit(self) -> dict[str, Any]:
        return {"kind": self.kind, "id": self.id, "name": self.name, "machine_id": self.machine_id, "path": self.path}


def repo_allowed(config: dict[str, Any], owner: str, repo: str) -> bool:
    name = f"{owner}/{repo}".lower()
    return any(match(pattern.lower(), name) for pattern in config["repos"])


# --------------------------------------------------------------------------- pkt-line


class NeedMore(Exception):
    pass


def read_pkt_lines(data: bytes, limit: int | None = None) -> tuple[list[bytes | None], int]:
    """Parse pkt-lines up to and including the first flush; ``None`` is a flush/delim packet."""
    packets: list[bytes | None] = []
    offset = 0
    while True:
        if offset + 4 > len(data):
            raise NeedMore()
        try:
            length = int(data[offset : offset + 4], 16)
        except ValueError as error:
            raise RuleError("invalid_pkt_line", "request is not a pkt-line stream") from error
        if length in (0, 1, 2):
            packets.append(None)
            offset += 4
            if length == 0:
                return packets, offset
            continue
        if length < 4:
            raise RuleError("invalid_pkt_line", "invalid pkt-line length")
        if offset + length > len(data):
            raise NeedMore()
        packets.append(data[offset + 4 : offset + length])
        offset += length
        if limit is not None and len(packets) > limit:
            raise RuleError("too_many_commands", "push contains too many ref updates")


@dataclass(frozen=True)
class RefUpdate:
    old: str
    new: str
    ref: str

    @property
    def delete(self) -> bool:
        return self.new in ZERO_IDS

    @property
    def create(self) -> bool:
        return self.old in ZERO_IDS

    def audit(self) -> dict[str, str]:
        return {"ref": self.ref, "old": self.old, "new": self.new}


def parse_receive_pack_commands(plain: bytes) -> tuple[list[RefUpdate], set[str]]:
    """Parse the command list at the start of a receive-pack request body."""
    packets, _ = read_pkt_lines(plain, limit=10000)
    updates: list[RefUpdate] = []
    capabilities: set[str] = set()
    for packet in packets:
        if packet is None:
            break
        line = packet.rstrip(b"\n")
        if line.startswith(b"shallow "):
            continue
        if line.startswith(b"push-cert"):
            raise RuleError("push_cert_unsupported", "signed pushes are not supported by the gateway")
        if not updates and b"\0" in line:
            line, _, raw_capabilities = line.partition(b"\0")
            capabilities = set(raw_capabilities.decode("ascii", "replace").split())
        fields = line.decode("utf-8", "replace").split(" ")
        if len(fields) != 3 or not all(re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", item) for item in fields[:2]):
            raise RuleError("invalid_push_command", "malformed receive-pack command")
        if not fields[2].startswith("refs/") or ".." in fields[2]:
            raise RuleError("invalid_push_command", "push target is not a full ref name")
        updates.append(RefUpdate(fields[0], fields[1], fields[2]))
    if not updates and packets[0] is not None:
        raise RuleError("invalid_push_command", "push contains no ref updates")
    # A lone flush is git's auth probe before a large (chunked) push: no ref changes.
    return updates, capabilities


class PushHead:
    """Incrementally decode a (possibly gzip-encoded) receive-pack body until its command list ends."""

    MAX_HEAD = 4 * 1024 * 1024

    def __init__(self, gzip_encoded: bool) -> None:
        self.decoder = zlib.decompressobj(16 + zlib.MAX_WBITS) if gzip_encoded else None
        self.plain = b""

    def feed(self, chunk: bytes) -> tuple[list[RefUpdate], set[str]] | None:
        if self.decoder is not None:
            self.plain += self.decoder.decompress(chunk, self.MAX_HEAD)
        else:
            self.plain += chunk
        try:
            return parse_receive_pack_commands(self.plain)
        except NeedMore:
            if len(self.plain) > self.MAX_HEAD:
                raise RuleError("push_head_too_large", "receive-pack command list is too large") from None
            return None


def push_decisions(config: dict[str, Any], caller: Caller, updates: list[RefUpdate]) -> list[str | None]:
    """Return a denial reason (or ``None``) for every update."""
    reasons: list[str | None] = []
    for update in updates:
        if update.delete and match_any(config["protected_refs"], update.ref):
            reasons.append("gateway: deleting a protected ref is not allowed")
        elif caller.privileged(config):
            reasons.append(None)
        elif match_any(config["agent_push_refs"], update.ref):
            reasons.append(None)
        else:
            allowed = ", ".join(config["agent_push_refs"]) or "nothing"
            reasons.append(f"gateway: Agents may push only {allowed}")
    return reasons


def pkt(data: bytes) -> bytes:
    return f"{len(data) + 4:04x}".encode() + data


def receive_pack_rejection(updates: list[RefUpdate], reasons: list[str | None], capabilities: set[str]) -> bytes:
    """A receive-pack result that makes ``git push`` print ``! [remote rejected] ... (reason)``."""
    report = pkt(b"unpack ok\n")
    for update, reason in zip(updates, reasons):
        text = reason or "gateway: another ref in this push was rejected"
        report += pkt(f"ng {update.ref} {text}\n".encode())
    report += b"0000"
    if "side-band-64k" in capabilities or "side-band" in capabilities:
        size = 65515 if "side-band-64k" in capabilities else 995
        framed = b""
        message = next((reason for reason in reasons if reason), "") + "\n"
        framed += pkt(b"\x02" + message.encode())
        for start in range(0, len(report), size):
            framed += pkt(b"\x01" + report[start : start + size])
        return framed + b"0000"
    return report


def parse_report_status(data: bytes, capabilities: set[str]) -> dict[str, str]:
    """Best effort: map ref -> "ok" or "ng <reason>" from a receive-pack result."""
    try:
        if "side-band-64k" in capabilities or "side-band" in capabilities:
            inner = b""
            offset = 0
            while offset + 4 <= len(data):
                length = int(data[offset : offset + 4], 16)
                if length == 0:
                    break
                if length < 5 or offset + length > len(data):
                    break
                if data[offset + 4] == 1:
                    inner += data[offset + 5 : offset + length]
                offset += length
            data = inner
        packets, _ = read_pkt_lines(data)
    except (NeedMore, RuleError, ValueError):
        return {}
    result = {}
    for packet in packets:
        if packet is None:
            continue
        line = packet.decode("utf-8", "replace").rstrip("\n")
        kind, _, rest = line.partition(" ")
        if kind == "ok":
            result[rest] = "ok"
        elif kind == "ng":
            ref, _, reason = rest.partition(" ")
            result[ref] = f"ng {reason}".strip()
    return result


# --------------------------------------------------------------------------- REST API


API_REPO_PATH = re.compile(r"/repos/([^/]+)/([^/]+)(/.*)?")


def api_decision(config: dict[str, Any], caller: Caller, method: str, path: str) -> tuple[str | None, str | None, str | None]:
    """Return (denial_reason, owner, repo) for a REST passthrough request."""
    if method not in API_METHODS:
        return "method_not_allowed", None, None
    if "//" in path or "/../" in path + "/" or "/./" in path + "/" or "%" in path:
        return "invalid_path", None, None
    repository = API_REPO_PATH.fullmatch(path)
    if repository is None:
        if method in ("GET", "HEAD") and match_any(config["api_global_reads"], path):
            return None, None, None
        return "path_not_allowed", None, None
    owner, repo = repository.group(1), repository.group(2)
    if not OWNER.fullmatch(owner) or not REPO.fullmatch(repo) or not repo_allowed(config, owner, repo):
        return "repo_not_allowed", owner, repo
    if method in ("GET", "HEAD") or caller.privileged(config):
        return None, owner, repo
    for rule in config["agent_api_writes"]:
        rule_method, rule_path = rule.split(" ", 1)
        if rule_method == method and match(rule_path.replace("{repo}", f"{owner}/{repo}"), path):
            return None, owner, repo
    return "agent_write_not_allowed", owner, repo
