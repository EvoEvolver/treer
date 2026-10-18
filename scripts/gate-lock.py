#!/usr/bin/env python3
"""Machine-wide gate slot for heavy local gates (`just check`, e2e, launcher tests).

Several agents run full gates on one machine; the resulting load (11-27) makes
timing-sensitive tests flake. This wrapper serializes them with an advisory
flock under ~/.cache/treer-gate, gives release gates priority and lets authors
wait in arrival order:

  python3 scripts/gate-lock.py [--role author|release] [--name NAME] -- COMMAND...
  python3 scripts/gate-lock.py --status [--json]

* One holder at a time (gate.lock, exclusive flock; the OS drops it when the
  holder dies, so a crash never leaves a stale lock).
* Every waiter takes a ticket (queue/<sequence>-<pid>.json, sequence from a
  counter file under a short lock) and only the head of the queue contends for
  the slot, so a newer waiter can never win it from an older one. Release
  tickets sort before author tickets; within a role the order is first come,
  first served.
* A waiter refreshes its ticket every poll. A ticket whose process is dead, or
  that has not been refreshed for TREER_GATE_STALE_SECONDS (default 30; this also
  covers a reused pid or a hung waiter), is removed by whoever sees it.
* A release gate also announces itself in release-wanted.json while it waits and
  while it runs (kept for tools that read it).
* Nested use is a no-op: the child gets TREER_GATE_HELD=1.
* TREER_GATE_LOCK=0 or CI set: run the command directly (hosted CI has its own machine).
* Focused runs (single test files, cargo test -p ..., single recipes) do not use it.
* Waiters started before this queue existed do not take tickets and can still cut in;
  that ends as soon as they have run.
"""
import argparse
import fcntl
import json
import os
import subprocess
import sys
import time
from pathlib import Path

POLL_SECONDS = float(os.environ.get("TREER_GATE_POLL_SECONDS", "2"))
STALE_SECONDS = float(os.environ.get("TREER_GATE_STALE_SECONDS", "30"))
ROLE_RANK = {"release": 0, "author": 1}


def gate_dir():
    path = Path(os.environ.get("TREER_GATE_DIR") or Path.home() / ".cache" / "treer-gate")
    path.mkdir(parents=True, exist_ok=True)
    return path


def alive(pid):
    try:
        os.kill(int(pid), 0)
    except (OSError, ValueError, TypeError):
        return False
    return True


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def release_wanted(directory, own_pid):
    info = read_json(directory / "release-wanted.json")
    if info.get("pid") and info["pid"] != own_pid and alive(info["pid"]):
        return info
    return None


def queue_dir(directory):
    path = directory / "queue"
    path.mkdir(parents=True, exist_ok=True)
    return path


def next_sequence(directory):
    """Monotonic ticket number; the counter file is only touched under queue.lock."""
    with open(directory / "queue.lock", "a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        counter = directory / "queue.seq"
        try:
            last = int(counter.read_text().strip() or 0)
        except (OSError, ValueError):
            last = 0
        sequence = last + 1
        temporary = directory / f"queue.seq.{os.getpid()}.tmp"
        temporary.write_text(str(sequence))
        os.replace(temporary, counter)
        return sequence


def enqueue(directory, role, name, since):
    sequence = next_sequence(directory)
    path = queue_dir(directory) / f"{sequence:012d}-{os.getpid()}.json"
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps({"pid": os.getpid(), "name": name, "role": role, "since": since, "seq": sequence}))
    os.replace(temporary, path)  # a reader never sees a half-written ticket
    return path


def tickets(directory, clean=True):
    """Live tickets in queue order. Dead or unrefreshed ones are dropped (and removed when `clean`)."""
    live = []
    now = time.time()
    for path in sorted(queue_dir(directory).glob("*.json")):
        info = read_json(path)
        if not info or "seq" not in info:
            continue
        try:
            fresh = now - path.stat().st_mtime <= STALE_SECONDS
        except OSError:
            continue  # raced with its owner removing it
        if not (fresh and alive(info.get("pid"))):
            if clean:
                try:
                    path.unlink()
                except OSError:
                    pass
            continue
        live.append(info)
    return sorted(live, key=lambda info: (ROLE_RANK.get(info.get("role"), 1), info["seq"]))


def describe(info):
    return f"{info.get('name', '?')} (pid {info.get('pid', '?')}, since {info.get('since', '?')})"


def status_text(directory):
    holder = read_json(directory / "gate.holder.json")
    lines = [f"gate-lock status ({directory})"]
    if holder and alive(holder.get("pid")):
        lines.append(f"holder: {describe(holder)} role={holder.get('role', '?')}")
    else:
        lines.append("holder: none (slot free)")
    waiting = tickets(directory, clean=False)
    lines.append(f"queue: {len(waiting)} waiting" if waiting else "queue: empty")
    for position, info in enumerate(waiting, 1):
        lines.append(f"  {position}. {describe(info)} role={info.get('role', '?')} ticket={info['seq']}")
    return "\n".join(lines)


def status_json(directory):
    holder = read_json(directory / "gate.holder.json")
    return json.dumps({"holder": holder if holder and alive(holder.get("pid")) else None,
                       "queue": tickets(directory, clean=False)}, indent=2)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--role", choices=["author", "release"], default=os.environ.get("TREER_GATE_ROLE", "author"))
    parser.add_argument("--name", default=os.environ.get("TREER_GATE_NAME") or os.environ.get("USER", "unknown"))
    parser.add_argument("--status", action="store_true", help="show the holder and the waiting queue, then exit")
    parser.add_argument("--json", action="store_true", help="with --status: machine-readable output")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.status:
        directory = gate_dir()
        print(status_json(directory) if args.json else status_text(directory))
        return 0
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a command is required after --")
    if os.environ.get("TREER_GATE_HELD") == "1" or os.environ.get("TREER_GATE_LOCK") == "0" or os.environ.get("CI"):
        return subprocess.call(command)

    directory = gate_dir()
    me = os.getpid()
    wanted = directory / "release-wanted.json"
    since = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    if args.role == "release":
        wanted.write_text(json.dumps({"pid": me, "name": args.name, "since": since}))
    ticket = enqueue(directory, args.role, args.name, since)
    lock_file = open(directory / "gate.lock", "a+")
    announced = None
    try:
        while True:
            try:
                os.utime(ticket)  # heartbeat: a waiter that stops refreshing loses its place
            except OSError:
                ticket = enqueue(directory, args.role, args.name, since)  # cleaned by mistake: queue up again
            line = tickets(directory)
            head = line[0] if line else None
            if head and head["pid"] == me:
                try:
                    fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    message = f"gate-lock: waiting for gate slot held by {describe(read_json(directory / 'gate.holder.json'))}"
            elif head and head.get("role") == "release" and args.role == "author":
                message = f"gate-lock: waiting for release gate {describe(head)}"
            else:
                ahead = sum(1 for info in line if info["pid"] != me and (ROLE_RANK.get(info.get("role"), 1), info["seq"]) <
                            (ROLE_RANK[args.role], read_json(ticket).get("seq", 0)))
                message = (f"gate-lock: waiting for gate slot held by {describe(read_json(directory / 'gate.holder.json'))};"
                           f" {ahead} waiting ahead in the queue")
            if message != announced:
                print(message, file=sys.stderr, flush=True)
                announced = message
            time.sleep(POLL_SECONDS)
        try:
            ticket.unlink()  # the slot is ours: the next waiter becomes head
        except OSError:
            pass
        # "since" is when the slot was acquired; "queued_since" is when this process began waiting.
        acquired = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        (directory / "gate.holder.json").write_text(json.dumps({"pid": me, "name": args.name, "role": args.role,
                                                                "since": acquired, "queued_since": since}))
        print(f"gate-lock: slot acquired by {args.name} ({args.role})", file=sys.stderr, flush=True)
        env = dict(os.environ, TREER_GATE_HELD="1")
        return subprocess.call(command, env=env)
    finally:
        try:
            ticket.unlink()
        except OSError:
            pass
        if args.role == "release" and read_json(wanted).get("pid") == me:
            try:
                wanted.unlink()
            except OSError:
                pass
        try:
            (directory / "gate.holder.json").unlink()
        except OSError:
            pass
        lock_file.close()


if __name__ == "__main__":
    raise SystemExit(main())
