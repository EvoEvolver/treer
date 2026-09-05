#!/usr/bin/env python3
import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path


def load_profiles(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    if data.get("schema") != "treer.launcher-profiles/v1":
        raise SystemExit(f"unsupported profile manifest: {path}")
    profiles = data.get("profiles")
    if not isinstance(profiles, list):
        raise SystemExit(f"profile manifest has no profiles array: {path}")
    return profiles


def run(argv: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, check=check, text=True, capture_output=not check)


def upsert(profile: dict, repo_cwd: str) -> None:
    name = profile["name"]
    description = profile.get("description", "")
    command = profile["run"]["command"]
    args = profile["run"].get("args", [])
    exists = run(
        ["treer", "agent", "admin", "profile", "show", name], check=False
    ).returncode == 0
    if exists:
        argv = [
            "treer", "agent", "admin", "profile", "update", name,
            "--description", description, "--cwd", repo_cwd, "--command", command,
        ]
        if args:
            for value in args:
                argv.extend(["--arg", value])
        else:
            argv.append("--clear-args")
    else:
        argv = [
            "treer", "agent", "admin", "profile", "create", name,
            "--description", description, "--cwd", repo_cwd, command,
        ]
        if args:
            argv.append("--")
            argv.extend(args)
    subprocess.check_call(argv)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--list", action="store_true")
    parser.add_argument("--agent", action="append", default=[])
    parser.add_argument(
        "--presentation", choices=["headless", "remote-codex-ui"], default="headless"
    )
    parser.add_argument("--repo-cwd")
    parser.add_argument("--machine")
    parser.add_argument("--launch", action="store_true")
    parser.add_argument("--check", action="store_true")
    options = parser.parse_args()
    profiles = load_profiles(options.manifest)

    if options.list:
        providers = sorted({profile["provider"] for profile in profiles})
        for provider in providers:
            print(provider)
        return

    if not options.agent:
        raise SystemExit("at least one --agent is required; use --list first")
    if not options.repo_cwd:
        raise SystemExit("--repo-cwd is required")
    if options.launch and not options.machine:
        raise SystemExit("--machine is required with --launch")

    known = {profile["provider"] for profile in profiles}
    unknown = sorted(set(options.agent) - known)
    if unknown:
        raise SystemExit(f"unknown ACP launcher: {', '.join(unknown)}")

    selected = [
        profile
        for profile in profiles
        if profile["provider"] in options.agent
        and profile["presentation"] == options.presentation
    ]
    for profile in selected:
        missing = [name for name in profile.get("requires", []) if shutil.which(name) is None]
        if missing:
            raise SystemExit(
                f"{profile['name']} requires commands not found on PATH: {', '.join(missing)}"
            )

    if options.check:
        return

    for profile in selected:
        upsert(profile, options.repo_cwd)
        print(f"installed profile: {profile['name']}")
        if not options.launch:
            continue
        agent_name = profile["agent_name"]
        if run(["treer", "agent", "show", agent_name], check=False).returncode == 0:
            print(f"Agent already exists: {agent_name}")
            continue
        subprocess.check_call(
            [
                "treer", "agent", "admin", "profile", "launch", profile["name"],
                "--machine", options.machine, "--name", agent_name,
            ]
        )


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
