import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SCRIPTS = Path(__file__).resolve().parents[1]


class CookieTest(unittest.TestCase):
    def test_cookie_headers_and_failed_login_without_a_disk_cookie_jar(self):
        origins = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                origins.append(self.headers.get("Origin"))
                self.send_response(401 if self.path == "/denied" else 200)
                if self.path == "/login":
                    self.send_header("Set-Cookie", "session=fake; Path=/; HttpOnly")
                    self.send_header("set-cookie", "other=test; Secure")
                self.end_headers()
                self.wfile.write(b'{"ok":true}')

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as directory:
            for route, expected in (("login", "session=fake; other=test"), ("denied", None), ("empty", None)):
                result = subprocess.run(["sh", "-c", '. "$1"; canary_login_cookies "$2"',
                                         "cookie-test", str(SCRIPTS / "canary-auth.sh"),
                                         f"http://127.0.0.1:{server.server_port}/{route}"],
                                        input='{"password":"fake"}', text=True,
                                        capture_output=True, cwd=directory,
                                        env={**os.environ, "NO_PROXY": "127.0.0.1", "canary_console_origin": "https://app.example.test"})
                if expected:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.strip(), expected)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
            self.assertEqual(list(Path(directory).iterdir()), [])
        self.assertEqual(set(origins), {"https://app.example.test"})

    def test_canary_cookie_writes_carry_the_configured_console_origin(self):
        script = (SCRIPTS / "test-canary.sh").read_text()
        self.assertIn("TREER_CANARY_APP_URL", script)
        self.assertEqual(script.count('-H "Origin: $canary_console_origin"'), 3)
        self.assertNotIn("proxy_url%", script.split("canary_console_origin=", 1)[1].split("\n", 1)[0])


if __name__ == "__main__":
    unittest.main()
