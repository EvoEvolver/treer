import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cli_contract  # noqa: E402


class CliContractTests(unittest.TestCase):
    def test_known_calls_pass(self) -> None:
        for argv in (
            ["message", "send", "--to", "a", "--body", "x", "--external-source-file", "-"],
            ["message", "send", "--to", "a", "--body-file", "-"],
            ["message", "receive", "--limit", "5"],
            ["message", "send", "--to", "a", "--body=-1", "--external-source-file", "-"],
            ["message", "send", "--to", "a", "--body", "-"],
            ["message", "send", "--to", "a", "--body=-x"],
            ["agent", "show", "/infra/x"],
            ["--workspace", "w", "agent", "show", "x"],
            ["exec", "--machine", "m", "--", "ls", "-la"],
        ):
            self.assertIsNone(cli_contract.violation(argv), argv)

    def test_drift_is_rejected(self) -> None:
        for argv in (
            ["agent", "get", "x"],  # the /status regression: no such subcommand
            ["message", "send", "--to", "a", "--no-such-flag", "x"],
            ["message", "send", "--to"],
            ["message", "send", "--to", "a", "--body", "-1"],
            ["message", "send", "--to", "a", "--body", "-x"],
            ["message", "send", "--to", "a", "--body", "--help"],
            ["nonsense"],
        ):
            self.assertIsNotNone(cli_contract.violation(argv), argv)


if __name__ == "__main__":
    unittest.main()
