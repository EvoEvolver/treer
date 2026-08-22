from __future__ import annotations

import importlib.util
import json
import os
import socket
import sqlite3
import tempfile
import threading
import time
import unittest
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


APP_ROOT = Path(__file__).resolve().parents[1]
FAKE_TREER = APP_ROOT / "tests/fixtures/fake_treer.py"
SPEC = importlib.util.spec_from_file_location("treer_telegram_app", APP_ROOT / "telegram.py")
telegram = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = telegram
SPEC.loader.exec_module(telegram)


class FakeBotState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.updates: list[dict[str, Any]] = []
        self.sends: list[dict[str, Any]] = []
        self.behaviors: list[str] = []
        self.next_message_id = 101
        self.offsets: list[int] = []


class FakeBotServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], state: FakeBotState) -> None:
        self.state = state
        super().__init__(address, FakeBotHandler)


class FakeBotHandler(BaseHTTPRequestHandler):
    server: FakeBotServer

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length))
        method = self.path.rsplit("/", 1)[-1]
        if method == "getMe":
            self.reply(200, {"ok": True, "result": {"id": 999, "is_bot": True, "username": "treer_test_bot"}})
            return
        if method == "getUpdates":
            offset = int(payload["offset"])
            with self.server.state.lock:
                self.server.state.offsets.append(offset)
                updates = [item for item in self.server.state.updates if item["update_id"] >= offset]
            self.reply(200, {"ok": True, "result": updates})
            return
        if method == "sendMessage":
            with self.server.state.lock:
                behavior = self.server.state.behaviors.pop(0) if self.server.state.behaviors else "ok"
                if behavior == "429":
                    self.reply(
                        429,
                        {
                            "ok": False,
                            "error_code": 429,
                            "description": "Too Many Requests",
                            "parameters": {"retry_after": 0.01},
                        },
                    )
                    return
                message_id = self.server.state.next_message_id
                self.server.state.next_message_id += 1
                accepted = dict(payload)
                accepted["message_id"] = message_id
                self.server.state.sends.append(accepted)
            if behavior == "ambiguous":
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                return
            if behavior == "reject":
                self.reply(400, {"ok": False, "error_code": 400, "description": "Bad Request"})
                return
            self.reply(200, {"ok": True, "result": {"message_id": message_id, "chat": {"id": payload["chat_id"]}}})
            return
        self.reply(404, {"ok": False, "error_code": 404})

    def reply(self, status: int, value: dict[str, Any]) -> None:
        encoded = json.dumps(value).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format: str, *args: object) -> None:
        return


class TelegramBridgeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.cli_state = self.root / "fake-treer.json"
        self.cli_state.write_text("{}", encoding="utf-8")
        self.app_state = self.root / "app-state"
        self.app_state.mkdir()
        self.database = self.app_state / "telegram-state.sqlite3"
        FAKE_TREER.chmod(0o755)
        os.environ["FAKE_TREER_STATE"] = str(self.cli_state)
        os.environ["FAKE_TELEGRAM_ACK_GUARD_DB"] = str(self.database)
        self.bot_state = FakeBotState()
        self.bot_server = FakeBotServer(("127.0.0.1", 0), self.bot_state)
        self.bot_thread = threading.Thread(target=self.bot_server.serve_forever, daemon=True)
        self.bot_thread.start()
        self.config = telegram.Config(
            api_base_url=f"http://127.0.0.1:{self.bot_server.server_address[1]}",
            allowed_user_ids=frozenset({1234}),
            bindings=(
                telegram.Binding(
                    chat_id="-10042",
                    thread_id=9,
                    target_agent_id="agent-a",
                    wake_agent=True,
                ),
            ),
            poll_timeout_seconds=1,
            receive_wait_milliseconds=0,
            batch_size=20,
            retry_initial_seconds=0.01,
            retry_max_seconds=0.05,
            ambiguous_retry_seconds=0.01,
            respond_to_denied=True,
        )
        self.bridge = self.new_bridge()

    def tearDown(self) -> None:
        self.bot_server.shutdown()
        self.bot_server.server_close()
        self.bot_thread.join(timeout=2)
        os.environ.pop("FAKE_TREER_STATE", None)
        os.environ.pop("FAKE_TELEGRAM_ACK_GUARD_DB", None)
        os.environ.pop("FAKE_TREER_FAIL_ACK_DELIVERY", None)
        self.temporary.cleanup()

    def new_bridge(self) -> Any:
        state = telegram.StateStore(self.database)
        state.bind_bot("999")
        state.recover_sending(self.config.ambiguous_retry_seconds)
        return telegram.TelegramBridge(
            self.config,
            state,
            telegram.TreerCli(str(FAKE_TREER)),
            telegram.BotApi(self.config.api_base_url, "999:test", timeout_seconds=2),
            "999",
            self.app_state,
        )

    def inbound(self, update_id: int, message_id: int, text: str, *, reply_to: int | None = None, user_id: int = 1234) -> dict[str, Any]:
        message: dict[str, Any] = {
            "message_id": message_id,
            "message_thread_id": 9,
            "chat": {"id": -10042, "type": "supergroup"},
            "from": {"id": user_id, "is_bot": False, "username": "researcher"},
            "text": text,
        }
        if reply_to is not None:
            message["reply_to_message"] = {"message_id": reply_to}
        return {"update_id": update_id, "message": message}

    def add_delivery(self, delivery_id: str, message_id: str, body: str, contexts: list[str]) -> None:
        state = json.loads(self.cli_state.read_text(encoding="utf-8"))
        state.setdefault("deliveries", []).append(
            {
                "delivery_id": delivery_id,
                "message": {
                    "schema_version": 1,
                    "message_id": message_id,
                    "workspace_id": "workspace-a",
                    "sender": {"kind": "agent", "id": "agent-a", "name": "Builder"},
                    "recipients": [
                        {"kind": "agent", "id": "bridge-agent", "name": "Telegram"}
                    ],
                    "context_ids": contexts,
                    "body": body,
                    "created_at": "2026-08-20T12:00:00Z",
                },
                "recipient": {"kind": "agent", "id": "bridge-agent", "name": "Telegram"},
                "created_at": "2026-08-20T12:00:01Z",
                "acked": False,
            }
        )
        self.cli_state.write_text(json.dumps(state), encoding="utf-8")

    def cli_value(self) -> dict[str, Any]:
        return json.loads(self.cli_state.read_text(encoding="utf-8"))

    def test_reply_chain_round_trips_between_telegram_and_core_dag(self) -> None:
        first = self.inbound(1000, 100, "Please review the experiment")
        self.bridge.process_update(first)
        state = self.cli_value()
        self.assertEqual(len(state["messages"]), 1)
        m1 = state["messages"][0]
        self.assertEqual(m1["context_ids"], [])
        self.assertEqual(m1["external_source"]["message_id"], "100")
        self.assertEqual(m1["external_source"]["metadata"]["user_id"], "1234")
        self.assertNotIn("Please review", state["prompts"][0]["text"])
        self.assertIn(m1["message_id"], state["prompts"][0]["text"])

        self.bridge.process_update(first)
        self.assertEqual(len(self.cli_value()["messages"]), 1)

        self.add_delivery("dlv_m2", "msg_agent_reply", "Review complete", [m1["message_id"]])
        self.bridge.run_outbound_once(wait_milliseconds=0)
        sends = list(self.bot_state.sends)
        self.assertEqual(len(sends), 1)
        self.assertEqual(sends[0]["chat_id"], -10042)
        self.assertEqual(sends[0]["message_thread_id"], 9)
        self.assertEqual(sends[0]["reply_parameters"]["message_id"], 100)
        self.assertTrue(next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_m2")["acked"])

        third = self.inbound(1002, 102, "Can you clarify?", reply_to=101)
        self.bridge.process_update(third)
        m3 = self.cli_value()["messages"][-1]
        self.assertEqual(m3["context_ids"], ["msg_agent_reply"])

        restarted = self.new_bridge()
        restarted.process_update(third)
        self.assertEqual(len(self.cli_value()["messages"]), 2)
        self.bot_state.updates = [first, third]
        restarted.run_inbound_once(timeout=0)
        self.assertEqual(self.bot_state.offsets[-1], 1003)

    def test_unauthorized_updates_are_denied_before_treer(self) -> None:
        denied = self.inbound(2000, 200, "steal context", user_id=9876)
        self.bridge.process_update(denied)
        self.assertEqual(self.cli_value().get("messages", []), [])
        self.assertIn("not allowed", self.bot_state.sends[-1]["text"])
        self.assertEqual(self.bridge.state.last_update_id(), 2000)

    def test_long_multi_parent_delivery_splits_and_acks_after_every_mapping(self) -> None:
        self.bridge.process_update(self.inbound(3000, 300, "root"))
        root_id = self.cli_value()["messages"][0]["message_id"]
        self.bridge.process_update(self.inbound(3001, 301, "branch", reply_to=300))
        branch_id = self.cli_value()["messages"][1]["message_id"]
        body = "x" * 5000
        self.add_delivery("dlv_long", "msg_long", body, [branch_id, root_id])
        self.bridge.run_outbound_once(wait_milliseconds=0)
        outbound = self.bot_state.sends[-2:]
        self.assertEqual(len(outbound), 2)
        self.assertLessEqual(len(outbound[0]["text"]), 4096)
        self.assertLessEqual(len(outbound[1]["text"]), 4096)
        self.assertEqual(outbound[0]["reply_parameters"]["message_id"], 301)
        self.assertEqual(outbound[1]["reply_parameters"]["message_id"], outbound[0]["message_id"])
        self.assertIn("2 context nodes", outbound[1]["text"])
        delivery = next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_long")
        self.assertTrue(delivery["acked"])

    def test_rate_limit_and_ambiguous_send_recover_without_early_ack(self) -> None:
        self.bridge.process_update(self.inbound(4000, 400, "root"))
        root_id = self.cli_value()["messages"][0]["message_id"]

        self.add_delivery("dlv_rate", "msg_rate", "rate limited", [root_id])
        self.bot_state.behaviors.append("429")
        self.bridge.run_outbound_once(wait_milliseconds=0)
        rate = next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_rate")
        self.assertFalse(rate["acked"])
        time.sleep(0.06)
        self.bridge.run_outbound_once(wait_milliseconds=0)
        self.assertTrue(next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_rate")["acked"])

        self.add_delivery("dlv_ambiguous", "msg_ambiguous", "ambiguous", [root_id])
        self.bot_state.behaviors.append("ambiguous")
        self.bridge.run_outbound_once(wait_milliseconds=0)
        ambiguous = next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_ambiguous")
        self.assertFalse(ambiguous["acked"])
        connection = sqlite3.connect(self.database)
        count = connection.execute(
            "SELECT ambiguity_count FROM outbound_deliveries WHERE delivery_id = 'dlv_ambiguous'"
        ).fetchone()[0]
        connection.close()
        self.assertEqual(count, 1)
        accepted_before_retry = len(self.bot_state.sends)
        time.sleep(0.02)
        restarted = self.new_bridge()
        restarted.run_outbound_once(wait_milliseconds=0)
        self.assertEqual(len(self.bot_state.sends), accepted_before_retry + 1)
        self.assertTrue(next(item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_ambiguous")["acked"])

    def test_crashes_at_each_commit_boundary_replay_without_losing_core_state(self) -> None:
        update = self.inbound(5000, 500, "persist me")
        complete_update = self.bridge.state.complete_update

        def crash_before_mapping(*_args: object, **_kwargs: object) -> None:
            raise RuntimeError("injected crash after Core send")

        self.bridge.state.complete_update = crash_before_mapping
        with self.assertRaisesRegex(RuntimeError, "after Core send"):
            self.bridge.process_update(update)
        self.bridge.state.complete_update = complete_update
        self.assertEqual(len(self.cli_value()["messages"]), 1)
        self.bridge.process_update(update)
        self.assertEqual(len(self.cli_value()["messages"]), 1)
        root_id = self.cli_value()["messages"][0]["message_id"]

        self.add_delivery("dlv_crash_send", "msg_crash_send", "send after restart", [root_id])
        send_message = self.bridge.bot.send_message

        def crash_during_send(*_args: object, **_kwargs: object) -> dict[str, Any]:
            raise KeyboardInterrupt("injected process crash")

        self.bridge.bot.send_message = crash_during_send
        delivery = next(
            item for item in self.cli_value()["deliveries"] if item["delivery_id"] == "dlv_crash_send"
        )
        with self.assertRaises(KeyboardInterrupt):
            self.bridge.process_delivery(delivery)
        self.bridge.bot.send_message = send_message
        connection = sqlite3.connect(self.database)
        status = connection.execute(
            "SELECT status FROM outbound_chunks WHERE delivery_id = 'dlv_crash_send'"
        ).fetchone()[0]
        connection.close()
        self.assertEqual(status, "sending")
        restarted = self.new_bridge()
        time.sleep(0.02)
        restarted.run_outbound_once(wait_milliseconds=0)
        self.assertTrue(
            next(
                item
                for item in self.cli_value()["deliveries"]
                if item["delivery_id"] == "dlv_crash_send"
            )["acked"]
        )

        self.add_delivery("dlv_ack_retry", "msg_ack_retry", "map before ack", [root_id])
        os.environ["FAKE_TREER_FAIL_ACK_DELIVERY"] = "dlv_ack_retry"
        sends_before = len(self.bot_state.sends)
        restarted.run_outbound_once(wait_milliseconds=0)
        sends_after_failure = len(self.bot_state.sends)
        self.assertEqual(sends_after_failure, sends_before + 1)
        self.assertFalse(
            next(
                item
                for item in self.cli_value()["deliveries"]
                if item["delivery_id"] == "dlv_ack_retry"
            )["acked"]
        )
        os.environ.pop("FAKE_TREER_FAIL_ACK_DELIVERY")
        restarted.run_outbound_once(wait_milliseconds=0)
        self.assertEqual(len(self.bot_state.sends), sends_after_failure)
        self.assertTrue(
            next(
                item
                for item in self.cli_value()["deliveries"]
                if item["delivery_id"] == "dlv_ack_retry"
            )["acked"]
        )


if __name__ == "__main__":
    unittest.main()
