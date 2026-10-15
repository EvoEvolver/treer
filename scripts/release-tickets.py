#!/usr/bin/env python3
"""Create the Issues ticket chain for one release candidate.

Reviews come first so the release can be blocked_by them, and the acceptance
ticket is blocked_by the release. Without --create the commands are only
printed. The Issues CLI supplies its own identity; this script never reads or
stores credentials.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CLI = ROOT / "apps" / "issues" / "issues"


def parse_pair(value):
    role, separator, owner = value.partition("=")
    if not separator or not role or not owner:
        raise argparse.ArgumentTypeError("expected ROLE=ASSIGNEE")
    return role, owner


def steps(options):
    """Yield (key, command arguments, names of earlier keys it is blocked by)."""
    reviews = []
    for role, owner in options.review:
        key = f"review:{role}"
        reviews.append(key)
        yield key, ["create", f"[Review] {options.name}: {role}", "--assignee", owner,
                    "--description", f"Review the frozen candidate for {options.name}; record the exact commit and verdict as a comment."], []
    yield "release", ["create", f"[Release] {options.name}", "--assignee", options.release_owner,
                      "--label", "type:release",
                      "--description", "Record each step as '[release] <step> <SHA> <result>': integrate, check, push, CI, native, canary, policy check."], reviews
    for role, owner in options.acceptance:
        yield f"acceptance:{role}", ["create", f"[Acceptance] {options.name}: {role}", "--assignee", owner,
                                     "--description", "Accept the canary-passed release on the real target; record machine, version and result."], ["release"]


def run(cli, arguments):
    result = subprocess.run([sys.executable, str(cli), *arguments], check=True, capture_output=True, text=True)
    return json.loads(result.stdout)["issue"]["id"]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True, help="release name, e.g. 'launcher batch 3'")
    parser.add_argument("--parent", type=int, required=True, help="epic ticket id")
    parser.add_argument("--release-owner", required=True)
    parser.add_argument("--review", action="append", default=[], type=parse_pair, metavar="ROLE=ASSIGNEE")
    parser.add_argument("--acceptance", action="append", default=[], type=parse_pair, metavar="ROLE=ASSIGNEE")
    parser.add_argument("--issues-cli", default=str(DEFAULT_CLI))
    parser.add_argument("--create", action="store_true", help="create the tickets (default: print only)")
    options = parser.parse_args(argv)
    created = {}
    for key, arguments, blockers in steps(options):
        arguments = [*arguments, "--parent-id", str(options.parent)]
        for blocker in blockers:
            if options.create:
                arguments += ["--blocked-by", str(created[blocker])]
            else:
                arguments += ["--blocked-by", f"<{blocker}>"]
        if options.create:
            created[key] = run(options.issues_cli, arguments)
            print(f"{key}: #{created[key]}")
        else:
            print("issues", *[json.dumps(a) if " " in a else a for a in arguments])
    return 0


if __name__ == "__main__":
    sys.exit(main())
