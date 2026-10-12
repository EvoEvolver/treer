#!/usr/bin/env python3
"""Read-only inventory; never remove a checkout or branch."""
import argparse
import json
from pathlib import Path
import subprocess


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=True).stdout.strip()


def inventory(root):
    records = []
    for block in git(root, "worktree", "list", "--porcelain").split("\n\n"):
        fields = dict(line.split(" ", 1) if " " in line else (line, "") for line in block.splitlines())
        if "worktree" not in fields:
            continue
        path = Path(fields["worktree"])
        record = {"path": str(path), "branch": fields.get("branch", "detached").removeprefix("refs/heads/"),
                  "head": fields["HEAD"], "clean": False, "merged": False}
        try:
            record["clean"] = not git(path, "status", "--porcelain")
            record["merged"] = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor",
                record["head"], "main"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
            record["candidate"] = record["clean"] and record["merged"] and record["branch"] != "main"
        except (OSError, subprocess.CalledProcessError):
            record["candidate"] = False
            record["error"] = "unreadable checkout; preserve"
        records.append(record)
    registered = {record["path"] for record in records}
    # Include old directories not registered in this repository, but never declare them safe.
    possible = list(root.parent.glob(root.name + "-*"))
    cache = root / ".treer/canary-checkouts"
    if cache.is_dir():
        possible += list(cache.iterdir())
    for path in possible:
        if path.is_dir() and str(path) not in registered:
            records.append({"path": str(path), "candidate": False, "error": "unregistered directory; preserve"})
    branches = git(root, "for-each-ref", "--merged=main", "--format=%(refname:short)", "refs/heads").splitlines()
    return {"main": git(root, "rev-parse", "main"), "worktrees": records,
            "merged_branches": [name for name in branches if name != "main"],
            "removal_authorized": False,
            "note": "Candidates still require explicit cleanup approval and a fresh clean/merged check; active owner checkouts are not automatically safe."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    checkout = Path(__file__).resolve().parent.parent
    common = Path(git(checkout, "rev-parse", "--path-format=absolute", "--git-common-dir"))
    parser.add_argument("--root", type=Path, default=common.parent)
    args = parser.parse_args()
    print(json.dumps(inventory(args.root), indent=2))


if __name__ == "__main__":
    main()
