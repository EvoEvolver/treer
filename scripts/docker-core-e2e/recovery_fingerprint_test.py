"""Synthetic file controls only; no installed configuration or process mutation."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from recovery_fingerprint import projection, read_private


class FingerprintTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        directory = self.home / '.config/treer/agent-servers'
        directory.mkdir(parents=True)
        self.controller = directory / 'fixture-controller.json'
        self.controller.write_text(json.dumps(dict(workspace='ws', server_id='server', service_manager='nohup',
            host_socket=str(self.home / 'absent.sock'),listen='127.0.0.1:1',
            machine_token='SYNTHETIC-MACHINE-ONLY',operator_credential='SYNTHETIC-OPERATOR-ONLY')))
        (directory / 'fixture-host.json').write_text('{"synthetic":true}')
        identity = self.home / '.local/state/treer/machines/node/machine-identity.json'
        identity.parent.mkdir(parents=True)
        identity.write_text('{"synthetic":true}')

    def test_missing_host_projects_hashes_without_credentials(self):
        value = projection(self.home, 'ws', 'server')
        self.assertEqual(len(value['registration_hashes']), 3)
        self.assertFalse(value['host_reachable'])
        self.assertFalse(value['host_record_matches_process'])
        self.assertTrue(value['credentials_present'])
        self.assertNotIn('SYNTHETIC', json.dumps(value))
        self.assertNotIn(str(self.home), json.dumps(value))

    def test_identity_and_ambiguous_registration_refused(self):
        with self.assertRaises(ValueError):
            projection(self.home, 'other', 'server')
        self.controller.with_name('second-controller.json').write_text('{}')
        with self.assertRaises(ValueError):
            projection(self.home, 'ws', 'server')

    def test_symlink_large_and_fifo_refused_without_blocking(self):
        link = self.home / 'link'
        link.symlink_to(self.controller)
        with self.assertRaises(ValueError):
            read_private(link, self.home)
        large = self.home / 'large'
        large.write_bytes(b'x' * 65537)
        with self.assertRaises(ValueError):
            read_private(large, self.home)
        fifo = self.home / 'fifo'
        os.mkfifo(fifo)
        with self.assertRaises(ValueError):
            read_private(fifo, self.home)

    def test_cli_requires_explicit_fixture_opt_in_before_read(self):
        env = {key:value for key,value in os.environ.items() if key != 'TREER_SYNTHETIC_RECOVERY_226'}
        result = subprocess.run([sys.executable, str(Path(__file__).with_name('recovery_fingerprint.py')), 'ws', 'server'],
            env=env, capture_output=True, text=True, timeout=3)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, '')
        self.assertEqual(result.stderr.strip(), 'synthetic Docker recovery fixture only')


if __name__ == '__main__':
    unittest.main()
