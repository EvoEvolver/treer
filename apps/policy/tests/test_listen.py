"""Startup bind regression; no sockets or inherited runtime environment."""
import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('listen_policy', Path(__file__).resolve().parents[1] / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class ListenTests(unittest.TestCase):
    def assert_startup_bind(self, argv, expected, listen=None):
        with tempfile.TemporaryDirectory() as root:
            environment = {'TREER_WORKSPACE_ID': 'ws_test',
                           'POLICY_DATA_FILE': str(Path(root) / 'policy.json')}
            if listen is not None:
                environment['POLICY_LISTEN'] = listen
            previous_umask = os.umask(0o077)
            try:
                with patch.dict(os.environ, environment, clear=True), \
                        patch.object(sys, 'argv', ['policy.py', *argv]), \
                        patch.object(policy, 'PolicyServer') as server, \
                        contextlib.redirect_stdout(io.StringIO()):
                    policy.main()
                    server.assert_called_once_with(expected, policy.PolicyHandler)
                    server.return_value.serve_forever.assert_called_once_with()
            finally:
                os.umask(previous_umask)

    def test_default_is_loopback(self):
        self.assert_startup_bind([], ('127.0.0.1', 8787))

    def test_port_argument_keeps_loopback(self):
        self.assert_startup_bind(['--port', '43210'], ('127.0.0.1', 43210))

    def test_explicit_listen_overrides_host_and_port(self):
        self.assert_startup_bind(['--port', '43210'], ('0.0.0.0', 23456), '0.0.0.0:23456')


if __name__ == '__main__':
    unittest.main()
