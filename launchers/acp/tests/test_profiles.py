import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class AcpLauncherProfilesTest(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads((ROOT / "profiles.json").read_text())
        self.profiles = self.manifest["profiles"]

    def test_every_provider_has_separate_headless_and_ui_profiles(self):
        providers = {profile["provider"] for profile in self.profiles}
        self.assertEqual(providers, {"grok", "cursor", "codex", "claude", "opencode"})
        for provider in providers:
            variants = {
                profile["presentation"]: profile
                for profile in self.profiles
                if profile["provider"] == provider
            }
            self.assertEqual(set(variants), {"headless", "remote-codex-ui"})
            self.assertNotIn("--ui", variants["headless"]["run"]["args"])
            self.assertEqual(
                variants["remote-codex-ui"]["run"]["args"][-2:],
                ["--ui", "remote-codex"],
            )

    def test_profiles_use_only_the_ordinary_launcher_command(self):
        for profile in self.profiles:
            self.assertEqual(
                profile["run"]["command"],
                "./launchers/acp/scripts/treer-agent.sh",
            )
            self.assertEqual(profile["run"]["args"][:2], ["--harness", profile["provider"]])
            args = profile["run"]["args"]
            self.assertIn("--base-command", args)
            self.assertIn("--server-command", args)
            self.assertTrue(args[args.index("--base-command") + 1])
            self.assertTrue(args[args.index("--server-command") + 1])

    def test_apply_list_needs_no_treer_or_build_tools(self):
        result = subprocess.run(
            [str(ROOT / "scripts/apply.sh"), "--list"],
            check=True,
            capture_output=True,
            text=True,
            env={"PATH": "/usr/bin:/bin"},
        )
        self.assertEqual(
            result.stdout.splitlines(),
            ["claude", "codex", "cursor", "grok", "opencode"],
        )

    def test_remote_ui_is_pinned_to_an_immutable_commit(self):
        lock = json.loads((ROOT / "optional-ui/remote-codex.lock.json").read_text())
        self.assertRegex(lock["commit"], r"^[0-9a-f]{40}$")
        self.assertNotIn("ref", lock)

    def test_profile_launch_requires_a_resolved_machine(self):
        result = subprocess.run(
            [
                str(ROOT / "scripts/install_profiles.py"),
                str(ROOT / "profiles.json"),
                "--agent",
                "codex",
                "--repo-cwd",
                ".",
                "--launch",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--machine is required with --launch", result.stderr)


if __name__ == "__main__":
    unittest.main()
