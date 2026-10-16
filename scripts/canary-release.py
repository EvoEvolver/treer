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


GH_TIMEOUT = 120


def lap(stage, path):
    """Append the seconds since the previous lap (see scripts/release-timing.sh)."""
    if not path:
        return
    now = int(time.time())
    last_file = Path(str(path) + ".last")
    last = int(last_file.read_text()) if last_file.exists() else now
    with open(path, "a") as handle:
        handle.write(f"{stage}\t{now - last}\n")
    last_file.write_text(f"{now}\n")


def run(*args, cwd, capture=False, **kwargs):
    if args and args[0] == "gh":
        kwargs.setdefault("timeout", GH_TIMEOUT)  # an external hang must not outlive the CI deadline
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
    last_url = None
    while True:
        runs = json.loads(run("gh", "run", "list", "--workflow", "ci.yml", "--commit", commit,
                              "--limit", "20", "--json", "databaseId,status,conclusion,url,event,headSha",
                              cwd=root, capture=True))
        # A pull-request run, or a run for another commit, is not the gate for this release.
        runs = [item for item in runs
                if item["event"] in ("push", "workflow_dispatch") and item["headSha"] == commit]
        if runs:
            latest = max(runs, key=lambda item: item["databaseId"])
            last_url = latest["url"]
            if latest["status"] == "completed":
                if latest["conclusion"] != "success":
                    raise RuntimeError(f"CI {latest['conclusion']} for {commit}: {latest['url']}; "
                                       "fix it before publishing, no deployment started")
                print(f"Exact-commit CI passed: {latest['url']}")
                return latest["url"]
        if clock() >= deadline:
            seen = f" (last seen: {last_url})" if last_url else ""
            raise RuntimeError(f"no successful CI run for {commit} within {timeout}s{seen}; no deployment started")
        sleep(15)


WINDOWS_JOB = "Native Windows embedded launcher and ACP UI"


def windows_acceptance(checkout, commit, run_id):
    """Same rule as collect-build-artifacts.sh: an overall success does not prove the Windows job ran."""
    receipt = json.loads(run("gh", "run", "view", str(run_id), "--json",
                             "status,conclusion,headSha,displayTitle,jobs", cwd=checkout, capture=True))
    if receipt["status"] != "completed" or receipt["conclusion"] != "success" or not (
            receipt["headSha"] == commit or receipt["displayTitle"] == f"Machine artifacts {commit}"):
        raise RuntimeError(f"native run {run_id} is incomplete, failed or for another candidate")
    workflow = run("git", "show", f"{commit}:.github/workflows/build-release-artifacts.yml", cwd=checkout, capture=True)
    if any(line.startswith("  windows-launcher-smoke:") for line in workflow.splitlines()):
        jobs = [job for job in receipt["jobs"] if job["name"] == WINDOWS_JOB]
        if len(jobs) != 1 or jobs[0]["status"] != "completed" or jobs[0]["conclusion"] != "success":
            raise RuntimeError(f"native Windows launcher/UI acceptance did not pass in run {run_id}")


def native_ready(checkout, commit):
    """Native artifacts must be green for this exact commit or provably reused from a canary_passed release.

    A local dist/ is never evidence by itself: it passes only with a successful native run for the
    commit, or with reuse provenance that matches the hashes a canary_passed manifest recorded."""
    dist = checkout / "dist"
    if dist.is_dir() and (dist / "artifact-provenance.json").is_file():
        run("sh", "scripts/reuse-machine-artifacts.sh", commit, "--verify-dist", str(dist), cwd=checkout)
        print("Native artifacts: reused dist matches the canary_passed release that recorded it")
        return
    runs = json.loads(run("gh", "run", "list", "--workflow", "build-release-artifacts.yml", "--limit", "100",
                          "--json", "databaseId,status,conclusion,url,displayTitle,headSha",
                          cwd=checkout, capture=True))
    target = f"Machine artifacts {commit}"

    def for_commit(item):
        # A dispatch on ref C may build revision B: when the title names a revision it alone decides.
        if item["displayTitle"].startswith("Machine artifacts "):
            return item["displayTitle"] == target
        return item["headSha"] == commit

    runs = [item for item in runs if for_commit(item)]
    if runs:
        latest = max(runs, key=lambda item: item["databaseId"])
        if latest["status"] == "completed" and latest["conclusion"] == "success":
            windows_acceptance(checkout, commit, latest["databaseId"])
            if dist.is_dir():
                run("sh", "scripts/verify-machine-artifacts.sh", commit, str(dist), cwd=checkout)
            print(f"Native artifacts: build passed {latest['url']}")
            return
        raise RuntimeError(f"native artifact build for {commit} is {latest['status']}/{latest['conclusion']}: "
                           f"{latest['url']}; it must be green before publishing")
    if dist.is_dir():
        raise RuntimeError(f"dist/ exists but there is no native run for {commit} and no reuse provenance; "
                           "a local dist is not evidence of a green native build")
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
    timings = root / ".treer" / "releases" / commit / "timings.tsv"
    timings.parent.mkdir(parents=True, exist_ok=True)
    for stale in (timings, Path(str(timings) + ".last")):
        stale.unlink(missing_ok=True)
    Path(str(timings) + ".last").write_text(f"{int(time.time())}\n")
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
        lap("prepare_worktree", timings)
        wait_ci(root, commit)
        lap("ci_wait", timings)
        native_ready(checkout, commit)
        lap("native_ready", timings)
        run("pnpm", "--dir", "web", "install", "--frozen-lockfile", cwd=checkout)
        lap("pnpm_install", timings)
        env = os.environ.copy()
        env["TREER_TIMINGS_FILE"] = str(timings)
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
        if args.check_ci and args.dry_run:
            parser.error("--dry-run and --check-ci are mutually exclusive: dry-run makes no network calls")
        if args.check_ci:
            wait_ci(root, git(root, "rev-parse", "--verify", f"{args.revision}^{{commit}}"))
        else:
            deploy(root, args)
    except (RuntimeError, subprocess.SubprocessError, OSError, KeyError, ValueError) as error:
        print(f"Canary release failed: {error}", file=sys.stderr)
        print("Fix the failure, then rerun the same revision with --resume. No local runtime was restarted.",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
