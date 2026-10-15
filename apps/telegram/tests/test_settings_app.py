from __future__ import annotations

import json
import os
import stat
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import test_telegram as base
from settings_app.api import Settings
from settings_app.auth import Auth, Error
from settings_app.server import Application, Server, build_settings
from telegram_app.common import BridgeError, TelegramError
from telegram_app.tokenstore import TokenStore

telegram = base.telegram
FAKE_TREER = base.FAKE_TREER
TOKEN = "123456:abcdefghijklmnopqrstuvwxyz0123"
PEOPLE = {
    "admin": {"kind": "human", "id": "usr_admin", "name": "Ada", "role": "admin"},
    "owner": {"kind": "human", "id": "usr_owner", "name": "Olu", "role": "owner"},
    "member": {"kind": "human", "id": "usr_member", "name": "Mel", "role": "member"},
    "agent-ok": {"kind": "agent", "id": "ag_ok", "name": "operator"},
    "agent-no": {"kind": "agent", "id": "ag_no", "name": "stranger"},
}
CSRF = {"X-Telegram-Settings": "1", "Content-Type": "application/json"}


class FakeAuth(Auth):
    """The real CSRF/origin logic with Treer's verification replaced by a token table."""
    def verify(self, token: str | None) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        if not token or token not in PEOPLE:
            raise Error(401, "identity_invalid", "Identity is inactive or outside this App's workspace")
        return PEOPLE[token], list(PEOPLE.values())


class FakeBot:
    def __init__(self, behaviour: Any) -> None:
        self.behaviour = behaviour

    def get_me(self) -> dict[str, Any]:
        if isinstance(self.behaviour, Exception):
            raise self.behaviour
        return self.behaviour


class SettingsAppTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.state_dir = self.root / "state"
        self.cli_state = self.root / "cli.json"
        self.cli_state.write_text("{}", encoding="utf-8")
        FAKE_TREER.chmod(0o755)
        os.environ["FAKE_TREER_STATE"] = str(self.cli_state)
        os.environ["FAKE_TREER_PATHS"] = json.dumps({"/team/poc": "ag_poc"})
        os.environ["FAKE_TREER_UNKNOWN_AGENTS"] = "ag_missing"
        self.me = {"id": 999, "is_bot": True, "username": "treer_test_bot"}
        self.bot = FakeBot(self.me)
        self.config = {"settings": {"admin_agent_ids": ["ag_ok"], "default_poc": "ag_default"}}
        self.settings = build_settings(self.config, self.state_dir, str(FAKE_TREER), bot_factory=lambda token: self.bot)
        self.auth = FakeAuth("http://127.0.0.1:9/", "svc_tg", "ws_test", "https://app.example/")
        self.server = Server(("127.0.0.1", 0), Application(self.settings, self.auth))
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.transcript: list[str] = []

    def tearDown(self) -> None:
        self.server.shutdown(); self.server.server_close()
        for name in ("FAKE_TREER_STATE", "FAKE_TREER_PATHS", "FAKE_TREER_UNKNOWN_AGENTS"):
            os.environ.pop(name, None)
        self.temporary.cleanup()

    def call(self, method: str, path: str, body: Any = None, who: str | None = "admin", headers: dict[str, str] | None = None,
             csrf: bool = True) -> tuple[int, Any]:
        merged = {**(CSRF if csrf else {"Content-Type": "application/json"}), **({"X-Treer-Identity-Token": who} if who is not None else {}), **(headers or {})}
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=merged)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=10) as response:
                raw, status = response.read(), response.status
        except urllib.error.HTTPError as exc:
            raw, status = exc.read(), exc.code
        self.transcript.append(raw.decode("utf-8", "replace"))
        try:
            return status, json.loads(raw)
        except ValueError:
            return status, raw

    def save_token(self, who: str = "admin") -> None:
        status, body = self.call("PUT", "/v1/token", {"token": TOKEN}, who)
        self.assertEqual(status, 200, body)

    def assert_no_token_anywhere(self) -> None:
        self.assertFalse([t for t in self.transcript if TOKEN in t or TOKEN.split(":")[1] in t], "the token never appears in a response")

    # --- access ------------------------------------------------------------------------------

    def test_authentication_and_roles(self) -> None:
        for who in (None, "forged", ""):
            self.assertEqual(self.call("GET", "/v1/status", who=who)[0], 401, who)
        self.assertEqual(self.call("GET", "/v1/status", who="member")[0], 403)
        self.assertEqual(self.call("GET", "/v1/status", who="agent-no")[0], 403)
        for who in ("admin", "owner", "agent-ok"):
            self.assertEqual(self.call("GET", "/v1/status", who=who)[0], 200, who)
        # every write is closed to the same principals, whatever the page shows
        for who in ("member", "agent-no"):
            for method, path, body in (("PUT", "/v1/token", {"token": TOKEN}), ("DELETE", "/v1/token", {}), ("POST", "/v1/pairing", {}),
                                       ("DELETE", "/v1/pairing", {}), ("PUT", "/v1/bindings", {"conversation": "1", "to": "ag_x"}),
                                       ("POST", "/v1/bindings/enable", {"conversation": "1"}), ("PUT", "/v1/users", {"user_id": 1, "label": "x", "enabled": True}),
                                       ("POST", "/v1/pause", {}), ("POST", "/v1/resume", {})):
                self.assertEqual(self.call(method, path, body, who=who)[0], 403, (who, method, path))
        self.assertFalse(self.settings.tokens.exists())
        # a bearer header works for agents; an empty injected header is never replaced by another credential
        self.assertEqual(self.call("GET", "/v1/status", who=None, headers={"Authorization": "Bearer agent-ok"})[0], 200)
        self.assertEqual(self.call("GET", "/v1/status", who="", headers={"Authorization": "Bearer agent-ok"})[0], 401)

    def test_csrf_and_origin(self) -> None:
        self.assertEqual(self.call("PUT", "/v1/token", {"token": TOKEN}, csrf=False)[0], 403, "humans need the custom header")
        self.assertEqual(self.call("POST", "/v1/pause", {}, headers={"Origin": "https://evil.example"})[0], 403)
        self.assertEqual(self.call("POST", "/v1/pause", {}, headers={"Origin": "null"})[0], 403)
        self.assertEqual(self.call("POST", "/v1/pause", {}, headers={"Sec-Fetch-Site": "cross-site"})[0], 403)
        self.assertEqual(self.call("POST", "/v1/pause", {}, headers={"Sec-Fetch-Site": "none"})[0], 403)
        self.assertEqual(self.call("POST", "/v1/pause", {}, headers={"Origin": "https://app.example", "Sec-Fetch-Site": "same-origin"})[0], 200)
        self.assertEqual(self.call("GET", "/v1/status", headers={"Sec-Fetch-Site": "cross-site"})[0], 200, "reads are not state changes")
        self.assertEqual(self.call("POST", "/v1/resume", {}, who="agent-ok", csrf=False)[0], 200, "bearer agents need no browser header")

    def test_http_surface(self) -> None:
        self.assertEqual(self.call("GET", "/health", who=None)[1], {"status": "ok", "service": "telegram-settings"})
        for path in ("/v1/nope", "/nope", "/v1/token"):
            self.assertEqual(self.call("GET", path)[0], 404, path)
        self.assertEqual(self.call("PATCH", "/v1/status")[0], 501, "unsupported methods are not routed")
        status, _ = self.call("PUT", "/v1/token", {"token": "x" * 9000})
        self.assertEqual(status, 413)
        request = urllib.request.Request(self.base + "/v1/pause", data=b"x=1", method="POST",
                                         headers={"X-Telegram-Settings": "1", "X-Treer-Identity-Token": "admin", "Content-Type": "text/plain"})
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request)
        self.assertEqual(caught.exception.code, 415)
        response = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(
            urllib.request.Request(self.base + "/health", method="HEAD"))
        self.assertEqual(response.status, 200)
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertIn("script-src 'self'", response.headers["Content-Security-Policy"])
        self.assertEqual(response.headers["X-Content-Type-Options"], "nosniff")

    # --- token --------------------------------------------------------------------------------

    def test_the_token_is_validated_stored_privately_and_never_returned(self) -> None:
        for bad in (None, 5, "", "no-colon", TOKEN + " junk"):
            self.assertEqual(self.call("PUT", "/v1/token", {"token": bad})[1]["error"]["code"], "invalid_token", bad)
        self.settings.limits.hits.clear()            # the shape checks above already used some of the per-minute allowance
        self.bot.behaviour = TelegramError("telegram_request_rejected", "x")
        self.assertEqual(self.call("PUT", "/v1/token", {"token": TOKEN})[1]["error"]["code"], "token_rejected")
        self.bot.behaviour = TelegramError("telegram_unavailable", "x")
        self.assertEqual(self.call("PUT", "/v1/token", {"token": TOKEN})[0], 502)
        self.bot.behaviour = BridgeError("boom " + TOKEN)
        self.assertEqual(self.call("PUT", "/v1/token", {"token": TOKEN})[1]["error"]["code"], "telegram_unavailable")
        self.bot.behaviour = {"id": 999, "is_bot": False, "username": "human_not_bot"}
        self.assertEqual(self.call("PUT", "/v1/token", {"token": TOKEN})[1]["error"]["code"], "token_rejected")
        self.assertFalse(self.settings.tokens.exists(), "nothing is stored unless getMe accepted the token")
        self.bot.behaviour = self.me
        status, body = self.call("PUT", "/v1/token", {"token": f"  {TOKEN}\n"})
        self.assertEqual((status, body), (200, {"bot": {"username": "treer_test_bot", "last4": "0123"}}))
        info = os.lstat(self.settings.tokens.path)
        self.assertEqual(stat.S_IMODE(info.st_mode), 0o600)
        self.assertEqual(self.settings.tokens.read(), TOKEN)
        status, body = self.call("GET", "/v1/status")
        self.assertEqual(body["bot"], {"configured": True, "username": "treer_test_bot", "last4": "0123"})
        self.assertEqual(body["audit"][0]["op"], "token.set")
        self.assertIn("last4:0123", body["audit"][0]["outcome"])
        self.assert_no_token_anywhere()

    def test_error_responses_and_logs_never_echo_the_token(self) -> None:
        self.bot.behaviour = RuntimeError("request to https://api.telegram.org/bot" + TOKEN + "/getMe failed")
        saved, sys.stderr = sys.stderr, open(os.devnull, "w")
        import io
        captured = io.StringIO()
        sys.stderr = captured
        try:
            status, body = self.call("PUT", "/v1/token", {"token": TOKEN})
        finally:
            sys.stderr = saved
        self.assertEqual((status, body["error"]["code"]), (500, "internal_error"))
        self.assertNotIn(TOKEN, captured.getvalue())
        self.assertIn("RuntimeError", captured.getvalue(), "only the exception type is logged")
        self.assert_no_token_anywhere()

    def test_token_attempts_are_rate_limited(self) -> None:
        self.bot.behaviour = TelegramError("telegram_request_rejected", "x")
        codes = [self.call("PUT", "/v1/token", {"token": TOKEN})[0] for _ in range(7)]
        self.assertEqual(codes[:5], [400] * 5)
        self.assertEqual(codes[5:], [429, 429], "the endpoint cannot be used as a token oracle")

    def test_a_different_bot_is_refused_and_clearing_removes_the_token(self) -> None:
        self.save_token()
        self.bot.behaviour = {"id": 555, "is_bot": True, "username": "other_bot"}
        self.assertEqual(self.call("PUT", "/v1/token", {"token": "654321:ZYXWVUTSRQPONMLKJIHGFEDCBA9876"})[1]["error"]["code"], "different_bot")
        self.assertEqual(self.settings.tokens.read(), TOKEN, "the stored token is untouched")
        self.bot.behaviour = self.me
        self.call("POST", "/v1/pairing", {})
        status, body = self.call("DELETE", "/v1/token", {})
        self.assertEqual((status, body), (200, {"cleared": True}))
        self.assertFalse(self.settings.tokens.exists())
        self.assertIsNone(self.settings.state.pairing_status(), "pairing codes die with the token")
        self.assertIsNone(self.call("GET", "/v1/status")[1]["bot"]["last4"])
        self.assertEqual(self.call("GET", "/v1/status")[1]["audit"][0]["op"], "token.clear")
        self.assertEqual(self.call("DELETE", "/v1/token", {})[1], {"cleared": False})

    # --- pairing ------------------------------------------------------------------------------

    def test_pairing_codes(self) -> None:
        self.assertEqual(self.call("POST", "/v1/pairing", {})[1]["error"]["code"], "no_bot")
        self.save_token()
        status, body = self.call("POST", "/v1/pairing", {})
        self.assertEqual((status, body["poc"]), (201, "ag_default"))
        self.assertRegex(body["code"], r"^[A-Z0-9]{5}-[A-Z0-9]{5}$")
        first = body["code"]
        status = self.call("GET", "/v1/status")[1]
        self.assertEqual(status["pairing"]["poc"], "ag_default")
        self.assertNotIn(first, json.dumps(status), "the code is shown once, at creation")
        second = self.call("POST", "/v1/pairing", {"poc": "/team/poc"})[1]
        self.assertEqual(second["poc"], "ag_poc")
        self.assertEqual(self.settings.state.redeem_pairing(first, "1")[0], "invalid", "regenerating kills the old code at once")
        self.assertEqual(self.call("POST", "/v1/pairing", {"poc": "ag_missing"})[1]["error"]["code"], "target_unavailable")
        self.assertEqual(self.call("POST", "/v1/pairing", {"poc": "-n"})[1]["error"]["code"], "invalid_target")
        self.assertEqual(self.call("DELETE", "/v1/pairing", {})[1], {"invalidated": 1})
        self.assertIsNone(self.call("GET", "/v1/status")[1]["pairing"])
        ops = [e["op"] for e in self.call("GET", "/v1/status")[1]["audit"]]
        self.assertIn("pairing.create", ops); self.assertIn("pairing.cancel", ops)
        self.assertEqual(self.call("POST", "/v1/pairing", {}, who="agent-ok")[0], 201)
        creator = self.settings.state.pairing_status()["created_by"]
        self.assertEqual(creator, "agent:ag_ok", "the code is bound to whoever created it")

    # --- POCs, users, pause, status ---------------------------------------------------------------

    def test_poc_user_and_pause_management_is_audited(self) -> None:
        self.assertEqual(self.call("PUT", "/v1/bindings", {"conversation": "1", "to": "ag_x"})[1]["error"]["code"], "no_bot")
        self.save_token()
        status, body = self.call("PUT", "/v1/bindings", {"conversation": "-100:5", "to": "/team/poc", "wake_agent": True})
        self.assertEqual((status, body["binding"]["target_agent_id"], body["binding"]["wake_agent"]), (200, "ag_poc", True))
        self.assertEqual(self.call("PUT", "/v1/bindings", {"conversation": "abc", "to": "ag_x"})[1]["error"]["code"], "invalid_conversation")
        self.assertEqual(self.call("PUT", "/v1/bindings", {"conversation": "1", "to": "ag_missing"})[1]["error"]["code"], "target_unavailable")
        self.assertEqual(self.call("PUT", "/v1/bindings", {"conversation": "1", "to": "ag_x", "wake_agent": "yes"})[1]["error"]["code"], "invalid_field")
        self.assertFalse(self.call("POST", "/v1/bindings/disable", {"conversation": "-100:5"})[1]["binding"]["enabled"])
        self.assertTrue(self.call("POST", "/v1/bindings/enable", {"conversation": "-100:5"})[1]["binding"]["enabled"])
        self.assertEqual(self.call("POST", "/v1/bindings/enable", {"conversation": "-404"})[0], 404)
        status, body = self.call("PUT", "/v1/users", {"user_id": 4242, "label": "  Dana   D ", "enabled": True})
        self.assertEqual(body["user"], {"user_id": "4242", "label": "Dana D", "enabled": True})
        for bad in ({"user_id": "abc", "label": "x", "enabled": True}, {"user_id": 1, "label": "", "enabled": True},
                    {"user_id": 1, "label": "x\u0007", "enabled": True}, {"user_id": 1, "label": "x", "enabled": "yes"}, {"user_id": True, "label": "x", "enabled": True}):
            self.assertEqual(self.call("PUT", "/v1/users", bad)[1]["error"]["code"], "invalid_user", bad)
        self.assertEqual(self.call("POST", "/v1/pause", {})[1], {"paused": True})
        status = self.call("GET", "/v1/status")[1]
        self.assertTrue(status["paused"])
        self.assertEqual({u["user_id"] for u in status["users"]}, {"4242"})
        self.assertEqual(status["bindings"][0]["conversation"], "-100:5")
        ops = [(e["op"], e["actor"], e["via"]) for e in status["audit"]]
        for op in ("poc.set", "poc.disable", "poc.enable", "users.set", "bridge.pause"):
            self.assertIn(op, [o for o, _, _ in ops])
        self.assertTrue(all(actor == "human:usr_admin" for _, actor, via in ops if via == "settings"))
        self.assertEqual(self.call("POST", "/v1/resume", {})[1], {"paused": False})
        self.assertFalse(self.call("GET", "/v1/status")[1]["paused"])

    def test_bridge_state_is_reported_with_a_stale_heartbeat_as_offline(self) -> None:
        self.assertEqual(self.call("GET", "/v1/status")[1]["bridge"]["state"], "offline")
        now = self.settings.clock()
        for state, error in (("needs_token", ""), ("running", ""), ("error", "different_bot")):
            self.settings.state.set_setting("bridge_state", state)
            self.settings.state.set_setting("bridge_error", error)
            self.settings.state.set_setting("bridge_heartbeat", repr(now))
            bridge = self.call("GET", "/v1/status")[1]["bridge"]
            self.assertEqual((bridge["state"], bridge["error"]), (state, error or None))
        self.settings.state.set_setting("bridge_heartbeat", repr(now - 120))
        self.assertEqual(self.call("GET", "/v1/status")[1]["bridge"]["state"], "offline")
        self.settings.state.set_setting("bridge_heartbeat", "not-a-number")
        self.assertEqual(self.call("GET", "/v1/status")[1]["bridge"]["state"], "offline")

    def test_page_text_is_data_not_markup(self) -> None:
        self.save_token()
        self.call("PUT", "/v1/users", {"user_id": 1, "label": "<img src=x onerror=alert(1)>", "enabled": True})
        users = self.call("GET", "/v1/status")[1]["users"]
        self.assertEqual(users[0]["label"], "<img src=x onerror=alert(1)>", "stored verbatim as text for the React page to render as text")

    def test_the_config_for_the_app_is_strict(self) -> None:
        for bad in ({"settings": {"nope": 1}}, {"settings": []}, {"settings": {"admin_agent_ids": "ag"}}, {"settings": {"admin_agent_ids": [1]}}):
            with self.assertRaises(ValueError):
                build_settings(bad, self.state_dir, str(FAKE_TREER))
        self.assertIsNotNone(build_settings({}, self.state_dir, str(FAKE_TREER)))


if __name__ == "__main__":
    unittest.main()
