import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_endpoint import e, FakeAPI
import tempfile
import threading
import json

state = tempfile.TemporaryDirectory()
c = e.mail.Config(
    "127.0.0.1",
    0,
    "svc_test",
    "http://127.0.0.1/",
    "http://fake.invalid/",
    e.ROOT / "web",
    Path(state.name) / "state.sqlite3",
)
api = FakeAPI()
app = e.Application(e.Settings(c, "ws_test", {"u1": "alice@example.test"}, [], []), api)
server = e.Server(("127.0.0.1", 0), app)
origin = f"http://127.0.0.1:{server.server_address[1]}"
app.config = e.mail.Config(
    "127.0.0.1",
    server.server_address[1],
    "svc_test",
    origin + "/",
    "http://fake.invalid/",
    e.ROOT / "web",
    Path(state.name) / "state.sqlite3",
)
n = app.notify(
    "agent",
    {
        "to": "u1",
        "subject": "A decision needs your review",
        "body": "We have finished the proposal. Please review the next steps.\n<script>window.XSS=true</script>",
    },
    "browser-seed",
)
app.deliver()
# Simulate the authenticated browser tunnel prefix in addition to direct origin.
original = e.Handler._dispatch


def prefixed(handler, method):
    handler.path = handler.path.removeprefix("/tunnel")
    return original(handler, method)


e.Handler._dispatch = prefixed
print(json.dumps({"url": origin, "thread": n["thread_id"]}), flush=True)
server.serve_forever()
