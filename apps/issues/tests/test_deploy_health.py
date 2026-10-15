import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from hermetic import hermetic_env

ROOT = Path(__file__).resolve().parents[1]
clean = hermetic_env(); os.environ.clear(); os.environ.update(clean)
sys.path.insert(0, str(ROOT))
from issues import Application, Server, parse_args
from store import Store


class DeploymentHealthTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-health-')
        self.store = Store(Path(self.directory.name) / 'app.sqlite3')
        self.server = Server(('127.0.0.1', 0), Application(self.store))   # no identity provider: the route must not need one
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f'http://127.0.0.1:{self.server.server_port}/'

    def tearDown(self):
        self.server.shutdown(); self.server.server_close()
        for name in ('APP_DEPLOY_VERSION', 'TREER_AGENT_ID'):
            os.environ.pop(name, None)
        self.directory.cleanup()

    def get(self, path='deployment-health', method='GET', headers=None):
        request = urllib.request.Request(self.base + path, method=method, headers=headers or {})
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=5) as response:
                return response.status, dict(response.headers), response.read()
        except urllib.error.HTTPError as exc:
            return exc.code, dict(exc.headers), exc.read()

    def test_reports_version_app_id_and_a_working_database_without_authentication(self):
        os.environ.update(APP_DEPLOY_VERSION='a' * 40, TREER_AGENT_ID='app_stable')
        status, headers, body = self.get()
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'version': 'a' * 40, 'app_id': 'app_stable', 'ready': True, 'database_ready': True})
        self.assertEqual(headers['Cache-Control'], 'no-store')
        self.assertIn('application/json', headers['Content-Type'])
        self.assertEqual(self.get(method='HEAD')[0], 200)
        self.assertEqual(self.get(headers={'Origin': 'https://evil.example', 'Cookie': 'x=1'})[0], 200, 'no identity of any kind is consulted')

    def test_outside_a_deployment_the_fields_are_null_not_invented(self):
        _, _, body = self.get()
        self.assertEqual(json.loads(body), {'version': None, 'app_id': None, 'ready': True, 'database_ready': True})

    def test_an_unusable_database_is_not_ready(self):
        os.chmod(self.store.path, 0)
        try:
            Path(self.store.path).rename(Path(self.store.path).with_name('gone'))
            Path(self.store.path).mkdir()                      # a directory where the database file should be
            status, _, body = self.get()
        finally:
            Path(self.store.path).rmdir()
        self.assertEqual(status, 503)
        self.assertEqual((json.loads(body)['ready'], json.loads(body)['database_ready']), (False, False))

    def test_no_other_methods_and_the_old_health_route_still_works(self):
        request = urllib.request.Request(self.base + 'deployment-health', method='POST', data=b'{}', headers={'Content-Type': 'application/json'})
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5)
        self.assertEqual(caught.exception.code, 404)
        self.assertEqual(json.loads(self.get('health')[2]), {'status': 'ok', 'service': 'issues'})

    def test_the_database_probe_is_memoised_briefly(self):
        application = self.server.application
        probes = []
        real = self.store.connect
        self.store.connect = lambda: probes.append(1) or real()
        try:
            clock = iter([100.0, 100.5, 101.9, 102.1]).__next__
            for _ in range(3):
                self.assertTrue(application.deployment_health(clock)['ready'])
            self.assertEqual(len(probes), 1, 'requests within the TTL share one probe')
            self.assertTrue(application.deployment_health(clock)['ready'])
            self.assertEqual(len(probes), 2, 'after the TTL the database is probed again')
        finally:
            self.store.connect = real

    def test_the_cache_does_not_hide_a_failure_beyond_the_ttl(self):
        application = self.server.application
        self.assertTrue(application.deployment_health(lambda: 0.0)['ready'])
        real = self.store.connect
        def broken():
            raise RuntimeError('db gone')
        self.store.connect = broken
        try:
            self.assertTrue(application.deployment_health(lambda: 1.0)['ready'], 'within the TTL: cached')
            self.assertFalse(application.deployment_health(lambda: 3.0)['ready'], 'after the TTL: the failure is visible')
        finally:
            self.store.connect = real

    def test_proxy_url_argument(self):
        self.assertEqual(parse_args(['--proxy-url', 'https://proxy.example/']).proxy_url, 'https://proxy.example/')
        self.assertIsNone(parse_args([]).proxy_url)
        with self.assertRaises(SystemExit):
            parse_args(['--nope'])


if __name__ == '__main__':
    unittest.main()
