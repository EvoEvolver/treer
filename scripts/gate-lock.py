#!/usr/bin/env python3
"""Machine-wide gate slot for heavy local gates (`just check`, e2e, launcher tests).

Several agents run full gates on one machine; the resulting load (11-27) makes
timing-sensitive tests flake. This wrapper serializes them with an advisory
flock under ~/.cache/treer-gate and gives release gates priority:

  python3 scripts/gate-lock.py [--role author|release] [--name NAME] -- COMMAND...

* One holder at a time (gate.lock, exclusive flock; the OS drops it when the
  holder dies, so a crash never leaves a stale lock).
* A release gate announces itself in release-wanted.json while it waits and
  while it runs; author gates do not start (and do not queue ahead) while a
  live release gate is announced.
* Nested use is a no-op: the child gets TREER_GATE_HELD=1.
* TREER_GATE_LOCK=0 or CI set: run the command directly (hosted CI has its own machine).
* Focused runs (single test files, cargo test -p ..., single recipes) do not use it.
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


def describe(info):
    return f"{info.get('name', '?')} (pid {info.get('pid', '?')}, since {info.get('since', '?')})"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--role", choices=["author", "release"], default=os.environ.get("TREER_GATE_ROLE", "author"))
    parser.add_argument("--name", default=os.environ.get("TREER_GATE_NAME") or os.environ.get("USER", "unknown"))
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
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
    lock_file = open(directory / "gate.lock", "a+")
    announced = False
    try:
        while True:
            if args.role == "author":
                other = release_wanted(directory, me)
                if other:
                    if not announced:
                        print(f"gate-lock: waiting for release gate {describe(other)}", file=sys.stderr, flush=True)
                        announced = True
                    time.sleep(POLL_SECONDS)
                    continue
            try:
                fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if not announced:
                    print(f"gate-lock: waiting for gate slot held by {describe(read_json(directory / 'gate.holder.json'))}",
                          file=sys.stderr, flush=True)
                    announced = True
                time.sleep(POLL_SECONDS)
        (directory / "gate.holder.json").write_text(json.dumps({"pid": me, "name": args.name, "role": args.role, "since": since}))
        print(f"gate-lock: slot acquired by {args.name} ({args.role})", file=sys.stderr, flush=True)
        env = dict(os.environ, TREER_GATE_HELD="1")
        return subprocess.call(command, env=env)
    finally:
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
