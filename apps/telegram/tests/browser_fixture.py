"""Hermetic settings App for the browser tests: real server, real CSRF/Origin logic, Treer verification replaced by a table
(admin, member), a fake Telegram getMe and the fake treer CLI. Prints its URL as JSON and serves until killed."""
import json
import os
import sys
import tempfile
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE))
from settings_app.auth import Auth, Error
from settings_app.server import Application, Server, build_settings

PEOPLE = {"admin": {"kind": "human", "id": "usr_admin", "name": "Ada", "role": "admin"},
          "member": {"kind": "human", "id": "usr_member", "name": "Mel", "role": "member"}}


class FakeAuth(Auth):
    def verify(self, token):
        if token not in PEOPLE:
            raise Error(401, "identity_invalid", "Identity is inactive or outside this App's workspace")
        return PEOPLE[token], list(PEOPLE.values())


class FakeBot:
    def __init__(self, token):
        self.token = token

    def get_me(self):
        from telegram_app.common import TelegramError
        if self.token.endswith("REJECTED000000"):
            raise TelegramError("telegram_request_rejected", "x")
        return {"id": 999, "is_bot": True, "username": "treer_test_bot"}


def main():
    assert not any(k.startswith(("TREER_", "AGENT_UI_")) for k in os.environ), "non-hermetic fixture"
    with tempfile.TemporaryDirectory(prefix="telegram-settings-") as directory:
        root = Path(directory)
        cli_state = root / "cli.json"
        cli_state.write_text("{}")
        fake = HERE / "fixtures/fake_treer.py"
        fake.chmod(0o755)
        os.environ.update(FAKE_TREER_STATE=str(cli_state), FAKE_TREER_PATHS=json.dumps({"/team/poc": "ag_poc"}))
        settings = build_settings({"settings": {"default_poc": "ag_default"}}, root / "state", str(fake), bot_factory=FakeBot)
        application = Application(settings)
        server = Server(("127.0.0.1", 0), application)
        # The App's public origin is the one the browser talks to; the Origin check is exercised for real.
        application.auth = FakeAuth("http://127.0.0.1:9/", "svc_tg", "ws", f"http://127.0.0.1:{server.server_port}/")
        threading.Thread(target=server.serve_forever, daemon=True).start()
        print(json.dumps({"url": f"http://127.0.0.1:{server.server_port}/"}), flush=True)
        threading.Event().wait()


if __name__ == "__main__":
    main()
