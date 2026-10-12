import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("canary_release", "canary-release.py")
checkpoint = load("checkpoint", "release-checkpoint.py")


class ReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.remote = Path(self.temp.name) / "remote.git"
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "test@example.invalid")
        (self.root / ".gitignore").write_text("/.treer/\n")
        (self.root / "source.txt").write_text("committed candidate\n")
        self.git("add", ".")
        self.git("commit", "-m", "candidate")
        self.commit = self.git("rev-parse", "HEAD")
        subprocess.run(["git", "init", "--bare", str(self.remote)], check=True, capture_output=True)
        self.git("remote", "add", "origin", str(self.remote))
        self.git("push", "origin", "main")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True,
                                       stderr=subprocess.DEVNULL).strip()

    def args(self, **kwargs):
        return argparse.Namespace(revision="HEAD", push=False, resume=False, dry_run=False,
                                  notify=kwargs.get("notify"), agent=kwargs.get("agent"))

    def test_dry_run_does_not_write_fetch_push_or_check_identity(self):
        args = self.args(notify="leader", agent="deployer")
        args.dry_run = True
        with patch.object(release, "checked_identity", side_effect=AssertionError("network")):
            release.deploy(self.root, args)
        self.assertFalse((self.root / ".treer").exists())

    def test_wrong_caller_fails_before_push_or_creating_a_worktree(self):
        original_run = release.run

        def wrong_identity(*args, **kwargs):
            if args[:2] == ("treer", "whoami"):
                return json.dumps({"agent": {"agent_id": "dev1-id", "name": "dev1"}})
            return original_run(*args, **kwargs)

        with patch.object(release.shutil, "which", return_value="/bin/true"), \
             patch.object(release, "run", side_effect=wrong_identity):
            with self.assertRaisesRegex(RuntimeError, "caller is dev1"):
                release.deploy(self.root, self.args(notify="leader", agent="deployer"))
        self.assertFalse((self.root / ".treer").exists())

    def test_push_refuses_a_remote_commit_missing_from_the_candidate(self):
        self.git("checkout", "-b", "remote-change")
        (self.root / "source.txt").write_text("remote update\n")
        self.git("commit", "-am", "remote update")
        remote_commit = self.git("rev-parse", "HEAD")
        self.git("push", "origin", "HEAD:main")
        self.git("checkout", "main")
        args = self.args()
        args.push = True
        with patch.object(release.shutil, "which", return_value="/bin/true"):
            with self.assertRaises(subprocess.CalledProcessError):
                release.deploy(self.root, args)
        self.assertEqual(self.git("rev-parse", "origin/main"), remote_commit)
        self.assertFalse((self.root / ".treer/canary-checkouts").exists())

    def test_release_uses_committed_source_preserves_dirty_source_and_refuses_dirty_retry(self):
        (self.root / "source.txt").write_text("concurrent uncommitted work\n")
        original_run = release.run
        commands = []

        def fake_run(*args, cwd, **kwargs):
            commands.append(args)
            if args[0] == "git":
                return original_run(*args, cwd=cwd, **kwargs)
            if args[:2] == ("sh", "scripts/release-canary.sh"):
                self.assertEqual((cwd / "source.txt").read_text(), "committed candidate\n")
                path = cwd / ".treer/releases" / self.commit / "manifest.json"
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps({"commit": self.commit, "status": "canary_passed",
                                            "canary": {"proxy_deployment_id": "deployment-test"}}))

        with patch.object(release.shutil, "which", return_value="/bin/true"), \
             patch.object(release, "run", side_effect=fake_run):
            release.deploy(self.root, self.args())
            checkout = release.candidate(self.root, "HEAD")[1]
            (checkout / "source.txt").write_text("modified candidate\n")
            with self.assertRaisesRegex(RuntimeError, "modified"):
                release.deploy(self.root, self.args())
        self.assertEqual((self.root / "source.txt").read_text(), "concurrent uncommitted work\n")
        self.assertFalse((self.root / ".treer/canary-release.lock").exists())
        self.assertFalse(any("restart" in command for command in commands))

    def test_checkpoint_rejects_another_commit_or_changed_frontend(self):
        artifact = self.root / "index.html"
        artifact.write_text("passed frontend")
        path = self.root / "receipt.json"
        path.write_text(json.dumps(checkpoint.receipt(self.commit, artifact)))
        self.assertTrue(checkpoint.verified(path, self.commit, artifact))
        self.assertFalse(checkpoint.verified(path, "different-commit", artifact))
        artifact.write_text("changed frontend")
        self.assertFalse(checkpoint.verified(path, self.commit, artifact))


class CookieTest(unittest.TestCase):
    def test_cookie_headers_and_failed_login_without_a_disk_cookie_jar(self):
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                self.send_response(401 if self.path == "/denied" else 200)
                if self.path == "/login":
                    self.send_header("Set-Cookie", "session=fake; Path=/; HttpOnly")
                    self.send_header("set-cookie", "other=test; Secure")
                self.end_headers()
                self.wfile.write(b'{"ok":true}')

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as directory:
            for route, expected in (("login", "session=fake; other=test"), ("denied", None), ("empty", None)):
                result = subprocess.run(["sh", "-c", '. "$1"; canary_login_cookies "$2"',
                                         "cookie-test", str(SCRIPTS / "canary-auth.sh"),
                                         f"http://127.0.0.1:{server.server_port}/{route}"],
                                        input='{"password":"fake"}', text=True,
                                        capture_output=True, cwd=directory,
                                        env={**os.environ, "NO_PROXY": "127.0.0.1"})
                if expected:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.strip(), expected)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
