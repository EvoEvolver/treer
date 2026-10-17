import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / "build-single-image.sh"


class BuildScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.repo = self.root / "checkout"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copyfile(SOURCE, self.repo / "scripts/build-single-image.sh")
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.14"\n')
        (self.repo / ".single-container-input").mkdir()
        (self.repo / ".single-container-input/.gitignore").write_text('*\n!.gitignore\n')
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "docker.jsonl"
        self.env = {"PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "HOME": str(self.root / "home"), "TMPDIR": str(self.root),
                    "BUILD_TEST_LOG": str(self.log), "GIT_CONFIG_NOSYSTEM": "1",
                    "GIT_AUTHOR_NAME": "Fixture", "GIT_AUTHOR_EMAIL": "fixture@treer.invalid",
                    "GIT_COMMITTER_NAME": "Fixture", "GIT_COMMITTER_EMAIL": "fixture@treer.invalid"}
        Path(self.env["HOME"]).mkdir()
        self.git("init", "-q")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.sha = self.git("rev-parse", "HEAD").strip()
        self.env["BUILD_TEST_SHA"] = self.sha
        # The fake records argv and checks the archived context; it never
        # invokes Docker, a Treer binary, or any real user's machine.
        (self.bin / "docker").write_text('''#!''' + shutil.which("python3") + '''
import json,os,sys
from pathlib import Path
a=sys.argv[1:]
with open(''' + repr(str(self.log)) + ''','a') as f:f.write(json.dumps(a)+'\\n')
if a[:2]==['context','inspect']:print('unix:///fixture.sock')
elif a==['info','--format','{{.Architecture}}']:print('aarch64')
elif a[:2]==['image','inspect']:print(''' + repr(self.sha) + ''')
elif a[:2]==['buildx','build']:
 context=Path(a[-1])
 assert not (context/'.env').exists() and not (context/'.git').exists()
''')
        (self.bin / "docker").chmod(0o755)

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], env=self.env, text=True)

    def run_script(self, **env):
        return subprocess.run(["/bin/sh", str(self.repo / "scripts/build-single-image.sh")],
                              cwd=self.root, env={**self.env, **env}, capture_output=True, text=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_builds_archived_commit_before_loading_local_image_without_push(self):
        (self.repo / ".env").write_text("FIXTURE_ONLY=excluded\n")
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stderr)
        calls = self.calls()
        builds = [c for c in calls if c[:2] == ["buildx", "build"]]
        self.assertEqual(len(builds), 1)
        self.assertIn("local", builds[0])
        self.assertIn("--load", builds[0])
        for c in builds:
            self.assertIn("TREER_BUILD_COMMIT=" + self.sha, c)
            self.assertIn("TREER_VERSION=0.1.14", c)
            self.assertNotIn("--push", c)
        self.assertIn("treer-single:" + self.sha[:12], r.stdout)
        self.assertIn("console.treer.localhost:8080", r.stdout)
        self.assertIn("proxy.treer.localhost:8080", r.stdout)
        self.assertIn("treer-data:/data", r.stdout)
        self.assertFalse(any(c[0] in ("run", "push", "stop", "restart") for c in calls))
        self.assertFalse(list(self.root.glob("treer-single-build.*")))

    def test_missing_docker_has_actionable_error(self):
        empty = self.root / "empty"
        empty.mkdir()
        r = self.run_script(PATH=str(empty))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Docker is missing", r.stderr)

    def test_dirty_checkout_fails_without_building(self):
        (self.repo / "Cargo.toml").write_text("uncommitted change\n")
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("commit or stash", r.stderr)
        self.assertFalse(any(c[:2] == ["buildx", "build"] for c in self.calls()))

    def test_docker_receives_no_agent_or_cloud_credentials(self):
        fake = self.bin / "docker"
        fake.write_text(fake.read_text().replace("a=sys.argv[1:]", "a=sys.argv[1:]\nassert 'TREER_WORKLOAD_TOKEN' not in os.environ and 'AWS_SECRET_ACCESS_KEY' not in os.environ"))
        r = self.run_script(TREER_WORKLOAD_TOKEN="fixture", AWS_SECRET_ACCESS_KEY="fixture")
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_wrong_image_revision_is_rejected(self):
        fake = self.bin / "docker"
        fake.write_text(fake.read_text().replace(self.sha, "b" * 40))
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("revision does not match", r.stderr)


if __name__ == "__main__":
    unittest.main()
