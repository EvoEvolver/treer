import contextlib
import io
import os
from pathlib import Path
import secrets
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).parents[2]))
from http_app import client

class ClientTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.path=Path(self.tmp.name)/'token'
        self.token=secrets.token_urlsafe(32);self.path.write_text(self.token);self.path.chmod(0o600)
    def tearDown(self):self.tmp.cleanup()
    def call(self, url='https://endpoint.example/', path=None):
        with patch.object(sys,'argv',['client.py',url,str(path or self.path),'outbox']):client.main()
    def test_private_token_and_no_redirect_handler(self):
        response=contextlib.nullcontext(io.BytesIO(b'{"items":[]}'))
        class Output:
            buffer=io.BytesIO()
        with patch.object(client.urllib.request,'build_opener') as opener, patch.object(sys,'stdout',Output()):
            opener.return_value.open.return_value=response
            self.call()
            request=opener.return_value.open.call_args.args[0]
            self.assertEqual(request.get_header('Authorization'),'Bearer '+self.token)
            self.assertNotIn(self.token,request.full_url)
            self.assertIsInstance(opener.call_args.args[0],client.NoRedirect)
            self.assertIsNone(opener.call_args.args[0].redirect_request())
    def test_insecure_urls_permissions_and_symlinks_rejected_before_network(self):
        with patch.object(client.urllib.request,'build_opener') as opener:
            for url in ('http://endpoint.example/','https://u:p@endpoint.example/','https://endpoint.example/?token=x'):
                with self.assertRaises(ValueError):self.call(url)
            self.path.chmod(0o644)
            with self.assertRaises(ValueError):self.call()
            self.path.chmod(0o600)
            link=self.path.with_name('link');link.symlink_to(self.path)
            with self.assertRaises(OSError):self.call(path=link)
            opener.assert_not_called()
