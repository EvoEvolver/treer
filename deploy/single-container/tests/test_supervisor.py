"""Hermetic unit/process tests: no Treer environment, sockets, or databases."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("single_supervisor", Path(__file__).parents[1] / "supervisor.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PrivateDataTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.secret = self.root / "secret"

    def tearDown(self):
        self.tmp.cleanup()

    def test_random_per_volume_and_restart_stable(self):
        a = module.persistent_secret(self.secret, first_start=True)
        b = module.persistent_secret(self.root / "other", first_start=True)
        self.assertNotEqual(a, b)
        self.assertEqual(module.persistent_secret(self.secret), a)
        self.assertEqual(self.secret.stat().st_mode & 0o777, 0o600)

    def test_missing_persisted_secret_fails_closed(self):
        with self.assertRaisesRegex(module.StartupError, "missing"):
            module.persistent_secret(self.secret)
        self.assertFalse(self.secret.exists())

    def test_database_password_cannot_silently_rotate(self):
        module.persistent_secret(self.secret, "first", first_start=True)
        with self.assertRaisesRegex(module.StartupError, "differs"):
            module.persistent_secret(self.secret, "second")
        self.assertEqual(module.read_secret(self.secret), "first")

    def test_symlink_and_hardlink_rejected(self):
        target = self.root / "target"
        module.write_secret(target, "synthetic-test-value")
        self.secret.symlink_to(target)
        with self.assertRaises(module.StartupError):
            module.read_secret(self.secret)
        self.secret.unlink()
        os.link(target, self.secret)
        with self.assertRaises(module.StartupError):
            module.read_secret(self.secret)

    def test_world_readable_file_rejected(self):
        module.write_secret(self.secret, "synthetic-test-value")
        self.secret.chmod(0o644)
        with self.assertRaises(module.StartupError):
            module.read_secret(self.secret)

    def test_non_regular_and_bad_values_rejected(self):
        self.secret.mkdir()
        with self.assertRaises(module.StartupError):
            module.read_secret(self.secret)
        self.secret.rmdir()
        os.mkfifo(self.secret, 0o600)
        with self.assertRaises(module.StartupError):
            module.read_secret(self.secret)
        self.secret.unlink()
        for value in ("", "line\nline", "line\rline", "nul\x00", "x" * 16385):
            module.write_secret(self.secret, value)
            with self.assertRaises(module.StartupError):
                module.read_secret(self.secret)
            self.secret.unlink()

    def test_env_precedence_and_file_private(self):
        module.write_secret(self.secret, "from-file")
        self.assertEqual(module.configured_secret({"ADMIN_PASSWORD_FILE": str(self.secret)}, "ADMIN_PASSWORD"), "from-file")
        self.assertEqual(module.configured_secret({"ADMIN_PASSWORD": "from-env", "ADMIN_PASSWORD_FILE": "/missing"}, "ADMIN_PASSWORD"), "from-env")
        with self.assertRaises(module.StartupError):
            module.configured_secret({"ADMIN_PASSWORD": ""}, "ADMIN_PASSWORD")

    def test_no_overwrite_on_exclusive_create(self):
        module.write_secret(self.secret, "first")
        with self.assertRaises(FileExistsError):
            module.write_secret(self.secret, "second")
        self.assertEqual(module.read_secret(self.secret), "first")

    def test_external_volume_symlink_rejected(self):
        (self.root / "external").symlink_to("/etc/passwd")
        with self.assertRaisesRegex(module.StartupError, "external"):
            module.check_volume_links(self.root)

    def test_release_downgrade_fails_closed(self):
        manifest = self.root / "build.json"
        manifest.write_text(json.dumps({"version": "0.2.0"}))
        module.check_image_version(self.root, manifest)
        manifest.write_text(json.dumps({"version": "0.1.9"}))
        with self.assertRaisesRegex(module.StartupError, "downgrade"):
            module.check_image_version(self.root, manifest)
        self.assertEqual(module.read_secret(self.root / "image-version"), "0.2.0")

    def test_upgrade_marker_and_invalid_version(self):
        manifest = self.root / "build.json"
        for version in ("0.1.0", "0.2.0", "0.2.0"):
            manifest.write_text(json.dumps({"version": version}))
            module.check_image_version(self.root, manifest)
        self.assertEqual(module.read_secret(self.root / "image-version"), "0.2.0")
        manifest.write_text(json.dumps({"version": "untrusted"}))
        with self.assertRaises(module.StartupError):
            module.check_image_version(self.root, manifest)

    def test_sql_literal_quotes_without_execution(self):
        self.assertEqual(module.sql_literal("a'b\\c"), "'a''b\\c'")


class ProcessTests(unittest.TestCase):
    def new_supervisor(self):
        supervisor = object.__new__(module.Supervisor)
        supervisor.stopping = False
        supervisor.env = {}
        supervisor.pg = Mock()
        supervisor.proxy = Mock()
        supervisor.start_proxy = Mock()
        supervisor.start_pg = Mock()
        supervisor.stop_child = Mock()
        return supervisor

    def test_shutdown_proxy_before_pg_fast(self):
        supervisor = self.new_supervisor()
        supervisor.shutdown()
        calls = supervisor.stop_child.call_args_list
        self.assertEqual(calls[0].args, (supervisor.proxy, "Proxy", signal.SIGTERM, 20))
        self.assertEqual(calls[1].args, (supervisor.pg, "PostgreSQL", signal.SIGINT, 40))
        self.assertEqual(calls[1].kwargs, {"group": False})

    def test_restore_maintenance_never_starts_proxy(self):
        supervisor = self.new_supervisor()
        supervisor.env["TREER_SINGLE_MAINTENANCE"] = "restore"
        supervisor.pg.poll.side_effect = [None, 1]
        with patch.object(module.time, "sleep"):
            with self.assertRaisesRegex(module.StartupError, "restore maintenance"):
                supervisor.supervise()
        supervisor.start_proxy.assert_not_called()

    def test_pg_crash_stops_proxy_before_restart_and_caps_failures(self):
        supervisor = self.new_supervisor()
        supervisor.pg.poll.return_value = 1
        events = []
        supervisor.stop_child.side_effect = lambda *_: events.append("stop-proxy")
        supervisor.start_pg.side_effect = lambda: events.append("start-pg")
        supervisor.start_proxy.side_effect = lambda: events.append("start-proxy")
        with patch.object(module.time, "sleep"), patch.object(module.time, "monotonic", side_effect=range(1000)):
            with self.assertRaisesRegex(module.StartupError, "repeated"):
                supervisor.supervise()
        self.assertEqual(events[:4], ["start-proxy", "stop-proxy", "start-pg", "start-proxy"])
        self.assertEqual(supervisor.start_pg.call_count, 5)
        self.assertEqual(supervisor.stop_child.call_count, 6)

    def test_proxy_crash_does_not_restart_healthy_pg(self):
        supervisor = self.new_supervisor()
        supervisor.pg.poll.return_value = None
        supervisor.proxy.poll.return_value = 1
        with patch.object(module.time, "sleep"), patch.object(module.time, "monotonic", side_effect=range(1000)):
            with self.assertRaises(module.StartupError):
                supervisor.supervise()
        supervisor.start_pg.assert_not_called()
        supervisor.stop_child.assert_not_called()

    def test_stop_during_backoff_does_not_restart(self):
        supervisor = self.new_supervisor()
        supervisor.pg.poll.return_value = 1
        def stop(_):
            supervisor.stopping = True
        with patch.object(module.time, "sleep", side_effect=stop):
            supervisor.supervise()
        supervisor.start_pg.assert_not_called()

    def test_sigterm_reaches_real_child_process_group(self):
        child = subprocess.Popen([sys.executable, "-I", "-c", "import time; time.sleep(60)"],
                                 env={"PATH": os.defpath}, start_new_session=True)
        try:
            module.Supervisor.stop_child(child, "test child", signal.SIGTERM, 2)
            self.assertEqual(child.returncode, -signal.SIGTERM)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()

    def test_kill_only_after_grace_expires(self):
        child = Mock(pid=12345)
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired("test", 40), 0]
        with patch.object(module.os, "killpg") as kill:
            module.Supervisor.stop_child(child, "PostgreSQL", signal.SIGINT, 40)
        self.assertEqual([call.args[1] for call in kill.call_args_list], [signal.SIGINT, signal.SIGKILL])
        self.assertEqual(child.wait.call_args_list[0].kwargs, {"timeout": 40})


if __name__ == "__main__":
    unittest.main()
