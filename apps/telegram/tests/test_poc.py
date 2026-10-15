from __future__ import annotations

import json
import os
import sqlite3
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any

import test_telegram as base
from telegram_app.common import BridgeError

telegram = base.telegram
APP_ROOT = base.APP_ROOT
FAKE_TREER = base.FAKE_TREER


class PocBase(unittest.TestCase):
    """A bridge with a fake Bot API and fake CLI; the registry is seeded from `config`."""
    limits: dict[str, Any] = {}
    admins: frozenset[str] = frozenset()

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
        os.environ["FAKE_TREER_PATHS"] = json.dumps({"/team/builder": "ag_builder", "/team/other": "ag_other"})
        os.environ["FAKE_TREER_UNKNOWN_AGENTS"] = "ag_missing"
        self.bot_state = base.FakeBotState()
        self.bot_server = base.FakeBotServer(("127.0.0.1", 0), self.bot_state)
        threading.Thread(target=self.bot_server.serve_forever, daemon=True).start()
        self.bridge = self.new_bridge()

    def tearDown(self) -> None:
        self.bot_server.shutdown()
        self.bot_server.server_close()
        for name in ("FAKE_TREER_STATE", "FAKE_TREER_PATHS", "FAKE_TREER_UNKNOWN_AGENTS"):
            os.environ.pop(name, None)
        self.temporary.cleanup()

    def make_config(self, **overrides: Any) -> Any:
        values: dict[str, Any] = dict(
            api_base_url=f"http://127.0.0.1:{self.bot_server.server_address[1]}",
            allowed_user_ids=frozenset({1234}),
            bindings=(telegram.Binding(chat_id="-10042", thread_id=9, target_agent_id="agent-a", wake_agent=False),),
            poll_timeout_seconds=1, receive_wait_milliseconds=0, batch_size=20, retry_initial_seconds=0.01,
            retry_max_seconds=0.05, ambiguous_retry_seconds=0.01, respond_to_denied=True,
            user_labels=((1234, "researcher"),), user_admin_agent_ids=self.admins,
            **self.limits,
        )
        values.update(overrides)
        return telegram.Config(**values)

    def new_bridge(self, **overrides: Any) -> Any:
        config = self.make_config(**overrides)
        state = telegram.StateStore(self.database)
        state.bind_bot("999")
        return telegram.TelegramBridge(config, state, telegram.TreerCli(str(FAKE_TREER)),
                                       telegram.BotApi(config.api_base_url, "999:test", timeout_seconds=2), "999", self.app_state)

    update_counter = 0

    def say(self, text: str, *, chat: int = -10042, thread: int | None = 9, user: int = 1234) -> None:
        PocBase.update_counter += 1
        n = PocBase.update_counter
        message: dict[str, Any] = {"message_id": 1000 + n, "chat": {"id": chat, "type": "supergroup"},
                                   "from": {"id": user, "is_bot": False}, "text": text}
        if thread is not None:
            message["message_thread_id"] = thread
        self.bridge.process_update({"update_id": n, "message": message})

    def cli(self) -> dict[str, Any]:
        return json.loads(self.cli_state.read_text(encoding="utf-8"))

    def core_sends(self) -> list[dict[str, Any]]:
        return [m for m in self.cli().get("messages", []) if m.get("external_source")]

    def notices(self) -> list[str]:
        return [item["text"] for item in self.bot_state.sends]

    def control(self, sender: str, body: dict[str, Any] | str, delivery: str | None = None, kind: str = "agent") -> str:
        delivery = delivery or f"d{time.monotonic_ns()}"
        state = self.cli()
        state.setdefault("deliveries", []).append({
            "delivery_id": delivery,
            "message": {"schema_version": 1, "message_id": f"m-{delivery}", "workspace_id": "w", "sender": {"kind": kind, "id": sender, "name": sender},
                        "recipients": [{"kind": "agent", "id": "bridge-agent"}], "context_ids": [],
                        "body": body if isinstance(body, str) else json.dumps(body), "created_at": "2026-10-15T00:00:00Z"},
            "recipient": {"kind": "agent", "id": "bridge-agent"}, "created_at": "2026-10-15T00:00:01Z", "acked": False})
        self.cli_state.write_text(json.dumps(state), encoding="utf-8")
        self.bridge.run_outbound_once(0)
        return delivery

    def result(self, delivery: str) -> dict[str, Any]:
        sent = [m for m in self.cli()["messages"] if m["context_ids"] == [f"m-{delivery}"]]
        self.assertEqual(len(sent), 1, "exactly one result message per control delivery")
        self.assertEqual(sent[0]["body"][:1], "{")
        return json.loads(sent[0]["body"]), sent[0]

    def audit(self) -> list[dict[str, Any]]:
        return self.bridge.state.audit_list("telegram", "999", 50)


class ImPocCommandTest(PocBase):
    def test_registry_is_seeded_from_config_and_then_authoritative(self) -> None:
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-10042:9").target_agent_id, "agent-a")
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "1234"), ("researcher", True))
        self.say("/poc /team/builder")
        self.bridge = self.new_bridge()          # restart with the ORIGINAL config: the change survives
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-10042:9").target_agent_id, "ag_builder")

    def test_show_set_off_on_from_the_chat_and_messages_follow_the_poc(self) -> None:
        self.say("/poc")
        self.assertIn("POC for this chat: agent-a (on)", self.notices()[-1])
        self.say("/poc /team/builder")
        self.assertIn("now ag_builder", self.notices()[-1])
        self.say("hello builder")
        (sent,) = self.core_sends()
        self.assertEqual(sent["recipients"][0]["id"], "ag_builder")
        self.assertEqual(sent["body"], "hello builder", "the body is passed through untouched")
        self.assertEqual(sent["external_source"]["metadata"]["user_label"], "researcher")
        self.say("/poc off")
        self.say("anyone there?")
        self.assertIn("switched off", self.notices()[-1])
        self.assertEqual(len(self.core_sends()), 1, "nothing is sent to a paused POC")
        self.say("/poc on")
        self.say("back")
        self.assertEqual(len(self.core_sends()), 2)
        events = self.audit()
        self.assertEqual([e["op"] for e in reversed(events)], ["poc.set", "poc.off", "poc.on"])
        self.assertTrue(all(e["actor"] == "telegram:1234(researcher)" and e["via"] == "telegram" for e in events))
        self.assertEqual((events[2]["old_target"], events[2]["new_target"]), ("agent-a", "ag_builder"))

    def test_unknown_targets_are_refused_and_not_audited_as_changes(self) -> None:
        self.say("/poc ag_missing")
        self.assertIn("Could not change the POC", self.notices()[-1])
        self.say("/poc two words")
        self.assertIn("Could not change the POC", self.notices()[-1])
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-10042:9").target_agent_id, "agent-a")
        self.assertEqual(self.audit(), [])

    def test_an_unbound_chat_can_get_a_poc_only_from_an_admitted_user(self) -> None:
        self.say("hello", chat=-777, thread=None)
        self.assertIn("not allowed", self.notices()[-1])
        self.say("/poc ag_fresh", chat=-777, thread=None, user=9999)         # not admitted
        self.assertIn("not allowed", self.notices()[-1])
        self.assertIsNone(self.bridge.state.get_binding("telegram", "999", "-777:0"))
        self.say("/poc ag_fresh", chat=-777, thread=None)
        self.assertIn("now ag_fresh", self.notices()[-1])
        self.say("hello", chat=-777, thread=None)
        self.assertEqual(self.core_sends()[-1]["recipients"][0]["id"], "ag_fresh")
        self.say("/poc on", chat=-888, thread=None)
        self.assertIn("no point of contact yet", self.notices()[-1])


class ControlMessageTest(PocBase):
    admins = frozenset({"ag_admin"})

    def test_any_agent_can_read_and_change_a_poc_and_the_result_comes_back_as_a_message(self) -> None:
        d = self.control("agent-x", {"im": "poc.get", "conversation": "-10042:9"})
        body, message = self.result(d)
        self.assertTrue(body["ok"])
        self.assertEqual(body["binding"]["target_agent_id"], "agent-a")
        self.assertEqual(message["recipients"][0]["id"], "agent-x")
        self.assertEqual(self.bot_state.sends, [], "control messages are never relayed to the chat")
        d = self.control("agent-x", {"im": "poc.set", "conversation": "-10042:9", "to": "/team/builder", "wake_agent": True})
        body, _ = self.result(d)
        self.assertEqual((body["binding"]["target_agent_id"], body["binding"]["wake_agent"]), ("ag_builder", True))
        self.assertEqual(self.audit()[0]["actor"], "agent:agent-x")
        self.assertEqual(self.audit()[0]["via"], "control")
        self.assertTrue(self.cli()["deliveries"][-1]["acked"])

    def test_set_creates_new_bindings_and_enable_disable(self) -> None:
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-555", "to": "ag_new"}))
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-555:0").target_agent_id, "ag_new")
        body, _ = self.result(self.control("agent-x", {"im": "poc.disable", "conversation": "-555"}))
        self.assertFalse(body["binding"]["enabled"])
        body, _ = self.result(self.control("agent-x", {"im": "poc.enable", "conversation": "-555:0"}))
        self.assertTrue(body["binding"]["enabled"])
        body, _ = self.result(self.control("agent-x", {"im": "poc.disable", "conversation": "-404"}))
        self.assertEqual(body["error"]["code"], "no_binding")

    def test_a_poc_can_hand_its_conversations_to_another_agent(self) -> None:
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-20", "to": "agent-a"}))
        body, _ = self.result(self.control("agent-a", {"im": "poc.handoff", "conversation": "-10042:9", "to": "/team/other"}))
        self.assertEqual((body["ok"], body["handed_over"], body["to"]), (True, ["-10042:9"], "ag_other"))
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-20:0").target_agent_id, "agent-a", "only the named conversation moved")
        body, _ = self.result(self.control("agent-a", {"im": "poc.handoff", "to": "ag_other"}))
        self.assertEqual(body["handed_over"], ["-20:0"], "without a conversation: everything it still serves")
        body, _ = self.result(self.control("agent-a", {"im": "poc.handoff", "to": "ag_other"}))
        self.assertEqual(body["error"]["code"], "not_poc")
        self.assertEqual(self.audit()[0]["op"], "poc.handoff")

    def test_errors_are_returned_not_raised(self) -> None:
        cases = [({"im": "poc.nope"}, "unknown_op"), ({"im": "poc.get"}, "invalid_conversation"),
                 ({"im": "poc.get", "conversation": "abc"}, "invalid_conversation"),
                 ({"im": "poc.set", "conversation": "-1", "to": "ag_missing"}, "target_unavailable"),
                 ({"im": "poc.set", "conversation": "-1", "to": "has space"}, "invalid_target"),
                 ({"im": "poc.set", "conversation": "-1", "to": "ag_ok", "wake_agent": "yes"}, "invalid_field")]
        for body, code in cases:
            result, _ = self.result(self.control("agent-x", body))
            self.assertEqual((result["ok"], result["error"]["code"]), (False, code), body)
        self.assertEqual(len(self.bridge.state.list_bindings("telegram", "999")), 1)

    def test_a_redelivered_control_message_is_applied_once(self) -> None:
        body = {"im": "poc.set", "conversation": "-10042:9", "to": "ag_once"}
        d = self.control("agent-x", body, delivery="dup1")
        self.result(d)
        state = self.cli()
        for delivery in state["deliveries"]:
            delivery["acked"] = False                       # Core redelivers: our ack was lost
        self.cli_state.write_text(json.dumps(state), encoding="utf-8")
        self.bridge.run_outbound_once(0)
        self.assertEqual(len([e for e in self.audit() if e["op"] == "poc.set"]), 1)
        self.assertEqual(len([m for m in self.cli()["messages"] if m["context_ids"] == ["m-dup1"]]), 1, "the result is replayed under the same idempotency key")

    def test_plain_and_non_control_json_bodies_are_relayed_normally(self) -> None:
        self.say("hello")                                   # creates mapping so a context reply can route
        mapped = self.core_sends()[0]["message_id"]
        state = self.cli()
        state.setdefault("deliveries", []).append({"delivery_id": "r1", "message": {
            "schema_version": 1, "message_id": "m-r1", "workspace_id": "w", "sender": {"kind": "agent", "id": "agent-a"},
            "recipients": [], "context_ids": [mapped], "body": '{"note": "not a control message"}', "created_at": "x"},
            "recipient": {}, "created_at": "x", "acked": False})
        self.cli_state.write_text(json.dumps(state), encoding="utf-8")
        self.bridge.run_outbound_once(0)
        self.assertIn('"note"', self.notices()[-1])

    def test_only_listed_agents_may_manage_im_users(self) -> None:
        result, _ = self.result(self.control("agent-x", {"im": "users.set", "user_id": 777, "label": "Mallory"}))
        self.assertEqual(result["error"]["code"], "forbidden")
        self.assertIsNone(self.bridge.state.get_user("telegram", "999", "777"))
        result, _ = self.result(self.control("ag_admin", {"im": "users.set", "user_id": 777, "label": "  Bob   B ", "enabled": True}))
        self.assertEqual(result["user"], {"user_id": "777", "label": "Bob B", "enabled": True})
        self.say("hi", user=777)
        self.assertEqual(self.core_sends()[-1]["external_source"]["metadata"]["user_label"], "Bob B")
        self.result(self.control("ag_admin", {"im": "users.set", "user_id": 777, "label": "Bob", "enabled": False}))
        self.say("hi again", user=777)
        self.assertIn("not allowed", self.notices()[-1])
        result, _ = self.result(self.control("ag_admin", {"im": "users.set", "user_id": "abc", "label": "x"}))
        self.assertEqual(result["error"]["code"], "invalid_user")
        users, _ = self.result(self.control("agent-x", {"im": "users.list"}))
        self.assertEqual({u["user_id"] for u in users["users"]}, {"1234", "777"})

    def test_list_and_audit_ops(self) -> None:
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-31", "to": "ag_l"}))
        listing, _ = self.result(self.control("agent-y", {"im": "poc.list"}))
        self.assertEqual({b["conversation"] for b in listing["bindings"]}, {"-10042:9", "-31:0"})
        audit, _ = self.result(self.control("agent-y", {"im": "audit", "limit": 5}))
        self.assertEqual(audit["events"][0]["actor"], "agent:agent-x")
        self.assertEqual(audit["events"][0]["new_target"], "ag_l")


class LimitTest(PocBase):
    limits = {"per_user_per_hour": 3, "per_conversation_per_minute": 100, "max_unreplied": 100}

    def test_per_user_hourly_limit(self) -> None:
        for i in range(3):
            self.say(f"m{i}")
        self.assertEqual(len(self.core_sends()), 3)
        self.say("m3")
        self.assertIn("too many messages", self.notices()[-1])
        self.assertEqual(len(self.core_sends()), 3, "the excess message never reaches Core")

    def test_limits_charge_only_accepted_messages(self) -> None:
        self.say("a")
        self.say("/status")
        self.say("/target")
        with sqlite3.connect(self.database) as db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM rate_events").fetchone()[0], 1, "commands are free and only accepted messages count")


class ConversationLimitTest(PocBase):
    limits = {"per_user_per_hour": 100, "per_conversation_per_minute": 2, "max_unreplied": 100}

    def test_per_conversation_burst_limit(self) -> None:
        self.say("a"); self.say("b"); self.say("c")
        self.assertEqual(len(self.core_sends()), 2)
        self.assertIn("too quickly", self.notices()[-1])


class UnrepliedTest(PocBase):
    limits = {"per_user_per_hour": 100, "per_conversation_per_minute": 100, "max_unreplied": 2}

    def test_backlog_cap_until_the_poc_answers(self) -> None:
        self.say("one"); self.say("two")
        self.say("three")
        self.assertIn("has not answered", self.notices()[-1])
        self.assertEqual(len(self.core_sends()), 2)
        mapped = self.core_sends()[0]["message_id"]
        state = self.cli()
        state.setdefault("deliveries", []).append({"delivery_id": "ans", "message": {
            "schema_version": 1, "message_id": "m-ans", "workspace_id": "w", "sender": {"kind": "agent", "id": "agent-a"},
            "recipients": [], "context_ids": [mapped], "body": "answer", "created_at": "x"}, "recipient": {}, "created_at": "x", "acked": False})
        self.cli_state.write_text(json.dumps(state), encoding="utf-8")
        self.bridge.run_outbound_once(0)
        time.sleep(0.01)
        self.say("four")
        self.assertEqual(len(self.core_sends()), 3, "once the POC has replied the conversation flows again")


class ConfigTest(unittest.TestCase):
    def load(self, config: dict[str, Any]) -> Any:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps(config), encoding="utf-8")
            old = {k: os.environ.get(k) for k in ("TREER_APP_CONFIG", "TREER_APP_STATE_DIR", "TELEGRAM_BOT_TOKEN")}
            os.environ.update(TREER_APP_CONFIG=str(path), TREER_APP_STATE_DIR=str(Path(directory) / "state"), TELEGRAM_BOT_TOKEN="1:abc")
            try:
                return telegram_runtime().load_config()[0]
            finally:
                for key, value in old.items():
                    os.environ.pop(key, None) if value is None else os.environ.__setitem__(key, value)

    def test_new_and_legacy_configs(self) -> None:
        legacy = self.load({"allowed_user_ids": [5], "bindings": [{"chat_id": 1, "target_agent_id": "a"}]})
        self.assertEqual((legacy.per_user_per_hour, legacy.max_unreplied, legacy.user_label(5)), (60, 50, "user-5"))
        new = self.load({"users": [{"id": 7, "label": " Ann  Lee "}], "limits": {"per_user_per_hour": 9, "max_unreplied": 4},
                         "control": {"user_admin_agent_ids": ["ag_x"]}})
        self.assertEqual((new.bindings, new.user_label(7), new.per_user_per_hour, new.max_unreplied, new.user_admin_agent_ids),
                         ((), "Ann Lee", 9, 4, frozenset({"ag_x"})))
        self.assertEqual(self.load({}).allowed_user_ids, frozenset(), "no users configured: they join by pairing")
        for bad in ({"users": [{"id": 1, "label": "x", "extra": 1}]}, {"allowed_user_ids": [1], "limits": {"nope": 1}},
                    {"allowed_user_ids": [1], "limits": {"per_user_per_hour": 0}}, {"allowed_user_ids": [1], "control": {"x": []}},
                    {"allowed_user_ids": [1, 1]}, {"users": [{"id": 1, "label": "a"}], "allowed_user_ids": [1]}):
            with self.assertRaises(BridgeError, msg=bad):
                self.load(bad)


def telegram_runtime() -> Any:
    import telegram_app.runtime as runtime
    return runtime


class RegistryTest(unittest.TestCase):
    def test_audit_retention_and_binding_rules(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            state = telegram.StateStore(Path(directory) / "s.sqlite3")
            with self.assertRaises(KeyError):
                state.set_binding("telegram", "b", "1:0", target=None, enabled=True, wake=None, actor="x", via="t", op="poc.on")
            for i in range(5100):
                state.audit(f"a{i}", "control", "poc.set", "telegram", "b", "1:0")
            events = state.audit_list("telegram", "b", 200)
            self.assertEqual(events[0]["actor"], "a5099")
            with sqlite3.connect(Path(directory) / "s.sqlite3") as db:
                self.assertLessEqual(db.execute("SELECT COUNT(*) FROM poc_audit").fetchone()[0], 5001)
            self.assertTrue(state.audit("x", "control", "poc.set", "telegram", "b", None, delivery_id="d"))
            self.assertFalse(state.audit("x", "control", "poc.set", "telegram", "b", None, delivery_id="d"), "one audit row per delivery")


if __name__ == "__main__":
    unittest.main()


class CtlTest(unittest.TestCase):
    def test_payloads_and_submission(self) -> None:
        import subprocess
        import sys
        from telegram_app.ctl import build, parser
        p = parser()
        cases = [
            (["get", "--conversation=-10042:9"], {"im": "poc.get", "conversation": "-10042:9"}),
            (["set", "--conversation=-5", "/team/x", "--wake"], {"im": "poc.set", "conversation": "-5", "to": "/team/x", "wake_agent": True}),
            (["set", "--conversation=-5", "ag_y"], {"im": "poc.set", "conversation": "-5", "to": "ag_y"}),
            (["handoff", "ag_z"], {"im": "poc.handoff", "to": "ag_z"}),
            (["handoff", "ag_z", "--conversation=-7:1"], {"im": "poc.handoff", "to": "ag_z", "conversation": "-7:1"}),
            (["disable", "--conversation=-1"], {"im": "poc.disable", "conversation": "-1"}),
            (["list"], {"im": "poc.list"}), (["audit", "--limit", "5"], {"im": "audit", "limit": 5}), (["users"], {"im": "users.list"}),
            (["user-set", "7", "Ann", "--disabled"], {"im": "users.set", "user_id": 7, "label": "Ann", "enabled": False}),
        ]
        for argv, expected in cases:
            self.assertEqual(build(p.parse_args(argv)), expected, argv)
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / "state.json"
            state.write_text("{}", encoding="utf-8")
            env = {**os.environ, "FAKE_TREER_STATE": str(state), "TREER_CLI": str(FAKE_TREER)}
            env.pop("IM_BRIDGE", None)
            FAKE_TREER.chmod(0o755)
            run = subprocess.run([sys.executable, str(APP_ROOT / "im-poc"), "--bridge", "/infra/im", "set", "--conversation=-5", "ag_y"], env=env, capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            (message,) = json.loads(state.read_text())["messages"]
            self.assertEqual(json.loads(message["body"]), {"im": "poc.set", "conversation": "-5", "to": "ag_y"})
            self.assertEqual(message["recipients"][0]["id"], "/infra/im")
            missing = subprocess.run([sys.executable, str(APP_ROOT / "im-poc"), "list"], env=env, capture_output=True, text=True)
            self.assertEqual(missing.returncode, 2)


class ReviewFixesTest(PocBase):
    admins = frozenset({"ag_admin"})

    def notices_to(self) -> dict[str, str]:
        return {m["recipients"][0]["id"]: m["body"] for m in self.cli().get("messages", []) if m["body"].startswith("IM point of contact changed")}

    def test_removing_a_config_entry_and_restarting_revokes_it(self) -> None:
        both = dict(allowed_user_ids=frozenset({1234, 5678}),
                    bindings=(telegram.Binding("-10042", 9, "agent-a", False), telegram.Binding("-20", None, "agent-b", False)))
        self.bridge = self.new_bridge(**both)
        self.say("/poc ag_runtime", chat=-20, thread=None)                  # runtime-owned from now on
        only_a = dict(allowed_user_ids=frozenset({1234}), bindings=(telegram.Binding("-10042", 9, "agent-a", False),))
        self.bridge = self.new_bridge(**only_a)
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "5678"), ("user-5678", False), "dropped from config: disabled")
        self.say("hi", user=5678)
        self.assertIn("not allowed", self.notices()[-1])
        self.assertTrue(self.bridge.state.get_binding("telegram", "999", "-20:0").enabled, "a runtime change is not overridden by the file")
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-20:0").target_agent_id, "ag_runtime")
        self.bridge = self.new_bridge(allowed_user_ids=frozenset({1234}), bindings=())
        binding = self.bridge.state.get_binding("telegram", "999", "-10042:9")
        self.assertFalse(binding.enabled)
        self.say("anyone?")
        self.assertIn("switched off", self.notices()[-1])
        ops = [e["op"] for e in self.audit()]
        self.assertIn("config.revoke_user", ops)
        self.assertIn("config.revoke_binding", ops)
        revoke = [e for e in self.audit() if e["op"] == "config.revoke_binding"][0]
        self.assertEqual((revoke["actor"], revoke["via"], revoke["old_target"]), ("config", "startup", "agent-a"))
        # adding the entry back re-enables a config-owned row
        self.bridge = self.new_bridge(**both)
        self.assertTrue(self.bridge.state.get_binding("telegram", "999", "-10042:9").enabled)
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "5678"), ("user-5678", True))

    def test_users_set_rows_are_runtime_owned(self) -> None:
        self.result(self.control("ag_admin", {"im": "users.set", "user_id": 4242, "label": "Dana"}))
        self.assertIn("users.set", [e["op"] for e in self.audit()])
        self.assertEqual(self.audit()[0]["actor"], "agent:ag_admin")
        self.bridge = self.new_bridge()                                       # restart: not in config, but runtime-owned
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "4242"), ("Dana", True))

    def test_rejected_requests_cannot_flush_the_change_history(self) -> None:
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-10042:9", "to": "ag_hijack"}))
        for i in range(250):
            self.bridge.state.audit_reject("agent:spam", "poc.set", "target_unavailable")
        self.assertEqual(self.audit()[0]["new_target"], "ag_hijack")
        self.assertEqual(len(self.bridge.state.rejects_list(1000)), 200, "rejects live in their own small ring")
        self.result(self.control("agent-x", {"im": "poc.get"}))                # an invalid request is recorded as a reject only
        self.assertEqual(self.bridge.state.rejects_list(1)[0]["code"], "invalid_conversation")
        self.assertEqual([e["op"] for e in self.audit()], ["poc.set"])

    def test_targets_must_look_like_ids_or_absolute_paths(self) -> None:
        for target in ("-n", "--help", "builder", "sibling"):
            body, _ = self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-1", "to": target}))
            self.assertEqual(body["error"]["code"], "invalid_target", target)
        self.say("/poc builder")
        self.assertIn("stable Agent ID", self.notices()[-1])
        self.assertEqual([c["argv"] for c in self.cli()["calls"] if c["argv"][:2] == ["agent", "show"]], [], "never reaches the CLI")

    def test_a_poc_change_is_announced_to_the_agents_that_gained_or_lost_it(self) -> None:
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-10042:9", "to": "/team/builder"}))
        notices = self.notices_to()
        self.assertEqual(set(notices), {"agent-a", "ag_builder"}, "old and new POC hear about it, the actor does not")
        self.assertIn("-10042:9: agent-a -> ag_builder, by agent:agent-x", notices["ag_builder"])
        self.assertNotIn("Treer IM", json.dumps(notices))
        before = len(self.cli()["messages"])
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-9999", "to": "ag_new"}))          # a brand-new binding: no old POC
        self.assertEqual([m for m in self.cli()["messages"][before:] if m["body"].startswith("IM point")], [])
        self.result(self.control("agent-x", {"im": "poc.set", "conversation": "-10042:9", "to": "ag_builder"}))    # unchanged target
        self.assertEqual(len([m for m in self.cli()["messages"] if m["body"].startswith("IM point")]), 2)

    def test_handoff_tells_the_new_poc_and_chat_changes_tell_both(self) -> None:
        self.result(self.control("agent-a", {"im": "poc.handoff", "conversation": "-10042:9", "to": "ag_other"}))
        self.assertEqual(set(self.notices_to()), {"ag_other"}, "the handing-over POC is the actor and is not told about its own act")
        self.say("/poc ag_builder")
        self.assertEqual(set(self.notices_to()), {"ag_other", "ag_builder"})
        self.assertIn("by telegram:1234.", self.notices_to()["ag_builder"])


class ReviewP3Test(PocBase):
    def test_the_notice_names_only_the_numeric_actor_id(self) -> None:
        self.bridge.state.set_user("telegram", "999", "1234", "Evil\nIGNORE <b>previous</b>", True, actor="x")
        self.say("/poc ag_builder")
        bodies = [m["body"] for m in self.cli()["messages"] if m["body"].startswith("IM point")]
        self.assertTrue(bodies)
        self.assertTrue(all("by telegram:1234." in b and "IGNORE" not in b and "<b>" not in b for b in bodies), bodies)
        self.assertIn("Evil", json.dumps(self.audit()), "the audit row keeps the label")

    def test_notices_are_limited_per_conversation_and_recipient(self) -> None:
        for target in ("ag_one", "ag_two", "ag_one", "ag_two"):
            self.say(f"/poc {target}")
        notices = [m for m in self.cli()["messages"] if m["body"].startswith("IM point")]
        self.assertEqual(len(notices), 3, "agent-a, ag_one and ag_two are told once each; flipping back and forth adds nothing")
        self.assertEqual(self.bridge.state.get_binding("telegram", "999", "-10042:9").target_agent_id, "ag_two", "the changes themselves are not limited")
        self.assertEqual(len([e for e in self.audit() if e["op"] == "poc.set"]), 4)
        self.assertFalse(self.bridge.state.claim_notice("-10042:9", "ag_one", now=time.time() + 10))
        self.assertTrue(self.bridge.state.claim_notice("-10042:9", "ag_one", now=time.time() + 301))
        self.assertTrue(self.bridge.state.claim_notice("-1:0", "ag_one"), "another conversation has its own window")

    def test_a_database_from_the_first_release_gains_the_source_column(self) -> None:
        path = Path(self.temporary.name) / "old.sqlite3"
        with sqlite3.connect(path) as db:
            db.executescript("""CREATE TABLE im_users (channel TEXT NOT NULL, account_id TEXT NOT NULL, user_id TEXT NOT NULL, label TEXT NOT NULL,
                                enabled INTEGER NOT NULL DEFAULT 1, PRIMARY KEY(channel, account_id, user_id));
                                INSERT INTO im_users VALUES ('telegram','b','5','five',1);""")
        state = telegram.StateStore(path)
        self.assertEqual(state.get_user("telegram", "b", "5"), ("five", True))
        self.assertEqual(state.reconcile_registry("telegram", "b", [], []), ["user 5 removed from config: disabled"], "old rows count as config-owned")
        state.set_user("telegram", "b", "6", "six", True, actor="x")


class AccountTest(unittest.TestCase):
    def test_reconcile_returns_log_lines(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            state = telegram.StateStore(Path(directory) / "s.sqlite3")
            self.assertEqual(state.reconcile_registry("telegram", "b", [("1:0", "ag_a", False)], [("5", "five")]), [])
            changes = state.reconcile_registry("telegram", "b", [], [])
            self.assertEqual(sorted(changes), ["binding 1:0 removed from config: disabled", "user 5 removed from config: disabled"])
            self.assertEqual(state.reconcile_registry("telegram", "b", [], []), [], "idempotent")


class TokenFileTest(unittest.TestCase):
    SECRET = "123456:SECRET-token-value"

    def load(self, config: dict[str, Any], *, env_token: str | None = None) -> tuple[Any, ...]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text(json.dumps({"allowed_user_ids": [5], **config}), encoding="utf-8")
            old = {k: os.environ.get(k) for k in ("TREER_APP_CONFIG", "TREER_APP_STATE_DIR", "TELEGRAM_BOT_TOKEN")}
            os.environ.pop("TELEGRAM_BOT_TOKEN", None)
            os.environ.update(TREER_APP_CONFIG=str(path), TREER_APP_STATE_DIR=str(Path(directory) / "state"))
            if env_token is not None:
                os.environ["TELEGRAM_BOT_TOKEN"] = env_token
            try:
                return telegram_runtime().load_config()
            finally:
                for key, value in old.items():
                    os.environ.pop(key, None) if value is None else os.environ.__setitem__(key, value)

    def token_file(self, directory: Path, content: bytes = b"123456:SECRET-token-value\n", mode: int = 0o600) -> Path:
        path = directory / "bot-token"
        path.write_bytes(content)
        path.chmod(mode)
        return path

    def test_the_token_is_read_from_a_private_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = self.token_file(Path(directory))
            self.assertEqual(self.load({"bot_token_file": str(path)})[3], self.SECRET, "the trailing newline is stripped")
            self.assertEqual(self.load({"bot_token_file": str(path)}, env_token="9:env")[3], "9:env", "the environment variable still wins")

    def test_unsafe_or_missing_token_files_are_refused_without_leaking_the_token(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            good = self.token_file(root)
            link = root / "link"
            link.symlink_to(good)
            cases = {
                "group readable": self.token_file(root, mode=0o640),
                "world readable": self.token_file(root, mode=0o644),
                "symlink": link,
                "directory": root,
                "empty": self.token_file(root, b""),
                "oversized": self.token_file(root, b"1:" + b"a" * 600),
                "non ascii": self.token_file(root, "1:密钥".encode()),
                "relative": "bot-token",
                "missing": root / "nope",
            }
            for name, path in cases.items():
                with self.assertRaises(BridgeError, msg=name) as caught:
                    self.load({"bot_token_file": str(path)})
                self.assertNotIn("SECRET", str(caught.exception), name)
            for content in (b"two words", b"1:a b"):
                with self.assertRaises(BridgeError):
                    self.load({"bot_token_file": str(self.token_file(root, content))})
        self.assertIsNone(self.load({})[3], "nothing configured yet: the bridge waits for a token instead of failing")
        with self.assertRaises(BridgeError):
            self.load({"bot_token_file": ""})

    def test_user_admin_is_open_by_default_and_a_list_restricts_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = str(self.token_file(Path(directory)))
            self.assertIsNone(self.load({"bot_token_file": path})[0].user_admin_agent_ids, "absent: any accepted sender")
            self.assertEqual(self.load({"bot_token_file": path, "control": {"user_admin_agent_ids": []}})[0].user_admin_agent_ids, frozenset(), "empty list: nobody")
            self.assertEqual(self.load({"bot_token_file": path, "control": {"user_admin_agent_ids": ["ag_a"]}})[0].user_admin_agent_ids, frozenset({"ag_a"}))


class OpenUserAdminTest(PocBase):
    admins = None

    def test_any_accepted_sender_may_admit_a_user_and_it_is_audited(self) -> None:
        result, _ = self.result(self.control("agent-x", {"im": "users.set", "user_id": 777, "label": "Dana"}))
        self.assertEqual(result["user"], {"user_id": "777", "label": "Dana", "enabled": True})
        self.assertEqual(self.audit()[0]["op"], "users.set")
        self.assertEqual(self.audit()[0]["actor"], "agent:agent-x")
        self.say("hi", user=777)
        self.assertEqual(self.core_sends()[-1]["external_source"]["metadata"]["user_label"], "Dana")


class EmptyUserAdminTest(PocBase):
    admins = frozenset()

    def test_an_explicit_empty_list_still_means_nobody(self) -> None:
        result, _ = self.result(self.control("agent-x", {"im": "users.set", "user_id": 777, "label": "Dana"}))
        self.assertEqual(result["error"]["code"], "forbidden")
