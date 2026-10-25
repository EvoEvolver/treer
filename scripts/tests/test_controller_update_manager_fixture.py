"""No manager registration, account mutation or topology execution in these tests."""
import importlib.util
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

MODULE = Path(__file__).resolve().parents[1] / 'controller-update-manager-fixture.py'
spec = importlib.util.spec_from_file_location('manager_fixture', MODULE)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class ManagerPreparationTests(unittest.TestCase):
    def test_plans_are_private_exclusive_and_never_start_processes(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(fixture.subprocess, 'Popen', side_effect=AssertionError('must not spawn')):
            for manager in ('systemd', 'launchd'):
                root = Path(temporary).resolve() / manager
                plan = fixture.prepare(root, manager)
                self.assertEqual(plan['native_update_acceptance'], 'not_run')
                self.assertEqual(plan['manager_acceptance'], 'not_run')
                self.assertEqual(root.stat().st_mode & 0o777, 0o700)
                self.assertEqual((root / 'plan.json').stat().st_mode & 0o777, 0o600)
                self.assertFalse((root / 'worker.json').exists())
                self.assertFalse((root / 'controller.json').exists())
                self.assertIn('--disposable', plan['host_argv'])
                if manager == 'systemd':
                    self.assertIn('--property=KillMode=control-group', plan['manual_start_argv'])
                    self.assertIn('--user', plan['manual_stop_argv'])
                else:
                    data = plistlib.loads(next(root.glob('*.plist')).read_bytes())
                    self.assertNotIn('KeepAlive', data)
                    self.assertEqual(data['ProgramArguments'], plan['host_argv'])
                    self.assertEqual(plan['manual_kickstart_argv'][1], 'kickstart')
                with self.assertRaises(FileExistsError):
                    fixture.prepare(root, manager)

    def test_managed_agent_and_missing_disposable_ack_refuse_before_spawn(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(fixture.subprocess, 'Popen', side_effect=AssertionError('must not spawn')):
            root = Path(temporary).resolve() / 'case'
            fixture.prepare(root, 'launchd')
            with self.assertRaisesRegex(ValueError, 'requires_explicit_disposable'):
                fixture.host(root, False)
            with patch.dict(os.environ, {'TREER_AGENT_ID': 'inert-fixture'}):
                with self.assertRaisesRegex(ValueError, 'requires_explicit_disposable'):
                    fixture.host(root, True)

    def test_non_temporary_and_alias_roots_are_not_adopted(self):
        with self.assertRaises(ValueError):
            fixture.checked_root(str(Path.home()))
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary).resolve()
            actual = parent / 'actual'
            actual.mkdir()
            alias = parent / 'alias'
            alias.symlink_to(actual, target_is_directory=True)
            with self.assertRaises(ValueError):
                fixture.prepare(alias, 'launchd')
            with self.assertRaises(ValueError):
                fixture.checked_root(str(parent / 'actual' / '..' / 'next'))


if __name__ == '__main__':
    unittest.main()
