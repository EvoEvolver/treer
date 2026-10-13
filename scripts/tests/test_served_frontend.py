import importlib.util
from pathlib import Path
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

spec = importlib.util.spec_from_file_location('served_frontend', Path(__file__).resolve().parents[1] / 'verify-served-frontend.py')
frontend = importlib.util.module_from_spec(spec)
spec.loader.exec_module(frontend)
NONCE = 'Abcdefghijklmnopqrstuvwxyz012345'
CSP = f"default-src 'self'; script-src 'nonce-{NONCE}'; object-src 'none'"
CANDIDATE = b'<!doctype html>\n<script>const fake = "<script>";</script>\n<script type="module" crossorigin>run()</script>\n'
SERVED = (f'<!doctype html>\n<script nonce="{NONCE}">const fake = "<script>";</script>\n'
          f'<script type="module" crossorigin nonce="{NONCE}">run()</script>\n').encode()


class FrontendTest(unittest.TestCase):
    def test_exact_worker_transform_ignores_tag_lookalikes_in_javascript(self):
        self.assertEqual(frontend.expected_html(CANDIDATE, CSP), SERVED)
        frontend.verify(CANDIDATE, SERVED, CSP)

    def test_preserves_unicode_crlf_and_quoted_attribute_tag_lookalikes(self):
        candidate = '<title>中文\u2028</title>\r\n<script data-example="<script>">ok()</script>\r\n'.encode()
        served = (f'<title>中文\u2028</title>\r\n<script data-example="<script>" nonce="{NONCE}">ok()</script>\r\n').encode()
        frontend.verify(candidate, served, CSP)

    def test_rejects_every_change_other_than_nonce_insertion(self):
        for changed in [SERVED.replace(b'run()', b'evil()'), SERVED + b' ',
                        SERVED.replace(b'<!doctype html>', b'<!DOCTYPE html>'),
                        SERVED.replace(f' nonce="{NONCE}"'.encode(), b'', 1),
                        SERVED.replace(NONCE.encode(), b'Z' * 32, 1),
                        SERVED.replace(b'"<script>"', f'"<script nonce=\"{NONCE}\">"'.encode()),
                        SERVED.replace(b'crossorigin', b'crossorigin data-extra="yes"')]:
            with self.subTest(changed=changed[:100]), self.assertRaises(ValueError):
                frontend.verify(CANDIDATE, changed, CSP)

    def test_rejects_weakened_ambiguous_or_invalid_script_policy(self):
        for csp in ['', "script-src 'unsafe-inline'", CSP + "; script-src 'unsafe-inline'",
                    CSP + "; script-src-elem 'unsafe-inline'", CSP.replace(NONCE, 'short'),
                    CSP.replace(NONCE, '<' * 32), CSP.replace("'; object", "' 'unsafe-inline'; object")]:
            with self.subTest(csp=csp), self.assertRaises(ValueError):
                frontend.verify(CANDIDATE, SERVED, csp)

    def test_rejects_unsupported_candidate_shapes(self):
        for candidate in [b'<script/>', b'<script nonce="existing">ok()</script>', b'<p>no script</p>']:
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                frontend.expected_html(candidate, CSP)

    def test_http_response_contract_and_no_redirects(self):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(302 if self.path == '/redirect' else 200)
                self.send_header('content-type', 'text/plain' if self.path == '/plain' else 'text/html')
                if self.path != '/missing':
                    self.send_header('content-security-policy', CSP)
                if self.path == '/duplicate':
                    self.send_header('content-security-policy', CSP)
                if self.path == '/redirect':
                    self.send_header('location', '/')
                self.end_headers()
                self.wfile.write(SERVED)

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        base = f'http://127.0.0.1:{server.server_port}'
        frontend.verify_url(CANDIDATE, base + '/')
        for path in ['/plain', '/missing', '/duplicate', '/redirect']:
            with self.subTest(path=path), self.assertRaises(ValueError):
                frontend.verify_url(CANDIDATE, base + path)
        for url in ['http://example.invalid/', 'https://user:secret@example.invalid/',
                    base + '/?token=synthetic', base + '/#fragment']:
            with self.subTest(url=url), self.assertRaises(ValueError):
                frontend.verify_url(CANDIDATE, url)


if __name__ == '__main__':
    unittest.main()
