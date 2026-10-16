#!/usr/bin/env python3
"""Classify a candidate by changed paths and print the gates it must pass.

This only plans; it never runs a gate, pushes, or deploys. Unknown paths
classify as core, so a new top-level directory can only make a release stricter.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]

# Order matters only for the report; a file belongs to the first matching rule.
RULES = [
    ("neutral", ("/.gitattributes", "/.gitignore")),
    ("docs", ("docs/", "/AGENTS.md", "/README.md", "/PLAN.md", "/LICENSE")),
    ("apps", ("apps/",)),
    ("web", ("web/",)),
    ("launcher", ("launchers/",)),
    ("pipeline", ("scripts/canary-", "scripts/release-canary.sh", "scripts/deploy-canary.sh",
                  "scripts/test-canary.sh", "scripts/promote-production.sh",
                  "scripts/release-checkpoint.py", "scripts/release-r2.mjs",
                  "scripts/verify-", "scripts/stage-", "scripts/collect-build-artifacts.sh",
                  "scripts/ensure-machine-artifacts.sh", "scripts/install.ps1",
                  "scripts/update-machine", "scripts/prepare-launcher-release.mjs",
                  "scripts/railway-error-summary.sh", "scripts/retag-ghcr.sh",
                  ".github/workflows/build-release-artifacts.yml", ".github/workflows/publish-ghcr.yml")),
    ("tooling", ("scripts/", ".github/", "justfile")),
]
ORDER = ["docs", "apps", "tooling", "web", "launcher", "pipeline", "core"]

LOCAL_GATE = {
    "docs": ["node scripts/check-docs.mjs"],
    "web": ["cd web && pnpm typecheck", "cd web && pnpm build", "cd web && pnpm test:e2e"],
    "launcher": ["just agent-launcher-test"],
    "tooling": ["just check"],
    "pipeline": ["just check"],
    "core": ["just check"],
}


def classify(path):
    for component, prefixes in RULES:
        for prefix in prefixes:
            if prefix.startswith("/"):
                if path == prefix[1:]:
                    return component
            elif path.startswith(prefix):
                return component
    return "core"


def app_gates(paths):
    names = sorted({p.split("/")[1] for p in paths if p.startswith("apps/") and p.count("/") >= 2})
    gates = []
    for name in names:
        tests = ROOT / "apps" / name / "tests"
        if tests.is_dir() and any(tests.glob("test_*.py")):
            gates.append(f"python3 -m unittest discover -s apps/{name}/tests -p 'test_*.py' -v")
        package = ROOT / "apps" / name / "package.json"
        if package.is_file():
            try:
                scripts = json.loads(package.read_text()).get("scripts", {})
            except (OSError, ValueError):
                scripts = {}
            # Only suggest a gate that exists: an App can have a package.json without a check script.
            if isinstance(scripts, dict) and "check" in scripts:
                gates.append(f"npm --prefix apps/{name} run check")
    return names, gates


def plan(paths, urgent=False):
    by_component = {}
    for path in paths:
        by_component.setdefault(classify(path), []).append(path)
    neutral = by_component.pop("neutral", [])
    components = [c for c in ORDER if c in by_component]
    if not components and neutral:
        components = ["docs"]
    local = []
    apps, app_commands = app_gates(by_component.get("apps", []))
    for component in components:
        if component == "apps":
            local.extend(["node scripts/check-docs.mjs", *app_commands])
        else:
            local.extend(LOCAL_GATE[component])
    local = list(dict.fromkeys(local))
    if any(c in components for c in ("core", "tooling", "pipeline")):
        # `just check` already contains every component gate.
        local = ["just check"]
    # Native artifacts and Canary prove the shipped binaries: the Controller
    # embeds the launcher, and the hosted Proxy bundles the Worker frontend.
    ships = any(c in components for c in ("core", "launcher", "web", "pipeline"))
    # Core and pipeline changes are never batched; urgent work is never batched.
    batch = not urgent and not {"core", "pipeline"} & set(components)
    return {
        "components": components,
        "apps": apps,
        "neutral_files": neutral,
        "unknown_paths_treated_as_core": [p for p in by_component.get("core", []) if not p.startswith(("crates/", "deploy/", "canary/", "native/"))],
        "local_gate": local,
        "ci": "exact-SHA CI (all jobs; CI is not path-filtered)",
        "native_build_and_canary": ships,
        "policy_install_check": ships,
        "app_owner_deploy": "apps" in components,
        "batchable": batch,
        "urgent": urgent,
    }


def changed(base, head):
    out = subprocess.run(
        ["git", "-C", str(ROOT), "diff", "--name-only", f"{base}", f"{head}"],
        check=True, capture_output=True, text=True,
    ).stdout
    return sorted(line for line in out.splitlines() if line)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base")
    parser.add_argument("head")
    parser.add_argument("--urgent", action="store_true", help="security fix or blocker: release alone, never batched")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    result = plan(changed(args.base, args.head), args.urgent)
    if args.json:
        print(json.dumps(result, indent=2))
        return 0
    print("components:", ", ".join(result["components"]) or "none")
    print("local gate:")
    for command in result["local_gate"]:
        print("  ", command)
    print("CI:", result["ci"])
    print("native build + canary + policy check:", "required" if result["native_build_and_canary"] else "not required")
    if result["app_owner_deploy"]:
        print("App deployment: by the App owner/leader after CI")
    print("batching:", "never (urgent)" if args.urgent else "may join a staging batch" if result["batchable"] else "release alone (core)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
