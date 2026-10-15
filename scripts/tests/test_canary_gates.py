import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("canary_release_gates", SCRIPTS / "canary-release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

COMMIT = "a" * 40


def ci(identifier, status="completed", conclusion="success", event="push", sha=COMMIT):
    return {"databaseId": identifier, "status": status, "conclusion": conclusion, "event": event,
            "headSha": sha, "url": f"https://example.invalid/runs/{identifier}"}


class Clock:
    def __init__(self):
        self.now = 0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class WaitCiTest(unittest.TestCase):
    def wait(self, responses, timeout=60):
        clock = Clock()
        queue = list(responses)

        def fake_run(*args, **kwargs):
            self.assertEqual(args[:4], ("gh", "run", "list", "--workflow"))
            return json.dumps(queue.pop(0) if len(queue) > 1 else queue[0])

        with patch.object(release, "run", side_effect=fake_run):
            return release.wait_ci(Path("."), COMMIT, timeout=timeout, sleep=clock.sleep, clock=clock)

    def test_a_green_push_run_for_the_exact_commit_passes(self):
        self.assertIn("runs/7", self.wait([[ci(7)]]))

    def test_a_red_run_refuses_immediately_and_names_the_run(self):
        with self.assertRaisesRegex(RuntimeError, "CI failure.*runs/8.*no deployment started"):
            self.wait([[ci(8, conclusion="failure")]])

    def test_in_progress_then_green_waits_and_passes(self):
        self.assertIn("runs/9", self.wait([[ci(9, status="in_progress", conclusion=None)], [ci(9)]]))

    def test_no_run_or_never_finishing_run_times_out_without_deploying(self):
        with self.assertRaisesRegex(RuntimeError, "no successful CI run.*within 60s"):
            self.wait([[]])
        with self.assertRaisesRegex(RuntimeError, "no successful CI run"):
            self.wait([[ci(3, status="in_progress", conclusion=None)]])

    def test_pull_request_runs_and_other_commits_do_not_count(self):
        with self.assertRaisesRegex(RuntimeError, "no successful CI run"):
            self.wait([[ci(5, event="pull_request"), ci(6, sha="b" * 40)]])

    def test_only_the_latest_run_decides(self):
        with self.assertRaisesRegex(RuntimeError, "CI failure.*runs/11"):
            self.wait([[ci(10), ci(11, conclusion="failure")]])
        self.assertIn("runs/13", self.wait([[ci(12, conclusion="failure"), ci(13)]]))


def native(identifier, status="completed", conclusion="success", title=f"Machine artifacts {COMMIT}", sha="c" * 40):
    return {"databaseId": identifier, "status": status, "conclusion": conclusion, "displayTitle": title,
            "headSha": sha, "url": f"https://example.invalid/native/{identifier}"}


class NativeReadyTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.checkout = Path(self.temp.name)

    def check(self, runs, reuse_ok=False):
        calls = []

        def fake_run(*args, **kwargs):
            calls.append(args)
            if args[0] == "gh":
                return json.dumps(runs)
            if "reuse-machine-artifacts.sh" in args[1]:
                if not reuse_ok:
                    raise subprocess.CalledProcessError(1, args)
                return None
            return None

        with patch.object(release, "run", side_effect=fake_run):
            release.native_ready(self.checkout, COMMIT)
        return calls

    def test_a_successful_native_build_for_the_commit_passes(self):
        self.check([native(1)])

    def test_a_failed_or_running_native_build_refuses(self):
        for run in (native(2, conclusion="failure"), native(3, status="in_progress", conclusion=None)):
            with self.assertRaisesRegex(RuntimeError, "must be green"):
                self.check([run])

    def test_a_build_for_another_commit_does_not_count_and_needs_a_reuse_anchor(self):
        with self.assertRaisesRegex(RuntimeError, "no green native build and no verifiable reuse anchor"):
            self.check([native(4, title="Machine artifacts " + "d" * 40)])
        calls = self.check([native(4, title="Machine artifacts " + "d" * 40)], reuse_ok=True)
        self.assertTrue(any("--check-only" in call for call in calls))

    def test_no_build_at_all_passes_only_with_a_verified_reuse_anchor(self):
        with self.assertRaises(RuntimeError):
            self.check([])
        self.check([], reuse_ok=True)

    def test_an_existing_dist_is_verified_not_trusted(self):
        (self.checkout / "dist").mkdir()
        seen = []
        with patch.object(release, "run", side_effect=lambda *a, **k: seen.append(a)):
            release.native_ready(self.checkout, COMMIT)
        self.assertEqual(seen[0][:2], ("sh", "scripts/verify-machine-artifacts.sh"))
        with patch.object(release, "run", side_effect=subprocess.CalledProcessError(1, "verify")):
            with self.assertRaises(subprocess.CalledProcessError):
                release.native_ready(self.checkout, COMMIT)


if __name__ == "__main__":
    unittest.main()
