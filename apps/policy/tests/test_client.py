"""Private synthetic client config; no real Treer environment or credentials."""
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

CLIENT = Path(__file__).resolve().parents[1] / 'treer-policy'
TOKEN = 'synthetic-private-client-token-at-least-32-characters'


class API(BaseHTTPRequestHandler):
    def do_GET(self): self.handle_request()
    def do_POST(self): self.handle_request()
    def handle_request(self):
        data = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        self.server.calls.append((self.command, self.path, self.headers.get('Authorization'), json.loads(data) if data else None))
        if self.server.behavior == 'redirect':
            self.send_response(302); self.send_header('Location', '/trap'); self.send_header('Content-Length', '0'); self.end_headers(); return
        code = 409 if self.server.behavior == 'error' else 200
        value = {'error': {'code': TOKEN}} if code != 200 else {'ok': True, 'echo': TOKEN}
        body = json.dumps(value).encode()
        self.send_response(code); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *args): pass


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), API)
        self.server.calls = []; self.server.behavior = 'ok'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True); self.thread.start()
        self.config = self.root / 'config.json'
        self.config.write_text(json.dumps({'url': f'http://127.0.0.1:{self.server.server_port}/embedded/', 'token': TOKEN}))
        self.config.chmod(0o600)
        self.request = self.root / 'request.json'; self.request.write_text('{"expected_revision":1,"mode":"monitor","document":{}}')
    def tearDown(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(2); self.temp.cleanup()
    def run_client(self, *args, config=None, stdin=None):
        result = subprocess.run([sys.executable, str(CLIENT), '--config', str(config or self.config), *args], input=stdin, text=True, capture_output=True,
            env={'PATH': os.defpath, 'HOME': str(self.root)}, timeout=5)
        self.assertNotIn(TOKEN, result.stdout + result.stderr)
        self.assertEqual(result.stderr, '')
        return result.returncode, json.loads(result.stdout)
    def test_read_submit_preview_and_secret_response_redaction(self):
        for command, path in [('read', 'v1/state'), ('submit', 'v1/policy/publish'), ('preview', 'v1/policy/explain')]:
            args = [command] if command == 'read' else [command, str(self.request)]
            status, result = self.run_client(*args)
            self.assertEqual(status, 0); self.assertEqual(result['echo'], '[redacted]')
            call = self.server.calls[-1]
            self.assertEqual(call[1], '/embedded/' + path); self.assertEqual(call[2], 'Bearer ' + TOKEN)
            self.assertEqual(call[0], 'GET' if command == 'read' else 'POST')
        self.assertEqual(self.run_client('submit', '-', stdin='{"document":{}}')[0], 0)
    def test_permissions_symlinks_invalid_and_missing_config_fail_closed(self):
        self.config.chmod(0o644); self.assertEqual(self.run_client('read')[0], 1); self.config.chmod(0o600)
        alias = self.root / 'link'; alias.symlink_to(self.config); self.assertEqual(self.run_client('read', config=alias)[0], 1)
        self.root.chmod(0o755); self.assertEqual(self.run_client('read')[0], 1); self.root.chmod(0o700)
        self.config.write_text('{"token":"short"}'); self.assertEqual(self.run_client('read')[0], 1)
        self.config.unlink(); self.assertEqual(self.run_client('read')[0], 1)
        self.assertEqual(self.server.calls, [])
    def test_redirect_errors_and_bounded_json_input(self):
        self.server.behavior = 'redirect'; self.assertEqual(self.run_client('read')[0], 1)
        self.assertFalse(any(c[1] == '/trap' for c in self.server.calls))
        self.server.behavior = 'error'; status, result = self.run_client('read')
        self.assertEqual(status, 1); self.assertEqual(result['status'], 409)
        self.assertEqual(result['error']['code'], '[redacted]')
        before = len(self.server.calls)
        for value in ('[]', 'not-json', '{"body":"' + 'x' * (301 * 1024) + '"}'):
            self.request.write_text(value); self.assertEqual(self.run_client('submit', str(self.request))[0], 1)
        self.assertEqual(len(self.server.calls), before)


if __name__ == '__main__': unittest.main()
