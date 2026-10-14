import json
import subprocess
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "railway-error-summary.sh"


def summarize(payload: str, env: dict | None = None) -> str:
    import os
    return subprocess.run(["sh", str(SCRIPT)], input=payload, capture_output=True, text=True, check=True,
                          env={**os.environ, **(env or {})}).stdout


class RailwayErrorSummaryTest(unittest.TestCase):
    def test_keeps_the_error_fields_and_nothing_else(self):
        text = summarize(json.dumps({"error": "build rejected", "status": "FAILED", "project": "p-123", "url": "https://x"}))
        self.assertEqual(text.strip(), "build rejected FAILED")

    def test_strips_control_characters_and_long_tokens_and_bounds_the_length(self):
        token = "A" * 48
        text = summarize(json.dumps({"message": "bad\x1b[31m\nline " + token + " " + "word " * 200}))
        self.assertNotIn("\x1b", text)
        self.assertNotIn(token, text)
        self.assertIn("[redacted]", text)
        self.assertLessEqual(len(text.strip()), 300)

    def test_replaces_uuid_shaped_credentials_and_exact_railway_token_values(self):
        uuid = "123e4567-e89b-12d3-a456-426614174000"
        short = "tok-123"
        text = summarize(json.dumps({"error": "denied for " + uuid + " using " + short + " twice " + short}), {"RAILWAY_API_TOKEN": short})
        self.assertNotIn(uuid, text)
        self.assertNotIn(short, text)
        self.assertGreaterEqual(text.count("[redacted]"), 3)

    def test_nested_objects_are_not_printed(self):
        text = summarize(json.dumps({"error": {"detail": "secret-ish nested body"}, "status": "FAILED"}))
        self.assertEqual(text.strip(), "FAILED")

    def test_non_json_output_prints_nothing(self):
        self.assertEqual(summarize("<html>502</html>").strip(), "")


if __name__ == "__main__":
    unittest.main()
