import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("healthcheck", Path(__file__).parents[1] / "healthcheck.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class HealthTests(unittest.TestCase):
    def test_fixed_loopback_uses_public_host_and_ignores_proxy_env(self):
        response = Mock(status=200)
        connection = Mock()
        connection.getresponse.return_value = response
        with patch.dict(module.os.environ, {"TREER_PROXY_PUBLIC_URL": "https://proxy.example/", "HTTP_PROXY": "http://evil.invalid"}, clear=True):
            with patch.object(module.http.client, "HTTPConnection", return_value=connection) as factory:
                self.assertEqual(module.main(), 0)
        factory.assert_called_once_with("127.0.0.1", 8080, timeout=2)
        connection.request.assert_called_once_with("GET", "/api/health", headers={"Host": "proxy.example"})

    def test_redirect_is_unhealthy_not_followed(self):
        connection = Mock()
        connection.getresponse.return_value = Mock(status=302)
        with patch.dict(module.os.environ, {}, clear=True):
            with patch.object(module.http.client, "HTTPConnection", return_value=connection):
                self.assertEqual(module.main(), 1)
        self.assertEqual(connection.request.call_count, 1)

    def test_credentials_and_invalid_url_refused(self):
        for value in ("", "file:///etc/passwd", "http://user:synthetic@host/"):
            with patch.dict(module.os.environ, {"TREER_PROXY_PUBLIC_URL": value}, clear=True):
                with patch.object(module.http.client, "HTTPConnection") as factory:
                    self.assertEqual(module.main(), 1)
                    factory.assert_not_called()
