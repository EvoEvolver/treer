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

    WORKFLOW = "jobs:\n  windows:\n    name: Build\n  windows-launcher-smoke:\n    name: Native Windows embedded launcher and ACP UI\n"
    GREEN_JOB = {"name": "Native Windows embedded launcher and ACP UI", "status": "completed", "conclusion": "success"}

    def check(self, runs, reuse_ok=False, verify_ok=True, jobs=None, workflow=None, view=None):
        calls = []
        jobs = [self.GREEN_JOB] if jobs is None else jobs
        workflow = self.WORKFLOW if workflow is None else workflow

        def fake_run(*args, **kwargs):
            calls.append(args)
            if args[0] == "gh" and args[2] == "view":
                receipt = {"status": "completed", "conclusion": "success", "headSha": COMMIT,
                           "displayTitle": f"Machine artifacts {COMMIT}", "jobs": jobs}
                return json.dumps(view or receipt)
            if args[0] == "gh":
                return json.dumps(runs)
            if args[0] == "git":
                return workflow
            failing = (("reuse-machine-artifacts.sh" in args[1] and not reuse_ok) or
                       ("verify-machine-artifacts.sh" in args[1] and not verify_ok))
            if failing:
                raise subprocess.CalledProcessError(1, args)
            return None

        with patch.object(release, "run", side_effect=fake_run):
            release.native_ready(self.checkout, COMMIT)
        return calls

    def make_dist(self, provenance=False):
        (self.checkout / "dist").mkdir()
        if provenance:
            (self.checkout / "dist" / "artifact-provenance.json").write_text("{}")

    def test_a_successful_native_build_for_the_commit_passes(self):
        self.check([native(1)])

    def test_a_failed_or_running_native_build_refuses(self):
        for run in (native(2, conclusion="failure"), native(3, status="in_progress", conclusion=None)):
            with self.assertRaisesRegex(RuntimeError, "must be green"):
                self.check([run])

    def test_the_windows_launcher_job_must_exist_once_and_be_green_when_the_workflow_defines_it(self):
        failing = dict(self.GREEN_JOB, conclusion="failure")
        skipped = dict(self.GREEN_JOB, conclusion="skipped")
        for jobs in ([], [failing], [skipped], [self.GREEN_JOB, self.GREEN_JOB], [dict(self.GREEN_JOB, status="in_progress", conclusion=None)]):
            with self.subTest(jobs=jobs):
                with self.assertRaisesRegex(RuntimeError, "Windows launcher/UI acceptance did not pass"):
                    self.check([native(30)], jobs=jobs)
        self.check([native(30)], jobs=[self.GREEN_JOB])

    def test_an_old_workflow_without_the_windows_job_does_not_require_it(self):
        self.check([native(31)], jobs=[], workflow="jobs:\n  windows:\n    name: Build\n")

    def test_the_run_receipt_must_belong_to_the_candidate_and_be_green(self):
        other = {"status": "completed", "conclusion": "success", "headSha": "e" * 40,
                 "displayTitle": "Machine artifacts " + "e" * 40, "jobs": [self.GREEN_JOB]}
        with self.assertRaisesRegex(RuntimeError, "another candidate"):
            self.check([native(32)], view=other)
        with self.assertRaisesRegex(RuntimeError, "another candidate"):
            self.check([native(33)], view=dict(other, headSha=COMMIT, displayTitle="x", conclusion="failure"))

    def test_an_existing_dist_also_goes_through_the_windows_acceptance(self):
        self.make_dist()
        with self.assertRaisesRegex(RuntimeError, "Windows launcher/UI acceptance did not pass"):
            self.check([native(34)], jobs=[])

    def test_a_dispatch_on_this_ref_that_built_another_revision_does_not_count(self):
        # headSha matches the candidate but the title says revision B was built.
        wrong = native(4, title="Machine artifacts " + "d" * 40, sha=COMMIT)
        with self.assertRaisesRegex(RuntimeError, "no green native build and no verifiable reuse anchor"):
            self.check([wrong])
        calls = self.check([wrong], reuse_ok=True)
        self.assertTrue(any("--check-only" in call for call in calls))

    def test_an_old_run_without_the_title_prefix_matches_on_head_sha(self):
        self.check([native(5, title="Build release artifacts", sha=COMMIT)])

    def test_no_build_at_all_passes_only_with_a_verified_reuse_anchor(self):
        with self.assertRaises(RuntimeError):
            self.check([])
        self.check([], reuse_ok=True)

    def test_a_local_dist_without_a_native_run_or_provenance_is_not_evidence(self):
        self.make_dist()
        with self.assertRaisesRegex(RuntimeError, "not evidence of a green native build"):
            self.check([])
        for run in (native(6, conclusion="failure"), native(7, status="in_progress", conclusion=None)):
            with self.assertRaisesRegex(RuntimeError, "must be green"):
                self.check([run])

    def test_a_local_dist_with_a_green_native_run_is_still_verified(self):
        self.make_dist()
        calls = self.check([native(8)])
        self.assertTrue(any("verify-machine-artifacts.sh" in call[1] for call in calls))
        with self.assertRaises(subprocess.CalledProcessError):
            self.check([native(8)], verify_ok=False)

    def test_a_reused_dist_must_verify_against_the_recorded_release_and_needs_no_gh(self):
        self.make_dist(provenance=True)
        calls = self.check([], reuse_ok=True)
        self.assertEqual(len(calls), 1)
        self.assertIn("--verify-dist", calls[0])
        self.assertFalse(any(call[0] == "gh" for call in calls))
        with self.assertRaises(subprocess.CalledProcessError):
            self.check([], reuse_ok=False)


class RunTimeoutTest(unittest.TestCase):
    def test_gh_calls_are_time_bounded_but_other_commands_are_not(self):
        seen = {}

        def fake(args, **kwargs):
            seen[args[0]] = kwargs.get("timeout")
            return subprocess.CompletedProcess(args, 0, stdout="")

        with patch.object(release.subprocess, "run", side_effect=fake):
            release.run("gh", "run", "list", cwd=Path("."), capture=True)
            release.run("git", "status", cwd=Path("."), capture=True)
        self.assertEqual(seen["gh"], release.GH_TIMEOUT)
        self.assertIsNone(seen["git"])


class EntrypointTest(unittest.TestCase):
    """Run the real script with a fake gh on PATH."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.repo = base / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "scripts" / "canary-release.py").write_text((SCRIPTS / "canary-release.py").read_text())
        git = ["git", "-C", str(self.repo)]
        subprocess.run([*git, "init", "-q"], check=True)
        subprocess.run([*git, "-c", "user.name=t", "-c", "user.email=t@example.invalid", "add", "-A"], check=True)
        subprocess.run([*git, "-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "c"], check=True)
        self.commit = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        self.bin = base / "bin"
        self.bin.mkdir()
        self.marker = base / "gh-called"

    def install_gh(self, payload):
        gh = self.bin / "gh"
        gh.write_text(f"#!/bin/sh\necho called >> '{self.marker}'\ncat <<'EOF'\n{json.dumps(payload)}\nEOF\n")
        gh.chmod(0o755)

    def invoke(self, *flags):
        import os
        return subprocess.run(["python3", "scripts/canary-release.py", "HEAD", *flags], cwd=self.repo, capture_output=True,
                              text=True, env={**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}"})

    def test_check_ci_passes_on_a_green_exact_commit_run_through_the_real_entrypoint(self):
        self.install_gh([ci(21, sha=self.commit)])
        result = self.invoke("--check-ci")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Exact-commit CI passed", result.stdout)

    def test_check_ci_fails_on_a_red_run_and_dry_run_cannot_be_combined_with_it(self):
        self.install_gh([ci(22, conclusion="failure", sha=self.commit)])
        red = self.invoke("--check-ci")
        self.assertEqual(red.returncode, 1)
        self.assertIn("Canary release failed: CI failure", red.stderr)
        self.marker.unlink()
        combined = self.invoke("--check-ci", "--dry-run")
        self.assertEqual(combined.returncode, 2)
        self.assertFalse(self.marker.exists(), "dry-run must not reach gh")


class TimingTest(unittest.TestCase):
    def test_laps_record_seconds_since_the_previous_lap_in_both_implementations(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "timings.tsv"
            self.assertIsNone(release.lap("ignored", None))
            Path(str(path) + ".last").write_text("100\n")
            with patch.object(release.time, "time", return_value=107.9):
                release.lap("ci_wait", path)
            self.assertEqual(path.read_text(), "ci_wait\t7\n")
            self.assertEqual(Path(str(path) + ".last").read_text(), "107\n")
            shell = subprocess.run(
                ["sh", "-c", '. "$1"; lap native_ready; lap deploy_total', "timing-test",
                 str(SCRIPTS / "release-timing.sh")],
                env={"PATH": "/usr/bin:/bin", "TREER_TIMINGS_FILE": str(path)}, capture_output=True, text=True)
            self.assertEqual(shell.returncode, 0, shell.stderr)
            rows = [line.split("\t") for line in path.read_text().splitlines()]
            self.assertEqual([row[0] for row in rows], ["ci_wait", "native_ready", "deploy_total"])
            self.assertTrue(all(row[1].isdigit() for row in rows))
            self.assertGreater(int(rows[1][1]), 1_000_000)  # first shell lap measures from the python lap (107s epoch)

    def test_lap_is_a_no_op_without_a_timings_file_and_never_fails_the_pipeline(self):
        shell = subprocess.run(["sh", "-c", 'set -eu; . "$1"; lap anything; echo done', "timing-test",
                                str(SCRIPTS / "release-timing.sh")],
                               env={"PATH": "/usr/bin:/bin"}, capture_output=True, text=True)
        self.assertEqual((shell.returncode, shell.stdout.strip()), (0, "done"))

    def test_the_manifest_records_the_stage_timings_from_the_pipeline_script(self):
        script = (SCRIPTS / "release-canary.sh").read_text()
        self.assertIn("timings_seconds: $timings", script)
        for stage in ("local_gate", "deploy_total", "canary_e2e_total"):
            self.assertIn(f"lap {stage}", script)


if __name__ == "__main__":
    unittest.main()
