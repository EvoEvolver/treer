#!/usr/bin/env python3
"""Publish an exact commit to Canary from an isolated, reusable worktree."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def run(*args, cwd, capture=False, **kwargs):
    result = subprocess.run(args, cwd=cwd, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None, **kwargs)
    return result.stdout.strip() if capture else None


def git(root, *args):
    return run("git", *args, cwd=root, capture=True)


def checked_identity(root, expected):
    agent = json.loads(run("treer", "whoami", cwd=root, capture=True))["agent"]
    if expected not in (agent["agent_id"], agent["name"]):
        raise RuntimeError(f"caller is {agent['name']} ({agent['agent_id']}), expected {expected}; "
                           "run from the intended Agent, never copy peer credentials")
    return agent["agent_id"]


def candidate(root, revision):
    commit = git(root, "rev-parse", "--verify", f"{revision}^{{commit}}")
    checkout = root / ".treer" / "canary-checkouts" / commit
    return commit, checkout


def deploy(root, args):
    commit, checkout = candidate(root, args.revision)
    if args.dry_run:
        print(json.dumps({"commit": commit, "worktree": str(checkout),
                          "push": args.push, "resume": args.resume,
                          "notify": args.notify, "expected_agent": args.agent,
                          "local_runtime_changes": False}, indent=2))
        return
    required = ["git", "gh", "railway", "pnpm", "docker", "just", "jq", "curl"]
    if args.notify:
        required.append("treer")
    missing = [tool for tool in required if not shutil.which(tool)]
    if missing:
        raise RuntimeError("missing tools: " + ", ".join(missing))
    agent_id = checked_identity(root, args.agent) if args.notify else None
    lock = root / ".treer" / "canary-release.lock"
    lock.parent.mkdir(parents=True, exist_ok=True)
    try:
        lock.mkdir()
    except FileExistsError:
        raise RuntimeError(f"another release is active ({lock}); inspect it before removing the lock") from None
    try:
        run("git", "fetch", "origin", cwd=root)
        remote_ref = os.environ.get("TREER_RELEASE_REMOTE_REF", "origin/main")
        if args.push:
            if remote_ref != "origin/main":
                raise RuntimeError("--push only supports origin/main; push custom release branches explicitly")
            # A divergent history needs a reviewed merge, never an automatic force push.
            run("git", "merge-base", "--is-ancestor", remote_ref, commit, cwd=root)
            run("git", "push", "origin", f"{commit}:refs/heads/main", cwd=root)
            run("git", "fetch", "origin", cwd=root)
        run("git", "merge-base", "--is-ancestor", commit, remote_ref, cwd=root)
        if checkout.exists():
            if Path(git(checkout, "rev-parse", "--show-toplevel")).resolve() != checkout.resolve():
                raise RuntimeError(f"not a release worktree: {checkout}")
            if git(checkout, "rev-parse", "HEAD") != commit or git(checkout, "status", "--porcelain"):
                raise RuntimeError(f"release worktree is modified: {checkout}")
        else:
            checkout.parent.mkdir(parents=True, exist_ok=True)
            run("git", "worktree", "add", "--detach", str(checkout), commit, cwd=root)
        run("pnpm", "--dir", "web", "install", "--frozen-lockfile", cwd=checkout)
        env = os.environ.copy()
        env["TREER_RELEASE_RESUME"] = "1" if args.resume else "0"
        env["CARGO_TARGET_DIR"] = str(checkout / "target")
        # One maintained release pipeline; no local Host/Controller operations.
        run("sh", "scripts/release-canary.sh", commit, cwd=checkout, env=env)
        manifest_path = checkout / ".treer" / "releases" / commit / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        if manifest["commit"] != commit or manifest["status"] != "canary_passed":
            raise RuntimeError("release returned without an eligible manifest")
        deployment = manifest["canary"]["proxy_deployment_id"]
        summary = f"deploy 完成: {commit} / Proxy {deployment} / Canary 健康检查、产物校验及双机 E2E 通过"
        print(summary)
        print(f"Manifest: {manifest_path}")
        if args.notify:
            if checked_identity(root, args.agent) != agent_id:
                raise RuntimeError("caller identity changed; notification was not sent")
            run("treer", "message", "send", "--to", args.notify, "--idempotency-key",
                f"deploy-{commit}", cwd=root, input=summary + "\n")
    finally:
        lock.rmdir()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("revision", nargs="?", default="HEAD")
    parser.add_argument("--push", action="store_true", help="fast-forward origin/main to the candidate")
    parser.add_argument("--resume", action="store_true", help="reuse a matching completed local gate")
    parser.add_argument("--dry-run", action="store_true", help="print the local plan; no writes or network calls")
    parser.add_argument("--notify", metavar="AGENT", help="send completion to this Agent after success")
    parser.add_argument("--agent", metavar="SELF", help="required caller name/ID when --notify is used")
    args = parser.parse_args()
    if args.notify and not args.agent:
        parser.error("--notify requires --agent to verify the sender identity")
    root = Path(__file__).resolve().parent.parent
    try:
        deploy(root, args)
    except (RuntimeError, subprocess.CalledProcessError, OSError, KeyError, ValueError) as error:
        print(f"Canary release failed: {error}", file=sys.stderr)
        print("Fix the failure, then rerun the same revision with --resume. No local runtime was restarted.",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
