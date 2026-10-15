#!/usr/bin/env python3
"""Publish an exact commit to Canary from an isolated, reusable worktree."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


def run(*args, cwd, capture=False, **kwargs):
    result = subprocess.run(args, cwd=cwd, check=True, text=True,
                            stdout=subprocess.PIPE if capture else None, **kwargs)
    return result.stdout.strip() if capture else None


def git(root, *args):
    return run("git", *args, cwd=root, capture=True)


def checked_identity(root, expected):
    agent = json.loads(run("treer", "whoami", cwd=root, capture=True))["agent"]
    if expected not in (agent["agent_id"], agent.get("path"), agent["name"]):
        raise RuntimeError(f"caller is {agent['name']} ({agent['agent_id']}), expected {expected}; "
                           "run from the intended Agent, never copy peer credentials")
    return agent["agent_id"]


def candidate(root, revision):
    commit = git(root, "rev-parse", "--verify", f"{revision}^{{commit}}")
    checkout = root / ".treer" / "canary-checkouts" / commit
    return commit, checkout


def wait_ci(root, commit, timeout=None, sleep=time.sleep, clock=time.monotonic):
    """Return only when the exact commit has a successful push/dispatch CI run."""
    timeout = int(os.environ.get("TREER_RELEASE_CI_TIMEOUT", "2700")) if timeout is None else timeout
    deadline = clock() + timeout
    while True:
        runs = json.loads(run("gh", "run", "list", "--workflow", "ci.yml", "--commit", commit,
                              "--limit", "20", "--json", "databaseId,status,conclusion,url,event,headSha",
                              cwd=root, capture=True))
        # A pull-request run, or a run for another commit, is not the gate for this release.
        runs = [item for item in runs
                if item["event"] in ("push", "workflow_dispatch") and item["headSha"] == commit]
        if runs:
            latest = max(runs, key=lambda item: item["databaseId"])
            if latest["status"] == "completed":
                if latest["conclusion"] != "success":
                    raise RuntimeError(f"CI {latest['conclusion']} for {commit}: {latest['url']}; "
                                       "fix it before publishing, no deployment started")
                print(f"Exact-commit CI passed: {latest['url']}")
                return latest["url"]
        if clock() >= deadline:
            raise RuntimeError(f"no successful CI run for {commit} within {timeout}s; no deployment started")
        sleep(15)


def native_ready(checkout, commit):
    """Native artifacts must already be green for this commit or verifiably reusable."""
    dist = checkout / "dist"
    if dist.is_dir():
        run("sh", "scripts/verify-machine-artifacts.sh", commit, str(dist), cwd=checkout)
        print("Native artifacts: existing dist verified")
        return
    runs = json.loads(run("gh", "run", "list", "--workflow", "build-release-artifacts.yml", "--limit", "100",
                          "--json", "databaseId,status,conclusion,url,displayTitle,headSha",
                          cwd=checkout, capture=True))
    runs = [item for item in runs
            if item["displayTitle"] == f"Machine artifacts {commit}" or item["headSha"] == commit]
    if runs:
        latest = max(runs, key=lambda item: item["databaseId"])
        if latest["status"] == "completed" and latest["conclusion"] == "success":
            print(f"Native artifacts: build passed {latest['url']}")
            return
        raise RuntimeError(f"native artifact build for {commit} is {latest['status']}/{latest['conclusion']}: "
                           f"{latest['url']}; it must be green before publishing")
    # No build exists: only a verified reuse of a canary_passed anchor may stand in for one.
    try:
        run("sh", "scripts/reuse-machine-artifacts.sh", commit, "--check-only", cwd=checkout)
    except subprocess.CalledProcessError:
        raise RuntimeError(f"no green native build and no verifiable reuse anchor for {commit}; "
                           "dispatch the Build release artifacts workflow and wait for it") from None


def deploy(root, args):
    commit, checkout = candidate(root, args.revision)
    if args.dry_run:
        print(json.dumps({"commit": commit, "worktree": str(checkout),
                          "push": args.push, "resume": args.resume,
                          "notify": args.notify, "expected_agent": args.agent,
                          "requires": ["green exact-commit CI", "green or verified-reused native artifacts"],
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
        wait_ci(root, commit)
        native_ready(checkout, commit)
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
    parser.add_argument("--check-ci", action="store_true", help="only wait for successful exact-commit CI, then exit")
    parser.add_argument("--notify", metavar="AGENT", help="send completion to this Agent after success")
    parser.add_argument("--agent", metavar="SELF", help="required caller name/ID when --notify is used")
    args = parser.parse_args()
    if args.notify and not args.agent:
        parser.error("--notify requires --agent to verify the sender identity")
    root = Path(__file__).resolve().parent.parent
    try:
        if args.check_ci:
            wait_ci(root, git(root, "rev-parse", "--verify", f"{args.revision}^{{commit}}"))
        else:
            deploy(root, args)
    except (RuntimeError, subprocess.CalledProcessError, OSError, KeyError, ValueError) as error:
        print(f"Canary release failed: {error}", file=sys.stderr)
        print("Fix the failure, then rerun the same revision with --resume. No local runtime was restarted.",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
