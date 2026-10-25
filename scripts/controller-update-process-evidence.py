#!/usr/bin/env python3
"""Read-only Linux /proc evidence for the isolated #222 collector.

No signals, subprocesses, updater calls, credential reads or mutation retries.
Scope is exe/cwd holders for one observed process UID, not every
descriptor or proof that every descendant exited. Denied evidence stays unknown.
"""
import argparse
import errno
import json
import os
from pathlib import Path
import re
import stat
import sys

MAX_PIDS = 4096
MAX_BYTES = 2 * 1024 * 1024


class EvidenceError(Exception):
    pass


def error_kind(error):
    if error.errno in (errno.ENOENT, errno.ESRCH):
        return "absent"
    if error.errno in (errno.EACCES, errno.EPERM):
        return "denied"
    return "io_error"


def parse_stat(raw, pid):
    # comm may contain spaces, parentheses and newlines. It is never emitted.
    if len(raw) > 4096:
        raise EvidenceError("stat_oversize")
    end = raw.rfind(b") ")
    if end < 0 or not raw.startswith(str(pid).encode() + b" ("):
        raise EvidenceError("stat_invalid")
    parts = raw[end + 2:].split()
    if len(parts) < 20 or len(parts[0]) != 1:
        raise EvidenceError("stat_invalid")
    state = parts[0].decode("ascii")
    if state not in "RSDZTtXxKWPI":
        raise EvidenceError("stat_state_invalid")
    values = [int(parts[i]) for i in (1, 2, 3, 19)]
    if min(values) < 0 or values[-1] == 0:
        raise EvidenceError("stat_identity_invalid")
    return dict(pid=pid, state=state, ppid=values[0], pgid=values[1],
                sid=values[2], start_ticks=values[3])


class Proc:
    def pids(self):
        result = []
        with os.scandir("/proc") as entries:
            for entry in entries:
                if entry.name.isascii() and entry.name.isdecimal():
                    result.append(int(entry.name))
                    if len(result) > MAX_PIDS:
                        raise EvidenceError("proc_scan_limit")
        return sorted(result)

    def owner(self, pid):
        return os.stat(f"/proc/{pid}", follow_symlinks=False).st_uid

    def uids(self, pid):
        fd = os.open(f"/proc/{pid}/status", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            raw = os.read(fd, 16385)
        finally:
            os.close(fd)
        if len(raw) > 16384:
            raise EvidenceError("status_oversize")
        values = [line.split()[1:] for line in raw.splitlines() if line.startswith(b"Uid:")]
        if len(values) != 1 or len(values[0]) != 4:
            raise EvidenceError("status_uid_invalid")
        result = [int(value) for value in values[0]]
        if min(result) < 0:
            raise EvidenceError("status_uid_invalid")
        return result

    def snapshot(self, pid):
        fd = os.open(f"/proc/{pid}/stat", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            raw = os.read(fd, 4097)
        finally:
            os.close(fd)
        value = parse_stat(raw, pid)
        value["proc_uid"] = self.owner(pid)
        value["uids"] = self.uids(pid)
        return value

    def link(self, pid, field):
        return os.readlink(f"/proc/{pid}/{field}")


def attempt(call):
    try:
        return {"status": "ok", "value": call()}
    except OSError as error:
        return {"status": error_kind(error), "errno": error.errno}
    except (EvidenceError, ValueError, UnicodeError):
        return {"status": "invalid"}


def within(value, root):
    if len(os.fsencode(value)) > 8192 or not value.startswith("/"):
        raise EvidenceError("link_invalid")
    # /proc returns absolute kernel paths; do not resolve or open their targets.
    if value == root or value.startswith(root + "/"):
        return True
    # Deleted executable links still establish a holder of the task image.
    if value.endswith(" (deleted)"):
        value = value[:-10]
        return value == root or value.startswith(root + "/")
    return False


def stable(before, after):
    return all(before[key] == after[key] for key in
               ("pid", "start_ticks", "proc_uid", "uids"))


def observe(proc, pid, root, uid):
    before = attempt(lambda: proc.snapshot(pid))
    row = {"pid": pid, "before": before}
    fields = {}
    for name in ("exe", "cwd"):
        result = attempt(lambda: within(proc.link(pid, name), root))
        if result["status"] == "ok":
            result["under_task_root"] = result.pop("value")
        fields[name] = result
    row["fields"] = fields
    after = attempt(lambda: proc.snapshot(pid))
    row["after"] = after
    if after["status"] == "absent":
        # Explicit disappearance observation, not inferred from a missing link.
        row["classification"] = "absent_after"
    elif before["status"] != "ok" or after["status"] != "ok":
        row["classification"] = "unknown_identity"
    elif not stable(before["value"], after["value"]):
        row["classification"] = "unknown_identity_changed"
    elif after["value"]["proc_uid"] != uid and uid not in after["value"]["uids"]:
        row["classification"] = "unknown_owner_changed"
    elif before["value"]["state"] in ("Z", "X", "x") and after["value"]["state"] in ("Z", "X", "x"):
        # Exact stable kernel identity is dead; retain all link errors as data.
        row["classification"] = "kernel_exited"
    elif any(field.get("under_task_root") is True for field in fields.values()):
        row["classification"] = "task_root_holder"
    elif all(field["status"] == "ok" for field in fields.values()):
        # This says nothing about task ancestry or other held descriptors.
        row["classification"] = "no_exe_cwd_root_reference"
    else:
        row["classification"] = "unknown_links"
    return row


def collect(proc, root, uid):
    rows, excluded, errors = [], 0, []
    pids = proc.pids()
    if len(pids) > MAX_PIDS:
        raise EvidenceError("proc_scan_limit")
    for pid in pids:
        owner = attempt(lambda: proc.owner(pid))
        uids = attempt(lambda: proc.uids(pid))
        if owner["status"] == "absent" and uids["status"] == "absent":
            continue
        if owner.get("value") == uid or uid in uids.get("value", []):
            rows.append(observe(proc, pid, root, uid))
        elif owner["status"] == "ok" and uids["status"] == "ok":
            excluded += 1
        else:
            errors.append({"pid": pid, "owner": owner, "uids": uids})
    holders = [row["pid"] for row in rows if row["classification"] == "task_root_holder"]
    unknown = [row["pid"] for row in rows if row["classification"].startswith("unknown")]
    unknown += [row["pid"] for row in errors]
    return dict(schema="treer.proc-root-evidence/v1", scope="matching observed UID or proc owner exe/cwd only; not all descendants or file descriptors",
                uid=uid, max_pids=MAX_PIDS, examined=len(pids), excluded_proven_other_uid=excluded,
                processes=rows, owner_errors=errors, holders=holders, unknown=unknown,
                root_references_proven_absent=not holders and not unknown,
                task_membership_inferred_from_uid=False, query_only=True)


def collector_identity(uid, hosted_root_observer, root):
    if sys.platform != "linux" or uid <= 0:
        raise ValueError("requires Linux and a non-root subject UID")
    if not hosted_root_observer:
        if uid != os.geteuid():
            raise ValueError("requires the actual non-root collector UID")
        return
    # Explicit disposable hosted observer only. Privilege improves read access,
    # never changes the subject UID, classification or denied/unknown outcome.
    if not (os.getuid() == os.geteuid() == 0
            and os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and os.environ.get("TREER_DISPOSABLE_RUNNER") == "1"
            and os.environ.get("TREER_ACCEPTANCE_ROOT") == str(root)
            and re.fullmatch(r"tn-[0-9a-f]{16}", root.name)):
        raise ValueError("requires an explicit disposable hosted root observer")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--uid", type=int, required=True)
    parser.add_argument("--hosted-root-observer", action="store_true")
    args = parser.parse_args()
    root = args.root
    try:
        collector_identity(args.uid, args.hosted_root_observer, root)
    except ValueError as error:
        parser.error(str(error))
    if not root.is_absolute() or root.resolve(strict=True) != root or root.parent == root:
        parser.error("root must be a physical absolute private directory")
    info = root.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != args.uid or stat.S_IMODE(info.st_mode) != 0o700:
        parser.error("root must be an owned 0700 directory")
    try:
        result = collect(Proc(), str(root), args.uid)
        result.update(observer_uid=os.geteuid(), hosted_root_observer=args.hosted_root_observer)
        raw = json.dumps(result, sort_keys=True).encode()
        if len(raw) > MAX_BYTES:
            raise EvidenceError("receipt_limit")
    except (OSError, EvidenceError) as error:
        # No raw OS error string/path. Collection failure never yields success.
        print(json.dumps({"schema": "treer.proc-root-evidence/v1", "query_only": True,
                          "root_references_proven_absent": False, "collector_error": type(error).__name__}))
        return 2
    print(raw.decode())
    return 0 if result["root_references_proven_absent"] else 1


if __name__ == "__main__":
    sys.exit(main())
