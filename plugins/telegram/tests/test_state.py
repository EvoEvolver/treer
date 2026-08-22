from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


PLUGIN_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN_ROOT))

from telegram_plugin.common import BridgeError  # noqa: E402
from telegram_plugin.state import StateStore  # noqa: E402


class StateStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.database = Path(self.temporary.name) / "telegram-state.sqlite3"
        self.state = StateStore(self.database)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_state_is_bound_to_one_bot_identity(self) -> None:
        self.state.bind_bot("bot-a")
        self.state.bind_bot("bot-a")

        with self.assertRaisesRegex(BridgeError, "different bot identity"):
            self.state.bind_bot("bot-b")

    def test_completing_an_update_advances_offset_and_commits_mapping(self) -> None:
        self.state.complete_update(
            42,
            bot_id="bot-a",
            chat_id="-1001",
            thread_id=7,
            telegram_message_id=99,
            core_message_id="msg-a",
            target_agent_id="agent-a",
            wake_requested=True,
        )

        self.assertEqual(self.state.last_update_id(), 42)
        self.assertTrue(self.state.update_completed(42))
        self.assertEqual(
            self.state.mapping_for_telegram("bot-a", "-1001", 7, 99),
            "msg-a",
        )
        wake = self.state.pending_wakes()
        self.assertEqual(len(wake), 1)
        self.assertEqual(wake[0]["core_message_id"], "msg-a")

    def test_outbound_intent_rejects_changed_delivery_content(self) -> None:
        self.state.ensure_outbound(
            "delivery-a",
            "message-a",
            "hash-a",
            "123",
            None,
            None,
            ["first"],
        )

        with self.assertRaisesRegex(BridgeError, "changed after Telegram intent"):
            self.state.ensure_outbound(
                "delivery-a",
                "message-a",
                "hash-b",
                "123",
                None,
                None,
                ["changed"],
            )


if __name__ == "__main__":
    unittest.main()
