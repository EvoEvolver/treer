import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
RETRY = SCRIPTS / "railway-retry.sh"


class RailwayRetryTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.counter = self.directory / "calls"

    def command(self, failures, stdout='{"error": "Problem processing request"}', code=1):
        """A fake CLI that fails `failures` times, printing `stdout`, then succeeds."""
        path = self.directory / "fake-railway"
        path.write_text(f"""#!/bin/sh
count=$(cat '{self.counter}' 2>/dev/null || echo 0)
count=$((count + 1))
echo $count > '{self.counter}'
if [ $count -le {failures} ]; then
    printf '%s' '{stdout}'
    exit {code}
fi
echo '{{"ok": true}}'
""")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return str(path)

    def run_retry(self, command, attempts="3", backoff="0"):
        return subprocess.run(["sh", str(RETRY), attempts, backoff, "--", command], capture_output=True, text=True,
                              env={**os.environ, "RAILWAY_API_TOKEN": "secrettokenvalue"})

    def calls(self):
        return int(self.counter.read_text()) if self.counter.exists() else 0

    def test_success_on_the_first_attempt_runs_once_and_passes_output_through(self):
        result = self.run_retry(self.command(0))
        self.assertEqual((result.returncode, self.calls()), (0, 1))
        self.assertIn('"ok": true', result.stdout)
        self.assertEqual(result.stderr, "")

    def test_a_transient_failure_is_retried_and_summarised_without_raw_output(self):
        result = self.run_retry(self.command(2))
        self.assertEqual((result.returncode, self.calls()), (0, 3))
        self.assertIn("railway attempt 1/3 failed (exit 1): Problem processing request", result.stderr)
        self.assertIn("railway attempt 2/3 failed", result.stderr)

    def test_persistent_failure_stops_at_the_attempt_limit_with_the_last_exit_code(self):
        result = self.run_retry(self.command(99, code=7), attempts="3")
        self.assertEqual((result.returncode, self.calls()), (7, 3))
        self.assertIn("attempt 3/3 failed (exit 7)", result.stderr)

    def test_a_single_attempt_never_retries(self):
        result = self.run_retry(self.command(99), attempts="1")
        self.assertEqual((result.returncode, self.calls()), (1, 1))

    def test_summaries_redact_tokens_and_identifiers(self):
        body = '{"error": "bad secrettokenvalue for 0a8e5754-50a5-4ab7-9929-96185e3da957 and abcdefghijklmnopqrstuvwxyz0123456789ABCD"}'
        result = self.run_retry(self.command(99, stdout=body), attempts="1")
        for leaked in ("secrettokenvalue", "0a8e5754", "abcdefghijklmnopqrstuvwxyz0123456789ABCD"):
            self.assertNotIn(leaked, result.stderr)

    def test_invalid_arguments_are_rejected_before_running_anything(self):
        for attempts, backoff in (("0", "0"), ("x", "0"), ("2", "-1"), ("2", "x")):
            result = self.run_retry(self.command(0), attempts=attempts, backoff=backoff)
            self.assertEqual(result.returncode, 2, (attempts, backoff))
        self.assertEqual(self.calls(), 0)


if __name__ == "__main__":
    unittest.main()
