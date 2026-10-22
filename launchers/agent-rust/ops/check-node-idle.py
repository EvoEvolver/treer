#!/usr/bin/env python3
"""Read-only, content-free migration eligibility check; never edits receipts.

Run only after the operator verifies launcher AND provider exit. This checks
durable pending work, not full import compatibility or provider delivery.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import stat
import sys


def require(condition):
    if not condition:
        raise ValueError("invalid")


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result)
        result[key] = value
    return result


def read_private(path, limit):
    path = Path(path)
    require(path.is_absolute())
    for parent in path.parents:
        m = parent.lstat()
        require(stat.S_ISDIR(m.st_mode) and not stat.S_ISLNK(m.st_mode))
        require(m.st_uid in (0, os.geteuid()))
        require(not m.st_mode & 0o022 or m.st_mode & stat.S_ISVTX)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1)
        require(before.st_uid == os.geteuid() and not before.st_mode & 0o077)
        require(before.st_size <= limit)
        data = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        require(len(data) <= limit)
        require((before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns))
    return data


def inspect(source, thread, private=None, delivery_audit=False):
    if private is not None:
        require(private.is_absolute())
        # First switch only: never interpret a leftover backup/journal as fresh.
        for directory in [private, *private.parents]:
            m = directory.lstat()
            require(stat.S_ISDIR(m.st_mode) and not stat.S_ISLNK(m.st_mode))
            require(m.st_uid in (0, os.geteuid()))
            require(not m.st_mode & 0o022 or m.st_mode & stat.S_ISVTX)
        m = private.lstat()
        require(m.st_uid == os.geteuid() and not m.st_mode & 0o077)
        require(set(os.listdir(private)) <= {"images", ".rust-launcher.lock"})
        images = private / "images"
        m = images.lstat()
        require(stat.S_ISDIR(m.st_mode) and not stat.S_ISLNK(m.st_mode))
        require(m.st_uid == os.geteuid() and not m.st_mode & 0o077)
        require(not os.listdir(images))
        if (private / ".rust-launcher.lock").exists() or (private / ".rust-launcher.lock").is_symlink():
            require(read_private(private / ".rust-launcher.lock", 0) == b"")
    names = set(os.listdir(source))
    sidecar = "agent-ui.json.goal-usage.json"
    require({"agent-ui.json", "queue.json"} <= names <= {"agent-ui.json", "queue.json", sidecar})
    sidecar_bytes = None
    if sidecar in names:
        sidecar_bytes = read_private(source / sidecar, 65536)
        usage = json.loads(sidecar_bytes, object_pairs_hook=unique_pairs)
        require(isinstance(usage, dict) and set(usage) == {"sessionId", "goalKey", "goal", "pendingExit", "tokensUsed", "timeUsedSeconds", "resultTokens"})
        session = usage["sessionId"]
        require(isinstance(session, str) and 0 < len(session.encode()) <= 1024 and all(c.isascii() and (c.isalnum() or c in "_-") for c in session))
        require(usage["goalKey"] == "" and all(usage[k] is None for k in ("goal", "pendingExit", "tokensUsed", "timeUsedSeconds")))
        tokens = usage["resultTokens"]
        require(type(tokens) in (int, float) and math.isfinite(tokens) and tokens >= 0)
    thread_bytes = read_private(source / "agent-ui.json", 65536)
    queue_bytes = read_private(source / "queue.json", 32 * 1024 * 1024)
    ui = json.loads(thread_bytes, object_pairs_hook=unique_pairs)
    queue = json.loads(queue_bytes, object_pairs_hook=unique_pairs)
    require(isinstance(ui, dict) and ui.get("threadId") == thread)
    require(isinstance(queue, dict) and type(queue.get("version")) is int and queue["version"] == 1)
    records = queue.get("records")
    require(isinstance(records, list) and len(records) <= 4096)
    counts = dict(queued=0, running=0, pendingAck=0, pendingNotification=0, attachmentReferences=0)
    deliveries = []
    for r in records:
        require(isinstance(r, dict) and isinstance(r.get("submission"), dict))
        counts["attachmentReferences"] += int("attachmentIds" in r or "attachmentSession" in r)
        state = r["submission"].get("state")
        require(state in ("queued", "running", "done", "failed", "interrupted"))
        if state in ("queued", "running"):
            counts[state] += 1
        messages = r.get("sourceMessages", [])
        require(isinstance(messages, list) and len(messages) <= 255)
        if r.get("sourceMessage") is not None:
            messages = [r["sourceMessage"]] + messages
        for message in messages:
            require(isinstance(message, dict) and type(message.get("ackPending")) is bool)
            counts["pendingAck"] += int(message["ackPending"])
        if delivery_audit:
            ids = [r["submission"].get("id")] + [m.get(k) for m in messages for k in ("messageId", "deliveryId")]
            require(all(isinstance(value, str) and 0 < len(value) <= 256 and
                        all(c.isascii() and (c.isalnum() or c in "_-") for c in value) for value in ids))
            deliveries.append(dict(submission=dict(id=r["submission"]["id"], state=state),
                                   sourceMessages=[{key: m[key] for key in ("messageId", "deliveryId", "ackPending")} for m in messages]))
        require("notification" in r and "notify" in r)
        notification = r["notification"]
        notify = r["notify"]
        require(notify is None or isinstance(notify, dict))
        on = [] if notify is None else notify.get("on")
        require(isinstance(on, list) and all(x in ("done", "failed", "interrupted") for x in on))
        if notification is not None:
            require(isinstance(notification, dict) and type(notification.get("delivered")) is bool)
            counts["pendingNotification"] += int(not notification["delivered"])
        elif state in on:
            counts["pendingNotification"] += 1
    result = dict(code="node_idle" if not any(counts.values()) else "node_pending_work",
                records=len(records), **counts, queueBytes=len(queue_bytes),
                queueSha256=hashlib.sha256(queue_bytes).hexdigest(),
                threadFileSha256=hashlib.sha256(thread_bytes).hexdigest(),
                files=[dict(name=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
                       for name, data in [("agent-ui.json", thread_bytes), ("queue.json", queue_bytes)]])
    if delivery_audit:
        result.update(deliveries=deliveries, privateRootChecked=private is not None)
    if sidecar_bytes is not None:
        result["files"].append(dict(name=sidecar, bytes=len(sidecar_bytes), sha256=hashlib.sha256(sidecar_bytes).hexdigest()))
        result["inertGoalUsageSidecar"] = True
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--thread", required=True)
    parser.add_argument("--private-root", required=True, type=Path)
    parser.add_argument("--source-only", action="store_true", help="pre-stop source audit only; does not check private-root eligibility")
    parser.add_argument("--delivery-audit", action="store_true", help="include submission state and message/delivery IDs only, never text")
    args = parser.parse_args()
    try:
        result = inspect(args.source, args.thread, None if args.source_only else args.private_root, args.delivery_audit)
    except Exception:
        print('{"code":"node_idle_check_invalid"}')
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0 if result["code"] == "node_idle" else 1


if __name__ == "__main__":
    sys.exit(main())
