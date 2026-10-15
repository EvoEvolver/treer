from __future__ import annotations

import json
import os
import sqlite3
import stat
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any

import test_telegram as base
import test_poc as poc_tests
from telegram_app import registry, runtime
from telegram_app.common import BridgeError
from telegram_app.tokenstore import TokenStore, last4, valid_shape

telegram = base.telegram
FAKE_TREER = base.FAKE_TREER
TOKEN = "123456:abcdefghijklmnopqrstuvwxyz0123"


class TokenStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.dir = Path(self.directory.name) / "state"
        self.store = TokenStore(self.dir)

    def tearDown(self) -> None:
        self.directory.cleanup()

    def test_write_read_replace_and_clear(self) -> None:
        self.assertIsNone(self.store.read())
        self.assertFalse(self.store.exists())
        self.store.write(TOKEN)
        info = os.lstat(self.store.path)
        self.assertEqual(stat.S_IMODE(info.st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(self.dir).st_mode), 0o700)
        self.assertEqual(self.store.read(), TOKEN)
        other = "654321:ZYXWVUTSRQPONMLKJIHGFEDCBA9876"
        self.store.write(other)
        self.assertEqual(self.store.read(), other)
        self.assertEqual([p.name for p in self.dir.iterdir()], ["bot-token"], "no temporary file is left behind")
        self.assertTrue(self.store.clear())
        self.assertFalse(self.store.exists())
        self.assertFalse(self.store.clear())

    def test_clear_overwrites_before_deleting(self) -> None:
        self.store.write(TOKEN)
        handle = open(self.store.path, "rb")          # keep the inode alive to inspect it after unlink
        try:
            self.store.clear()
            self.assertEqual(set(handle.read()) - {ord("0")}, set(), "the old bytes were overwritten, not just unlinked")
        finally:
            handle.close()

    def test_unsafe_files_and_bad_tokens_are_refused(self) -> None:
        self.dir.mkdir(mode=0o700)
        target = self.dir / "elsewhere"
        target.write_text(TOKEN)
        os.chmod(target, 0o600)
        self.store.path.symlink_to(target)
        with self.assertRaises(BridgeError):
            self.store.read()
        with self.assertRaises(BridgeError):
            self.store.write(TOKEN)                   # never replaces a symlink
        with self.assertRaises(BridgeError):
            self.store.clear()
        self.store.path.unlink()
        self.store.path.write_text(TOKEN)
        os.chmod(self.store.path, 0o640)
        with self.assertRaises(BridgeError):
            self.store.read()
        os.unlink(self.store.path)
        for bad in ("", "no-colon", "abc:def", "1:short", TOKEN + " x", "12:" + "é" * 30, "x" * 600):
            with self.assertRaises(BridgeError, msg=bad):
                self.store.write(bad)
        self.assertTrue(valid_shape(TOKEN))
        self.assertEqual(last4(TOKEN), "0123")


class PairingRegistryTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.state = telegram.StateStore(Path(self.directory.name) / "s.sqlite3")
        self.t = 1_800_000_000.0

    def tearDown(self) -> None:
        self.directory.cleanup()

    def test_code_shape_and_storage(self) -> None:
        code, expires = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        self.assertRegex(code, r"^[A-HJKMNP-TV-Z2-9]{5}-[A-HJKMNP-TV-Z2-9]{5}$")
        self.assertEqual(expires, self.t + registry.PAIR_TTL)
        with sqlite3.connect(Path(self.directory.name) / "s.sqlite3") as db:
            stored = json.dumps(db.execute("SELECT * FROM pairings").fetchall())
        self.assertNotIn(code.replace("-", ""), stored, "only a salted hash is stored")
        self.assertEqual(self.state.pairing_status(self.t)["poc_agent_id"], "ag_poc")
        self.assertNotIn("code", json.dumps(self.state.pairing_status(self.t)))

    def test_a_code_works_once_and_is_normalised(self) -> None:
        code, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        status, row = self.state.redeem_pairing(code.lower().replace("-", " "), "42", now=self.t + 5)
        self.assertEqual((status, row["poc_agent_id"], row["created_by"]), ("ok", "ag_poc", "human:usr_a"))
        self.assertEqual(self.state.redeem_pairing(code, "42", now=self.t + 6)[0], "invalid", "replay")
        self.assertEqual(self.state.redeem_pairing(code, "43", now=self.t + 6)[0], "invalid", "another user replaying it")
        self.assertIsNone(self.state.pairing_status(self.t + 7))

    def test_expiry_and_regeneration(self) -> None:
        old, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        new, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t + 1)
        self.assertEqual(self.state.redeem_pairing(old, "42", now=self.t + 2)[0], "invalid", "a new code kills the old one at once")
        self.assertEqual(self.state.redeem_pairing(new, "42", now=self.t + registry.PAIR_TTL + 2)[0], "invalid", "expired")
        fresh, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t + 1000)
        self.assertEqual(self.state.invalidate_pairings(now=self.t + 1001), 1)
        self.assertEqual(self.state.redeem_pairing(fresh, "42", now=self.t + 1002)[0], "invalid", "cleared")

    def test_per_user_lockout_even_with_the_right_code(self) -> None:
        code, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        for i in range(registry.PAIR_USER_FAILURES):
            self.assertEqual(self.state.redeem_pairing("AAAAA-AAAAA", "evil", now=self.t + i)[0], "invalid")
        self.assertEqual(self.state.redeem_pairing(code, "evil", now=self.t + 10)[0], "blocked", "blocked even with the right code")
        self.assertEqual(self.state.redeem_pairing(code, "good", now=self.t + 11)[0], "ok", "other users are unaffected")
        code2, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t + registry.PAIR_WINDOW)
        self.assertEqual(self.state.redeem_pairing(code2, "evil", now=self.t + registry.PAIR_WINDOW + 30)[0], "ok", "the block ends with the window")

    def test_global_failures_invalidate_every_open_code(self) -> None:
        code, _ = self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        for i in range(registry.PAIR_GLOBAL_FAILURES):          # many different users guessing
            self.state.redeem_pairing("AAAAA-AAAAA", f"u{i}", now=self.t + i)
        self.assertEqual(self.state.redeem_pairing(code, "honest", now=self.t + 50)[0], "locked")
        self.assertIsNone(self.state.pairing_status(self.t + 51), "the open code died with the lock")
        self.assertEqual(self.state.redeem_pairing(code, "honest", now=self.t + 52)[0], "locked")

    def test_wrong_lengths_and_junk_are_ordinary_failures(self) -> None:
        self.state.create_pairing("human:usr_a", "ag_poc", now=self.t)
        for junk in ("", "x", "A" * 500, "¿¿¿", "AAAAA-AAAA"):
            self.assertEqual(self.state.redeem_pairing(junk, "u", now=self.t)[0], "invalid")

    def test_settings_and_setting_keys(self) -> None:
        self.assertIsNone(self.state.get_setting("paused"))
        self.state.set_setting("paused", "1")
        self.assertEqual(self.state.get_setting("paused"), "1")
        self.state.delete_settings("paused")
        self.assertIsNone(self.state.get_setting("paused"))
        self.state.set_setting("bridge_state", "running")
        self.assertIn("bridge_state", self.state.settings_with_age("bridge_"))


class PairCommandTest(poc_tests.PocBase):
    """/pair CODE through the real bridge with the fake Bot API and fake CLI."""
    def pair(self, text: str, *, user: int = 9001, chat: int = 9001, chat_type: str = "private", username: str | None = "dana") -> None:
        poc_tests.PocBase.update_counter += 1
        n = poc_tests.PocBase.update_counter
        sender: dict[str, Any] = {"id": user, "is_bot": False}
        if username:
            sender["username"] = username
        self.bridge.process_update({"update_id": n, "message": {"message_id": 5000 + n, "chat": {"id": chat, "type": chat_type},
                                                                "from": sender, "text": text}})

    def code(self, poc: str = "ag_poc") -> str:
        return self.bridge.state.create_pairing("human:usr_admin", poc, now=time.time())[0]

    def test_a_stranger_pairs_and_gets_admitted_and_bound(self) -> None:
        self.say("hello", chat=9001, thread=None, user=9001)
        self.assertIn("not allowed", self.notices()[-1])
        self.pair(f"/pair {self.code()}")
        self.assertIn("Paired. Your messages in this chat now go to ag_poc", self.notices()[-1])
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "9001"), ("dana", True))
        binding = self.bridge.state.get_binding("telegram", "999", "9001:0")
        self.assertEqual((binding.target_agent_id, binding.enabled), ("ag_poc", True))
        self.say("hi poc", chat=9001, thread=None, user=9001)
        sent = self.core_sends()[-1]
        self.assertEqual((sent["recipients"][0]["id"], sent["body"], sent["external_source"]["metadata"]["user_label"]), ("ag_poc", "hi poc", "dana"))
        ops = [e["op"] for e in self.audit()]
        self.assertIn("pair.user", ops); self.assertIn("poc.pair", ops)
        self.assertTrue(all(e["via"] == "pair" for e in self.audit() if e["op"] in ("pair.user", "poc.pair")))
        recipients = {m["recipients"][0]["id"] for m in self.cli()["messages"] if "was paired" in m["body"]}
        self.assertEqual(recipients, {"ag_poc", "usr_admin"}, "the new POC and the admin who made the code are told")
        self.assertTrue(all("dana" in m["body"] and "<" not in m["body"] for m in self.cli()["messages"] if "was paired" in m["body"]))

    def test_the_code_is_single_use_and_wrong_codes_are_refused(self) -> None:
        code = self.code()
        self.pair("/pair AAAAA-AAAAA")
        self.assertIn("not valid or has expired", self.notices()[-1])
        self.assertIsNone(self.bridge.state.get_user("telegram", "999", "9001"))
        self.pair(f"/pair {code}")
        self.assertIn("Paired", self.notices()[-1])
        self.pair(f"/pair {code}", user=9002, chat=9002)
        self.assertIn("not valid or has expired", self.notices()[-1])
        self.assertIsNone(self.bridge.state.get_user("telegram", "999", "9002"))
        self.pair("/pair")
        self.assertIn("not valid", self.notices()[-1])

    def test_groups_never_consume_a_code(self) -> None:
        code = self.code()
        self.pair(f"/pair {code}", chat=-5000, chat_type="supergroup")
        self.assertIn("only in a private chat", self.notices()[-1])
        self.assertIsNotNone(self.bridge.state.pairing_status(), "the code is still unspent after a group attempt")
        self.assertIsNone(self.bridge.state.get_binding("telegram", "999", "-5000:0"))
        self.pair(f"/pair {code}")
        self.assertIn("Paired", self.notices()[-1])

    def test_brute_force_is_blocked_and_audited(self) -> None:
        code = self.code()
        for _ in range(registry.PAIR_USER_FAILURES):
            self.pair("/pair BBBBB-BBBBB", user=7, chat=7)
        self.pair(f"/pair {code}", user=7, chat=7)
        self.assertIn("Too many failed attempts", self.notices()[-1])
        self.assertIsNone(self.bridge.state.get_user("telegram", "999", "7"))

    def test_an_unlisted_label_falls_back_to_the_user_id_and_an_unknown_poc_is_reported(self) -> None:
        self.pair(f"/pair {self.code()}", username="x" * 40, user=9100, chat=9100)
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "9100"), ("user-9100", True))
        os.environ["FAKE_TREER_UNKNOWN_AGENTS"] = "ag_missing"
        self.pair(f"/pair {self.code('ag_missing')}", user=9200, chat=9200)
        self.assertIn("could not be set", self.notices()[-1])
        self.assertEqual(self.bridge.state.get_user("telegram", "999", "9200"), ("dana", True))
        self.assertIsNone(self.bridge.state.get_binding("telegram", "999", "9200:0"))

    def test_commands_work_for_the_paired_user_afterwards(self) -> None:
        self.pair(f"/pair {self.code()}")
        self.say("/poc", chat=9001, thread=None, user=9001)
        self.assertIn("POC for this chat: ag_poc (on)", self.notices()[-1])


class SupervisorTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.state_dir = self.root / "state"
        self.cli_state = self.root / "cli.json"
        self.cli_state.write_text("{}", encoding="utf-8")
        FAKE_TREER.chmod(0o755)
        self.bot_state = base.FakeBotState()
        self.server = base.FakeBotServer(("127.0.0.1", 0), self.bot_state)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.config_path = self.root / "config.json"
        self.config_path.write_text(json.dumps({"api_base_url": f"http://127.0.0.1:{self.server.server_address[1]}", "allowed_user_ids": [1],
                                                "poll_timeout_seconds": 1, "receive_wait_milliseconds": 0}), encoding="utf-8")
        self.saved = {k: os.environ.get(k) for k in ("TREER_APP_CONFIG", "TREER_APP_STATE_DIR", "TREER_CLI", "TELEGRAM_BOT_TOKEN", "FAKE_TREER_STATE")}
        os.environ.pop("TELEGRAM_BOT_TOKEN", None)
        os.environ.update(TREER_APP_CONFIG=str(self.config_path), TREER_APP_STATE_DIR=str(self.state_dir), TREER_CLI=str(FAKE_TREER),
                          FAKE_TREER_STATE=str(self.cli_state))
        self.builds = 0
        real = runtime.build_bridge
        def counting(*args: Any, **kwargs: Any) -> Any:
            self.builds += 1
            return real(*args, **kwargs)
        self.real_build, runtime.build_bridge = real, counting
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=runtime.run_supervised, args=(self.stopped,), kwargs={"poll": 0.1}, daemon=True)
        self.thread.start()
        self.store = TokenStore(self.state_dir)

    def tearDown(self) -> None:
        self.stopped.set()
        self.thread.join(timeout=10)
        runtime.build_bridge = self.real_build
        self.server.shutdown(); self.server.server_close()
        for key, value in self.saved.items():
            os.environ.pop(key, None) if value is None else os.environ.__setitem__(key, value)
        self.temporary.cleanup()

    def status(self) -> str | None:
        db = self.state_dir / "telegram-state.sqlite3"
        if not db.exists():
            return None
        with sqlite3.connect(db) as connection:
            row = connection.execute("SELECT value FROM settings WHERE key='bridge_state'").fetchone()
        return row[0] if row else None

    def until(self, want: str, seconds: float = 8) -> None:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.status() == want:
                return
            time.sleep(0.05)
        self.fail(f"status {self.status()!r}, wanted {want!r}")

    def test_waits_for_a_token_then_runs_reloads_pauses_and_clears(self) -> None:
        self.until("needs_token")
        self.assertEqual(self.builds, 0)
        self.store.write(TOKEN)
        self.until("running")
        self.assertEqual(self.builds, 1)
        with sqlite3.connect(self.state_dir / "telegram-state.sqlite3") as connection:
            settings = dict(connection.execute("SELECT key, value FROM settings").fetchall())
        self.assertEqual((settings["bot_id"], settings["bot_username"], settings["bridge_error"]), ("999", "treer_test_bot", ""))
        polls = len(self.bot_state.offsets)
        self.assertGreater(polls, 0, "the bridge polls once it has a token")
        # pause: polling stops, status says so; resume: it continues
        telegram.StateStore(self.state_dir / "telegram-state.sqlite3").set_setting("paused", "1")
        self.until("paused")
        time.sleep(1.5)
        frozen = len(self.bot_state.offsets)
        time.sleep(1.5)
        self.assertEqual(len(self.bot_state.offsets), frozen, "no polling while paused")
        telegram.StateStore(self.state_dir / "telegram-state.sqlite3").set_setting("paused", "0")
        self.until("running")
        # replace with another token of the same bot: rebuilt without restarting the process
        self.store.write("123456:ZYXWVUTSRQPONMLKJIHGFEDCBA9876")
        end = time.monotonic() + 8
        while self.builds < 2 and time.monotonic() < end:
            time.sleep(0.05)
        self.assertEqual(self.builds, 2, "a changed token rebuilds the bridge")
        self.until("running")
        self.assertTrue(self.store.clear())
        self.until("needs_token")

    def test_an_unreachable_bot_is_an_error_status_with_a_fixed_code(self) -> None:
        self.server.shutdown()
        self.server.server_close()                    # refuse connections instead of hanging until the Bot API timeout
        self.store.write(TOKEN)
        self.until("error", 20)
        with sqlite3.connect(self.state_dir / "telegram-state.sqlite3") as connection:
            error = connection.execute("SELECT value FROM settings WHERE key='bridge_error'").fetchone()[0]
        self.assertIn(error, ("bot_unavailable", "telegram_unavailable"))
        self.assertNotIn("abcdefghij", error)


if __name__ == "__main__":
    unittest.main()
