"""Local private capability lifecycle; synthetic files only, closed environment."""
import concurrent.futures
import contextlib
import importlib.util
import io
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('admin_file_policy', Path(__file__).resolve().parents[1] / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)

class AdminFileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.source = policy.AdminTokenFile(self.root / '.treer-policy-admin-token')
        self.env = patch.dict(os.environ, {}, clear=True);self.env.start()
    def tearDown(self): self.env.stop();self.temp.cleanup()
    def test_private_generation_is_preserved_and_loss_never_reprovisions(self):
        self.source.initialize();original = self.source.read()
        self.assertGreaterEqual(len(original),43)
        self.assertEqual(stat.S_IMODE(self.source.path.stat().st_mode),0o600)
        self.assertEqual((self.root/self.source.marker).read_text(),'initialized\n')
        self.source.initialize();self.assertEqual(self.source.read(),original)
        self.source.path.unlink();self.source.initialize()
        self.assertFalse(self.source.path.exists())
        self.source.rotate();self.assertNotEqual(self.source.read(),original)
    def test_atomic_rotation_and_missing_file_lock_live_operator_requests(self):
        self.source.initialize();original=self.source.read()
        handler=policy.PolicyHandler.__new__(policy.PolicyHandler)
        class Server: pass
        handler.server=Server();handler.server.admin_token_file=self.source
        self.assertEqual(handler._operator_token(),original)
        self.source.rotate();new=self.source.read();self.assertNotEqual(new,original)
        self.assertEqual(handler._operator_token(),new)
        self.source.path.unlink();self.assertIsNone(handler._operator_token())
        self.source.path.write_text(original);self.source.path.chmod(0o644)
        self.assertIsNone(handler._operator_token())
    def test_malformed_symlink_directory_and_wrong_owner_fail_closed(self):
        self.source.initialize();original=self.source.read()
        self.source.path.chmod(0o644)
        with self.assertRaises(ValueError): self.source.read()
        with self.assertRaises(ValueError): self.source.initialize()
        self.source.path.chmod(0o600)
        with patch.object(policy.os,'geteuid',return_value=os.geteuid()+1):
            with self.assertRaises(ValueError): self.source.read()
        self.source.path.unlink();self.source.path.symlink_to(self.root/'victim')
        (self.root/'victim').write_text(original)
        for action in (self.source.read,self.source.initialize,self.source.rotate):
            with self.assertRaises(OSError): action()
        self.assertEqual((self.root/'victim').read_text(),original)
        self.source.path.unlink();self.source.path.mkdir()
        with self.assertRaises(ValueError): self.source.read()
    def test_parent_symlink_rejected(self):
        (self.root/'link').symlink_to(self.root,target_is_directory=True)
        with self.assertRaises(OSError): policy.AdminTokenFile(self.root/'link'/'token').initialize()
    def test_bad_content_never_echoed(self):
        self.source.initialize()
        for bad in ('short','secret-with-newline\n'*3,'x'*8193,'密'*40):
            self.source.path.write_text(bad)
            with self.assertRaises(ValueError) as result:self.source.read()
            self.assertNotIn(bad,str(result.exception))
    def test_concurrent_initialize_never_overwrites(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(lambda _:self.source.initialize(),range(8)))
        token=self.source.read()
        self.source.initialize();self.assertEqual(self.source.read(),token)
        self.assertFalse(list(self.root.glob('.policy-admin-*')))
    def test_precedence_defaults_and_explicit_file(self):
        with patch.object(policy,'APP_ROOT',self.root),patch.object(policy,'BUNDLED_INSTALL',False):
            self.assertEqual(policy.admin_token_source(),(None,None))
            self.assertFalse(self.source.path.exists())
            os.environ['POLICY_ADMIN_TOKEN_FILE']=str(self.source.path)
            token,source=policy.admin_token_source();self.assertIsNone(token);self.assertEqual(source.path,self.source.path)
            os.environ['POLICY_ADMIN_TOKEN']='synthetic-environment-token-32-chars'
            self.assertEqual(policy.admin_token_source(initialize=True),(os.environ['POLICY_ADMIN_TOKEN'],None))
            os.environ['POLICY_ADMIN_TOKEN']=''
            with self.assertRaises(ValueError): policy.admin_token_source()
            self.assertFalse(self.source.path.exists())
            del os.environ['POLICY_ADMIN_TOKEN'];del os.environ['POLICY_ADMIN_TOKEN_FILE']
            with patch.object(policy,'BUNDLED_INSTALL',True):
                _,source=policy.admin_token_source(initialize=True)
                self.assertEqual(source.path,self.source.path)
                self.assertTrue(source.read())
    def test_bundled_startup_initializes_once_without_secret_output(self):
        os.environ['TREER_WORKSPACE_ID']='ws_test'
        os.environ['POLICY_DATA_FILE']=str(self.root/'policy.json')
        with patch.object(policy,'APP_ROOT',self.root),patch.object(policy,'BUNDLED_INSTALL',True),patch.object(sys,'argv',['policy.py','--initialize-admin-token-file']),patch.object(policy,'PolicyServer') as server,contextlib.redirect_stdout(io.StringIO()) as output:
            policy.main();original=self.source.read()
            self.assertNotIn(original,output.getvalue())
            server.return_value.admin_token_file.path.unlink()
            policy.main()
            self.assertFalse(self.source.path.exists())
            self.assertIsNone(server.return_value.admin_token)

    def test_rotation_command_exits_without_server_or_secret_output(self):
        os.environ['POLICY_ADMIN_TOKEN_FILE']=str(self.source.path)
        with patch.object(sys,'argv',['policy.py','--rotate-admin-token-file']),patch.object(policy,'PolicyServer') as server,contextlib.redirect_stdout(io.StringIO()) as output:
            policy.main();server.assert_not_called()
        self.assertNotIn(self.source.read(),output.getvalue())
        self.assertIn('no token is returned',output.getvalue())

if __name__=='__main__':unittest.main()
