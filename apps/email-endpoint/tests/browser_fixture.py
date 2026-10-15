import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_endpoint import e, FakeAPI
import tempfile
import threading
import json
import base64
import hashlib
import hmac
import http.client
import secrets
import time
import urllib.parse
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

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
app = e.Application(e.Settings(c, "ws_test", {"u1": "alice@example.test"}, [], [], runtime_id="app_test", contacts=[{"id":"outside","label":"Outside contact","email":"external@example.test"}]), api, tree_loader=lambda: api.tree, token_loader=lambda: "endpoint-token")
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
app.activate(e.mail.BrowserSession("", "admin", "ws_test", "svc_test", "u1", "Alice", "owner", 9999999999), {"enabled":True,"folder_id":"f1"})
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
# Authenticated local gateway reproduces the platform forwarding boundary.
# Its signing key is test-only, ephemeral, and never inherited from Treer.
signing_key = secrets.token_bytes(32)
issued = set()
verified = set()
original_api_request = api.request
oauth_codes = {}


def decode_token(token):
    encoded, signature = token.split(".")
    expected = hmac.new(signing_key, encoded.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(signature, expected):
        raise ValueError("invalid signature")
    claims = json.loads(base64.urlsafe_b64decode(encoded))
    if claims["exp"] <= time.time() or claims["aud"] != "svc_test":
        raise ValueError("expired or wrong audience")
    return claims


def signed_api(method, path, access_token=None, body=None, form=None):
    if path.endswith("/oauth/token"):
        challenge = oauth_codes.pop(form["code"], None)
        actual = base64.urlsafe_b64encode(hashlib.sha256(form["code_verifier"].encode()).digest()).rstrip(b"=").decode()
        if not challenge or challenge != actual or form["redirect_uri"] != app.config.callback_url:
            raise e.mail.AppApiError("invalid_grant", "invalid grant")
        return original_api_request(method, path, form=form | {"code":"member-code"})
    if path.endswith("identity/verify") and "." in body["token"]:
        try:
            claims = decode_token(body["token"])
        except (ValueError, KeyError):
            return {"active": False}
        verified.add(claims["jti"])
        result = original_api_request(method, path, body={"token": claims.get("principal", "human")})
        result["claims"]["exp"] = claims["exp"]
        return result
    if access_token and "." in access_token:
        decode_token(access_token)
        access_token = decode_token(access_token).get("principal", "human")
    return original_api_request(
        method, path, access_token=access_token, body=body, form=form
    )


api.request = signed_api
prefix = "/api/workspaces/ws_test/apps/app_test/ui"


class Gateway(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def forward(self):
        if self.path.startswith("/api/apps/oauth/authorize?"):
            if "treer_test_platform=logged-in" not in self.headers.get("Cookie", ""):
                self.send_error(401); return
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            if query.get("client_id") != ["svc_test"] or query.get("redirect_uri") != [app.config.callback_url] or query.get("code_challenge_method") != ["S256"]:
                self.send_error(400); return
            code = secrets.token_urlsafe(32)
            oauth_codes[code] = query["code_challenge"][0]
            self.send_response(302)
            self.send_header("Location", app.config.callback_url + "?" + urllib.parse.urlencode({"code":code,"state":query["state"][0]}))
            self.send_header("Content-Length", "0")
            self.end_headers(); return
        if self.path == "/control-plane/":
            content = (
                '<!doctype html><iframe title="Email endpoint" style="width:100%;height:100vh;border:0" src="'
                + prefix
                + '/"></iframe>'
            ).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        if self.path == "/gateway/stats":
            content = json.dumps(
                {
                    "issued": len(issued),
                    "verified": len(verified),
                    "oauth_requests": sum(
                        1 for _, p, _, _ in api.requests if "oauth" in p
                    ),
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        if "treer_test_platform=logged-in" not in self.headers.get("Cookie", ""):
            self.send_error(401)
            return
        claims = {
            "principal": "admin" if "treer_test_platform=logged-in-admin" in self.headers.get("Cookie", "") else "human",
            "aud": "svc_test",
            "exp": time.time() + 60,
            "jti": secrets.token_hex(12),
        }
        issued.add(claims["jti"])
        encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode()
        token = (
            encoded
            + "."
            + hmac.new(signing_key, encoded.encode(), hashlib.sha256).hexdigest()
        )
        headers = {
            k: v
            for k, v in self.headers.items()
            if not k.lower().startswith("x-treer-")
            and k.lower() not in ("cookie", "authorization", "host", "connection")
        }
        headers["X-Treer-Identity-Token"] = token
        headers["Host"] = "rewritten-upstream.invalid"
        content = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        conn = http.client.HTTPConnection(
            "127.0.0.1", server.server_address[1], timeout=10
        )
        path = self.path.removeprefix(prefix)
        conn.request(
            self.command, path, body=content if content else None, headers=headers
        )
        response = conn.getresponse()
        payload = response.read()
        self.send_response(response.status)
        for k, v in response.getheaders():
            if k.lower() not in ("content-length", "connection", "server", "date"):
                if k.lower() == "content-security-policy":
                    v = v.replace("frame-ancestors 'none'", "frame-ancestors 'self'")
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        conn.close()


gateway = ThreadingHTTPServer(("127.0.0.1", 0), Gateway)
gateway_origin = f"http://127.0.0.1:{gateway.server_address[1]}"
app.settings.browser_origins = (gateway_origin,)
app.config = replace(app.config, proxy_public_url=gateway_origin+"/")
threading.Thread(target=server.serve_forever, daemon=True).start()
print(
    json.dumps({"url": origin, "thread": n["thread_id"], "gateway": gateway_origin}),
    flush=True,
)
gateway.serve_forever()
