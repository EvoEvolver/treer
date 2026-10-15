from importlib import util
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
spec = util.spec_from_file_location("release_tickets", SCRIPTS / "release-tickets.py")
tickets = util.module_from_spec(spec)
spec.loader.exec_module(tickets)

FAKE_CLI = '''
import json, sys
from pathlib import Path
log = Path(sys.argv[0]).with_name("calls.json")
calls = json.loads(log.read_text()) if log.exists() else []
calls.append(sys.argv[1:])
log.write_text(json.dumps(calls))
print(json.dumps({"issue": {"id": 100 + len(calls)}}))
'''


class ReleaseTicketsTest(unittest.TestCase):
    def arguments(self, *extra, cli=None):
        return ["--name", "batch X", "--parent", "20", "--release-owner", "me",
                "--review", "core=core1", "--review", "security=sec1",
                "--acceptance", "leader=lead1", *extra, *(["--issues-cli", cli] if cli else [])]

    def test_dry_run_prints_commands_without_running_anything(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            tickets.main(self.arguments("--issues-cli", "/nonexistent/cli"))
        lines = out.getvalue().splitlines()
        self.assertEqual(len(lines), 4)
        self.assertIn("<review:core>", lines[2])
        self.assertIn("<review:security>", lines[2])
        self.assertIn("<release>", lines[3])

    def test_create_orders_reviews_then_release_then_acceptance_with_blockers(self):
        with tempfile.TemporaryDirectory() as directory:
            cli = Path(directory) / "cli.py"
            cli.write_text(FAKE_CLI)
            with contextlib.redirect_stdout(io.StringIO()):
                tickets.main(self.arguments("--create", cli=str(cli)))
            calls = json.loads((Path(directory) / "calls.json").read_text())
        self.assertEqual([c[1].split("]")[0] for c in calls], ["[Review", "[Review", "[Release", "[Acceptance"])
        release, acceptance = calls[2], calls[3]
        self.assertEqual([release[i + 1] for i, v in enumerate(release) if v == "--blocked-by"], ["101", "102"])
        self.assertEqual([acceptance[i + 1] for i, v in enumerate(acceptance) if v == "--blocked-by"], ["103"])
        self.assertIn("type:release", release)
        for call in calls:
            self.assertEqual(call[call.index("--parent-id") + 1], "20")

    def test_bad_pair_is_rejected(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            tickets.main(["--name", "x", "--parent", "1", "--release-owner", "me", "--review", "nobody"])


if __name__ == "__main__":
    unittest.main()
