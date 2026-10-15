"""Hermetic App deployment tests: disposable git, SQLite, HTTP and App processes."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "app-deploy"
loader = importlib.machinery.SourceFileLoader("app_deploy", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
deploy = importlib.util.module_from_spec(spec)
loader.exec_module(deploy)

APP = '''import http.server,json,os,sqlite3
from pathlib import Path
db=sqlite3.connect(os.environ['EXAMPLE_DB'])
db.execute('PRAGMA journal_mode=WAL')
db.execute('CREATE TABLE IF NOT EXISTS value(n INTEGER)')
if not db.execute('SELECT n FROM value').fetchall(): db.execute('INSERT INTO value VALUES(1)')
if VERSION != 1: db.execute('UPDATE value SET n=?',(VERSION,))
db.commit()
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  payload={'version':os.environ['APP_DEPLOY_VERSION'],'app_id':'app_fixture',
           'ready':READY,'database_ready':True}
  if self.path == '/route' and VERSION == 3: payload['app_id']='wrong-app'
  self.send_response(200);self.end_headers();self.wfile.write(json.dumps(payload).encode())
 def log_message(self,*args): pass
server=http.server.HTTPServer(('127.0.0.1',0),Handler)
Path(os.environ['PORT_FILE']).write_text(str(server.server_port))
server.serve_forever()
'''


@unittest.skipUnless(os.name == "posix", "POSIX deployment only")
class DeploymentTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.machine = Path(self.temporary.name).resolve()
        self.root = self.machine / "deployed/example"
        self.repo = self.machine / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "user.name", "Synthetic fixture")
        self.component = self.repo / "example"
        self.component.mkdir()
        self.base("init", "--machine-root", str(self.machine), "--command", sys.executable,
                  "--entry", "app.py", "--db-env", "EXAMPLE_DB")
        self.old = self.commit(1)
        self.stage(self.old)
        self.base("bootstrap", "--release", self.old)
        self.process = None
        self.addCleanup(self.stop_process)
        self.calls = []
        self.app = {"app_id": "app_fixture", "runtime_agent_id": "app_fixture",
                    "service_id": "svc_fixture", "server_id": "srv_fixture",
                    "hostname": "example", "desired_state": "running",
                    "command": str(Path(sys.executable).resolve()),
                    "args": [str(self.root / "entry.py"), "run", "--root", str(self.root)],
                    "cwd": str(self.root.relative_to(self.machine))}
        self.start_process()
        self.options = ["--app", "app_fixture", "--treer", sys.executable,
                        "--operation-id", "fixture", "--timeout", "1"]
        self.cli_patch = patch.object(deploy, "cli", side_effect=self.fake_cli)
        self.cli_patch.start()
        self.addCleanup(self.cli_patch.stop)

    def git(self, *args):
        return subprocess.check_output(["/usr/bin/git", "-C", str(self.repo), *args],
                                       stderr=subprocess.DEVNULL).decode().strip()

    def base(self, operation, *args):
        return deploy.main([operation, "--root", str(self.root), *args])

    def commit(self, number, ready=True):
        (self.component / "app.py").write_text(f"VERSION={number}\nREADY={ready!r}\n" + APP)
        self.git("add", ".")
        self.git("commit", "-qm", "fixture " + str(number))
        return self.git("rev-parse", "HEAD")

    def stage(self, commit, check=None):
        return self.base("stage", "--repo", str(self.repo), "--component", "example",
                         "--commit", commit, "--check-json",
                         json.dumps(check or [sys.executable, "-c", "import ast;ast.parse(open('app.py').read())"]))

    def stop_process(self):
        if self.process is not None:
            self.process.terminate()
            self.process.wait(timeout=5)
            self.process = None

    def start_process(self):
        self.port_file = self.root / "state/port"
        self.port_file.unlink(missing_ok=True)
        # Only fixture configuration, never real Treer credentials or UI settings.
        env = {"PATH": "/usr/bin:/bin", "PORT_FILE": str(self.port_file)}
        self.process = subprocess.Popen([sys.executable, str(self.root / "entry.py"),
                                         "run", "--root", str(self.root)], env=env,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        import time
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            self.assertIsNone(self.process.poll(), "fixture App must start")
            try:
                port = int(self.port_file.read_text())
                if 0 < port < 65536:
                    self.port = port
                    return
            except (FileNotFoundError, ValueError):
                pass  # Creation of the fixture port file precedes its write.
            time.sleep(.01)
        self.fail("fixture App must publish a complete port within five seconds")

    def fake_cli(self, options, *args):
        self.calls.append(args)
        if args == ("interface", "contract"):
            return {"machine": {"root": str(self.machine), "server_id": "srv_fixture"}}
        if args[:2] == ("app", "show"):
            return {"app": dict(self.app)}
        if args[:2] == ("app", "stop"):
            self.stop_process()
            self.app["desired_state"] = "stopped"
        elif args[:2] == ("app", "start"):
            self.start_process()
            self.app["desired_state"] = "running"
        else:
            self.fail("unexpected CLI operation")
        return {"app": dict(self.app)}

    def execute(self, commit, operation="deploy"):
        # The production route is stable; the fixture listener uses dynamic ports
        # after each restart. Rewrite only the port at the HTTP transport boundary.
        actual_health = deploy.health
        def health(receipt, version, timeout):
            current = dict(receipt)
            current["health_url"] = f"http://127.0.0.1:{self.port}/health"
            current["route_url"] = f"http://127.0.0.1:{self.port}/route"
            return actual_health(current, version, timeout)
        args = list(self.options)
        if operation == "deploy":
            args += ["--release", commit, "--health-url", "http://127.0.0.1:1/health",
                     "--route-url", "http://example.treer/route"]
        with patch.object(deploy, "health", side_effect=health):
            return self.base(operation, *args)

    def value(self):
        with sqlite3.connect(self.root / "state/app.sqlite3") as db:
            return db.execute("SELECT n FROM value").fetchone()[0]

    def test_success_identity_version_private_backup_and_idempotent_replay(self):
        new = self.commit(2)
        self.stage(new)
        before = dict(self.app)
        result = self.execute(new)
        self.assertEqual(result["status"], "completed")
        self.assertEqual(deploy.pointer(self.root), new)
        self.assertEqual(deploy.pointer(self.root, "previous"), self.old)
        self.assertEqual(self.app, before)
        self.assertEqual(self.value(), 2)
        backup = self.root / "backups/fixture.sqlite3"
        deploy.integrity(backup)
        with sqlite3.connect(backup) as db:
            self.assertEqual(db.execute("SELECT n FROM value").fetchone()[0], 1)
        self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
        self.assertEqual((self.root / "receipts/fixture.json").stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.root.stat().st_mode & 0o777, 0o700)
        mutations = [call for call in self.calls if call[1] in ("start", "stop")]
        self.execute(new)
        self.assertEqual([call for call in self.calls if call[1] in ("start", "stop")], mutations)

    def test_failed_health_restores_migrated_database_and_old_assets(self):
        new = self.commit(999, ready=False)
        self.stage(new)
        with self.assertRaisesRegex(deploy.Failure, "^rolled_back$"):
            self.execute(new)
        self.assertEqual(deploy.pointer(self.root), self.old)
        self.assertEqual(self.value(), 1)
        self.assertEqual(deploy.document(self.root / "receipts/fixture.json")["status"], "rolled_back")
        self.assertEqual(deploy.document(self.root / "receipts/fixture.json")["failed_phase"], "starting")
        self.assertEqual(deploy.document(self.root / "receipts/fixture.json")["failure"], "health_timeout")
        with self.assertRaisesRegex(deploy.Failure, "^rolled_back$"):
            self.execute(new)

    def test_actual_route_identity_mismatch_rolls_back_even_if_local_health_is_good(self):
        new = self.commit(3)
        self.stage(new)
        with self.assertRaisesRegex(deploy.Failure, "^rolled_back$"):
            self.execute(new)
        self.assertEqual(self.value(), 1)

    def test_crash_after_pointer_switch_recovers_with_durable_backup(self):
        new = self.commit(2)
        self.stage(new)
        class Crash(BaseException):
            pass
        original = deploy.point
        def crashing(root, name, value):
            original(root, name, value)
            if name == "current":
                raise Crash()
        with patch.object(deploy, "point", side_effect=crashing), self.assertRaises(Crash):
            self.execute(new)
        self.assertEqual(deploy.document(self.root / "receipts/fixture.json")["status"], "switching")
        with self.assertRaisesRegex(deploy.Failure, "operation_incomplete_run_recover"):
            self.execute(new)
        self.assertEqual(self.execute(new, "recover")["status"], "rolled_back")
        self.assertEqual(self.value(), 1)
        self.assertEqual(deploy.pointer(self.root), self.old)

    def test_failed_rollback_blocks_subsequent_deployments(self):
        new = self.commit(2)
        self.stage(new)
        original = self.fake_cli
        def failing(options, *args):
            if args[:2] == ("app", "start"):
                raise deploy.Failure("synthetic_spawn_failure")
            return original(options, *args)
        with patch.object(deploy, "cli", side_effect=failing), self.assertRaisesRegex(deploy.Failure, "rollback_failed"):
            self.execute(new)
        self.assertEqual(deploy.document(self.root / "receipts/fixture.json")["status"], "rollback_failed")
        self.options[self.options.index("fixture")] = "another"
        with self.assertRaisesRegex(deploy.Failure, "unfinished_deployment"):
            self.execute(new)

    def test_hash_mismatch_lock_and_foreign_app_fail_before_stop(self):
        new = self.commit(2)
        self.stage(new)
        with deploy.locked(self.root), self.assertRaisesRegex(deploy.Failure, "deployment_locked"):
            self.execute(new)
        self.app["server_id"] = "foreign"
        with self.assertRaisesRegex(deploy.Failure, "app_not_local"):
            self.execute(new)
        self.app["server_id"] = "srv_fixture"
        (self.root / "releases" / new / "app.py").write_text("tampered")
        with self.assertRaisesRegex(deploy.Failure, "release_integrity_failed"):
            self.execute(new)
        self.assertFalse(any(call[:2] == ("app", "stop") for call in self.calls))

    def test_stage_ignores_dirty_checkout_and_rejects_links_checks_and_short_sha(self):
        new = self.commit(2)
        (self.component / "app.py").write_text("dirty invalid code")
        with patch.dict(os.environ, {"TREER_WORKLOAD_CREDENTIAL": "synthetic-only",
                                    "AGENT_UI_SESSION_ID": "synthetic-only",
                                    "HTTPS_PROXY": "http://127.0.0.1:1"}):
            self.stage(new, [sys.executable, "-c", "import os;assert not any(k.startswith(('TREER_','AGENT_UI_')) or k=='HTTPS_PROXY' for k in os.environ)"])
        self.assertIn("VERSION=2", (self.root / "releases" / new / "app.py").read_text())
        (self.component / "escape").symlink_to("/etc/passwd")
        self.git("add", ".")
        self.git("commit", "-qm", "link fixture")
        bad = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(deploy.Failure, "archive_link_or_special"):
            self.stage(bad)
        self.assertFalse((self.root / "releases" / bad).exists())
        with self.assertRaisesRegex(deploy.Failure, "full_commit_required"):
            self.stage(new[:7])

    def test_symlink_root_and_pointer_escape_are_rejected(self):
        link = self.machine / "alias"
        link.symlink_to(self.root)
        with self.assertRaisesRegex(deploy.Failure, "directory_symlink_or_special"):
            deploy.layout(link)
        (self.root / "current").unlink()
        (self.root / "current").symlink_to(self.repo)
        with self.assertRaisesRegex(deploy.Failure, "invalid_version_pointer"):
            deploy.pointer(self.root)

    def test_sqlite_backup_captures_live_wal_and_detects_backup_tampering(self):
        source = self.root / "state/app.sqlite3"
        with sqlite3.connect(source) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("UPDATE value SET n=42")
            db.commit()
            saved = deploy.backup(self.root, "wal")
            with sqlite3.connect(self.root / "backups" / saved["name"]) as copy:
                self.assertEqual(copy.execute("SELECT n FROM value").fetchone()[0], 42)
        self.stop_process()
        with (self.root / "backups" / saved["name"]).open("ab") as file:
            file.write(b"changed")
        with self.assertRaisesRegex(deploy.Failure, "backup_integrity_failed"):
            deploy.restore(self.root, saved)

    def test_running_process_uses_physical_release_after_current_changes(self):
        new = self.commit(2)
        self.stage(new)
        deploy.point(self.root, "current", new)
        receipt = {"app_id": "app_fixture", "health_url": f"http://127.0.0.1:{self.port}/health",
                   "route_url": f"http://127.0.0.1:{self.port}/route"}
        deploy.health(receipt, self.old, 1)

    def test_crash_at_restored_main_file_publication_cannot_replay_candidate_wal(self):
        self.stop_process()
        saved = deploy.backup(self.root, "before-migration")
        target = self.root / "state/app.sqlite3"
        # A real writer exits without closing SQLite, leaving committed candidate
        # pages in its WAL. No user App/database is involved.
        deploy.run([sys.executable, "-c", """
import os,sqlite3,sys
db=sqlite3.connect(sys.argv[1])
db.execute('PRAGMA journal_mode=WAL')
db.execute('PRAGMA wal_autocheckpoint=0')
db.execute('CREATE TABLE candidate_only(n INTEGER)')
db.executemany('INSERT INTO candidate_only VALUES(?)', [(i,) for i in range(250)])
db.execute('UPDATE value SET n=999')
db.commit()
os._exit(0)
""", str(target)])
        self.assertGreater(Path(str(target) + "-wal").stat().st_size, 0)
        class Crash(BaseException):
            pass
        actual_replace = os.replace
        def publish_then_crash(source, destination):
            actual_replace(source, destination)
            if destination == target:
                raise Crash()
        with patch.object(deploy.os, "replace", side_effect=publish_then_crash), self.assertRaises(Crash):
            deploy.restore(self.root, saved)
        # This read happens BEFORE recover can retry restore. The old code let
        # the candidate WAL recreate its table/value here while integrity was ok.
        with sqlite3.connect(target) as database:
            self.assertEqual(database.execute("SELECT n FROM value").fetchall(), [(1,)])
            self.assertEqual(database.execute("SELECT name FROM sqlite_master WHERE name='candidate_only'").fetchall(), [])
            self.assertEqual(database.execute("PRAGMA integrity_check").fetchall(), [("ok",)])
        deploy.restore(self.root, saved)
        self.assertEqual(self.value(), 1)

    def test_backup_progress_exception_propagates_and_cleans_unpublished_backup(self):
        with patch.object(deploy.time, "monotonic", side_effect=[0, 121]):
            with self.assertRaisesRegex(deploy.Failure, "database_backup_timeout"):
                deploy.backup(self.root, "deadline")
        self.assertFalse((self.root / "backups/deadline.sqlite3").exists())
        self.assertFalse(any(path.name.startswith(".backup-") for path in (self.root / "backups").iterdir()))
        self.assertEqual(self.value(), 1)

    def test_failed_check_leaves_no_release_or_pointer_change(self):
        new = self.commit(2)
        with self.assertRaisesRegex(deploy.Failure, "command_failed"):
            self.stage(new, [sys.executable, "-c", "raise SystemExit(7)"])
        self.assertFalse((self.root / "releases" / new).exists())
        self.assertEqual(deploy.pointer(self.root), self.old)
        self.assertFalse(any(path.name.startswith(".stage-") for path in (self.root / "releases").iterdir()))

    def test_database_symlink_is_not_backed_up_or_restored(self):
        self.stop_process()
        database = self.root / "state/app.sqlite3"
        database.unlink()
        database.symlink_to(self.component / "app.py")
        with self.assertRaisesRegex(deploy.Failure, "regular_file_required"):
            deploy.backup(self.root, "unsafe")
        with self.assertRaisesRegex(deploy.Failure, "regular_file_required"):
            deploy.restore(self.root, None)

    def test_subprocess_output_and_time_are_bounded(self):
        with self.assertRaisesRegex(deploy.Failure, "command_output_too_large"):
            deploy.run([sys.executable, "-c", "print('x' * (3*1024*1024))"])
        with self.assertRaisesRegex(deploy.Failure, "command_failed_or_timed_out"):
            deploy.run([sys.executable, "-c", "import time;time.sleep(5)"], timeout=.05)

    def test_root_binding_survives_success_and_prevents_new_identity_reuse(self):
        new = self.commit(2)
        self.stage(new)
        self.execute(new)
        self.assertEqual(deploy.document(self.root / "binding.json")["app_id"], "app_fixture")
        third = self.commit(4)
        self.stage(third)
        self.app["app_id"] = "another-app"
        self.options[self.options.index("fixture")] = "another"
        with self.assertRaisesRegex(deploy.Failure, "deployment_bound_to_another_app"):
            self.execute(third)

    def test_unpublished_receipt_temporary_file_does_not_block_clean_retry(self):
        temporary = self.root / "receipts" / (".tmp-" + "a" * 32)
        temporary.write_text('{"schema":')
        new = self.commit(2)
        self.stage(new)
        self.assertEqual(self.execute(new)["status"], "completed")

    def test_health_rejects_credentials_redirects_and_plain_public_http(self):
        for url, local in [("http://user:secret@127.0.0.1/", True),
                           ("http://example.com/", False),
                           ("https://example.com/?token=secret", False),
                           ("http://localhost/", True)]:
            with self.assertRaises(deploy.Failure):
                deploy.check_url(url, local)
        with self.assertRaisesRegex(deploy.Failure, "health_redirect_refused"):
            deploy.NoRedirect().redirect_request(None)


if __name__ == "__main__":
    unittest.main()
