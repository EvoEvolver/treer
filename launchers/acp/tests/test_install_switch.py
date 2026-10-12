import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("native_session", ROOT / "scripts/native_session.py")
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


MOCK_TREER = r'''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
state=Path(os.environ['MOCK_STATE'])
args=sys.argv[1:]
# The real thin client streams stdin to the Controller.
if os.getenv('MOCK_DRAIN_STDIN'): sys.stdin.read()
with (state/'calls.jsonl').open('a') as f: f.write(json.dumps(args)+'\n')
if args==['agent','startup','set'] or args[:3]==['agent','startup','set']:
 (state/'startup.json').write_text(json.dumps(args))
 print('{}')
elif args==['agent','restart','self']:
 import sqlite3
 (state/'generation').write_text('2')
 file=Path(os.environ['MOCK_CWD'])/'.treer/agents/ag_test/journal.sqlite'
 file.parent.mkdir(parents=True,exist_ok=True)
 with sqlite3.connect(file) as c:
  c.execute('CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)')
  c.execute('INSERT OR REPLACE INTO kv VALUES (?,?)',('session_key','codex::'+os.environ['MOCK_SESSION']))
 print('{}')
elif args==['agent','show','self']:
 print(json.dumps({'agent_id':'ag_test','run_generation':int((state/'generation').read_text())}))
elif args==['interface','show']:
 print(json.dumps({'port':int(os.environ['MOCK_AIS_PORT']),'instance_id':'replacement'}))
elif args==['whoami']:
 print(json.dumps({'machine':{'root':str(state/'machine root'),'server_id':'srv_test'},'agent':{'agent_id':'ag_test','run_generation':1}}))
elif args[:3]==['agent','admin','profile']:
 action,name=args[3:5]
 file=state/(name+'.json')
 if action=='show':
  if not file.exists(): sys.exit(1)
  p=json.loads(file.read_text())
  if os.getenv('MOCK_BAD_SHOW'): p['args']=[]
  print(json.dumps(p))
 elif action=='create':
  cwd=args[args.index('--cwd')+1]; command=args[args.index('--cwd')+2]
  tail=args[args.index('--')+1:] if '--' in args else []
  file.write_text(json.dumps({'command':command,'cwd':cwd,'args':tail,'profile_id':'prof_test'}))
 elif action=='update':
  p=json.loads(file.read_text());p['command']=args[args.index('--command')+1];p['cwd']=args[args.index('--cwd')+1]
  p['args']=[args[i+1] for i,a in enumerate(args) if a=='--arg']
  file.write_text(json.dumps(p))
 else: sys.exit(2)
else: sys.exit(3)
'''

MOCK_CARGO = r'''#!/usr/bin/env python3
import os,sys
from pathlib import Path
args=sys.argv[1:];target=Path(args[args.index('--target-dir')+1])/'release/treer-acp'
target.parent.mkdir(parents=True,exist_ok=True)
target.write_text('#!/bin/sh\nprintf "%s\\n" "'+os.environ.get('MOCK_BUILD_TAG','version-one')+'"\n')
target.chmod(0o755)
'''


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="acp-install-test-")
        self.addCleanup(self.temporary.cleanup)
        self.state = Path(self.temporary.name).resolve()
        self.machine = self.state / "machine root"
        self.machine.mkdir()
        self.source = self.state / "source"
        shutil.copytree(ROOT, self.source / "launchers/acp", ignore=shutil.ignore_patterns(
            ".build", "target", "node_modules", "__pycache__", ".git"))
        self.bin = self.state / "bin"
        self.bin.mkdir()
        for name, text in [("treer", MOCK_TREER), ("cargo", MOCK_CARGO),
                           ("codex", "#!/bin/sh\nexit 0\n"), ("codex-acp", "#!/bin/sh\nexit 0\n")]:
            file = self.bin / name
            file.write_text(text)
            file.chmod(0o755)
        self.env = {**os.environ, "MOCK_STATE": str(self.state), "PATH": str(self.bin) + os.pathsep + os.environ["PATH"]}
        self.git("init", "-q")
        self.git("config", "user.name", "Installer Test")
        self.git("config", "user.email", "test@example.invalid")
        self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def install(self, check=True):
        return subprocess.run(["sh", str(ROOT / "install.sh"), "--source", str(self.source), "--agent", "codex"],
                              env=self.env, text=True, capture_output=True, check=check)

    def test_install_repeat_and_update_preserve_old_runtime(self):
        first = self.git("rev-parse", "HEAD")
        self.install()
        base = self.machine / ".treer/launchers/acp"
        old = base / first
        profile = json.loads((self.state / "Codex ACP.json").read_text())
        self.assertEqual(profile["command"], str(base / "current/launchers/acp/scripts/treer-agent.sh"))
        self.assertEqual(profile["cwd"], ".")
        self.install()
        self.assertEqual((base / "current").resolve(), old)
        self.env["MOCK_BUILD_TAG"] = "version-two"
        (self.source / "revision").write_text("two")
        second = self.commit()
        self.install()
        self.assertEqual((base / "current").resolve(), base / second)
        args = profile["args"]
        output = subprocess.check_output([str(old / "launchers/acp/scripts/treer-agent.sh"), *args], env=self.env, text=True)
        self.assertEqual(output.strip(), "version-one")
        output = subprocess.check_output([profile["command"], *args], env=self.env, text=True)
        self.assertEqual(output.strip(), "version-two")
        calls = [json.loads(line) for line in (self.state / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("launch" in call for call in calls))
        self.assertGreaterEqual(sum(call[:4] == ["agent", "admin", "profile", "show"] for call in calls), 6)

    def test_piped_install_survives_stdin_streaming_treer(self):
        # `curl ... | sh` feeds the script on stdin; the treer client must not eat it.
        result = subprocess.run(["sh", "-s", "--", "--source", str(self.source), "--agent", "codex"],
                                input=(ROOT / "install.sh").read_text(), env={**self.env, "MOCK_DRAIN_STDIN": "1"},
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ACP launcher installed", result.stdout)

    def test_sparse_checkout_covers_protocol_compile_inputs(self):
        import re
        repo = ROOT.parents[1]
        includes = set()
        for source in (repo / "crates/treer-protocol/src").rglob("*.rs"):
            for path in re.findall(r'include_(?:str|bytes)!\("([^"]+)"\)', source.read_text()):
                includes.add((source.parent / path).resolve().relative_to(repo).as_posix())
        self.assertTrue(includes)
        install = (ROOT / "install.sh").read_text()
        readme = (ROOT / "README.md").read_text()
        install_set = re.search(r'sparse-checkout set ([^\n]+)', install).group(1).split()
        readme_set = re.search(r'sparse-checkout set \\\n\s+([^\n]+)', readme).group(1).split()
        for entries in (install_set, readme_set):
            for path in includes:
                self.assertTrue(any(path == entry or path.startswith(entry + "/") for entry in entries),
                                f"{path} is not in sparse checkout {entries}")

    def test_failed_verification_rolls_back_current(self):
        self.install()
        base = self.machine / ".treer/launchers/acp"
        previous = (base / "current").resolve()
        (self.source / "revision").write_text("candidate")
        self.commit()
        self.env["MOCK_BAD_SHOW"] = "1"
        result = self.install(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((base / "current").resolve(), previous)
        self.assertIn("current was restored", result.stderr)

    def test_failed_build_does_not_activate(self):
        (self.bin / "cargo").write_text("#!/bin/sh\nexit 42\n")
        result = self.install(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.machine / ".treer/launchers/acp/current").exists())
        self.assertFalse((self.machine / ".treer/launchers/acp/.install-lock").exists())

    def test_switch_dry_run_preserves_startup_and_uses_pinned_directory(self):
        self.install()
        cwd = self.machine / "project"
        cwd.mkdir()
        sessions = self.state / "home/.codex/sessions/2026"
        sessions.mkdir(parents=True)
        (sessions / "one.jsonl").write_text(json.dumps({"type": "session_meta", "payload": {"id": "native-one", "cwd": str(cwd)}}) + "\n")
        # Only the child under test sees this disposable user's session store.
        env = {**self.env, "HOME": str(self.state / "home")}
        result = subprocess.run([str(ROOT / "scripts/switch.sh"), "--agent", "codex", "--dry-run"],
                                cwd=cwd, env=env, capture_output=True, text=True, check=True)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["session_id"], "native-one")
        self.assertNotIn("current", plan["command"][0])
        self.assertEqual(plan["command"][-2:], ["--session-id", "native-one"])
        calls = (self.state / "calls.jsonl").read_text()
        self.assertNotIn('"restart"', calls)
        self.assertNotIn('"startup"', calls)

    def exercise_detached_switch(self, actual_session):
        self.install()
        cwd = self.machine / "project"
        cwd.mkdir()
        sessions = self.state / "home/.codex/sessions/2026"
        sessions.mkdir(parents=True)
        (sessions / "one.jsonl").write_text(json.dumps({"type": "session_meta", "payload": {
            "id": "native-one", "cwd": str(cwd)}}) + "\n" + json.dumps({"type": "response_item", "payload": {
            "role": "user", "content": [{"text": "Finish the launcher task"}]}}) + "\n")
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                data = json.dumps({"instance_id": "replacement"}).encode()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                received.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                self.send_response(202)
                self.end_headers()

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        env = {**self.env, "HOME": str(self.state / "home"), "MOCK_CWD": str(cwd),
               "MOCK_SESSION": actual_session, "MOCK_AIS_PORT": str(server.server_port),
               "TREER_AGENT_ID": "ag_test", "TREER_WORKLOAD_CREDENTIAL": "test-only",
               "TREER_AGENT_SERVER_URL": "http://127.0.0.1:1"}
        subprocess.run([str(ROOT / "scripts/switch.sh"), "--agent", "codex"], cwd=cwd,
                       env=env, capture_output=True, text=True, check=True, timeout=5)
        # The wrapper has exited; its detached child must finish independently.
        folder = self.machine / ".treer/launchers/acp/switches/ag_test"
        deadline = time.monotonic() + 5
        result_files = []
        while time.monotonic() < deadline:
            result_files = list(folder.glob("*/result.json"))
            if result_files:
                break
            time.sleep(0.02)
        self.assertEqual(len(result_files), 1)
        result = json.loads(result_files[0].read_text())
        self.assertEqual(result["session_loaded"], actual_session == "native-one")
        startup = json.loads((self.state / "startup.json").read_text())
        self.assertEqual(startup[-2:], ["--session-id", actual_session])
        self.assertEqual(len(received), int(actual_session != "native-one"))
        if received:
            self.assertIn("Finish the launcher task", received[0]["text"])
            self.assertTrue(received[0]["operation_id"].startswith("acp-switch-"))

    def test_detached_restart_load_success_does_not_reprompt(self):
        self.exercise_detached_switch("native-one")

    def test_detached_restart_load_failure_sends_summary_and_saves_new_session(self):
        self.exercise_detached_switch("replacement-session")


class SessionTest(unittest.TestCase):
    def test_discovery_filters_cwd_then_uses_latest_modification(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            cwd = home / "work"
            cwd.mkdir()
            sessions = home / ".codex/sessions/2026"
            sessions.mkdir(parents=True)
            for name, folder, stamp in [("one", cwd, 1), ("two", cwd, 2), ("other", home, 3)]:
                file = sessions / (name + ".jsonl")
                file.write_text(json.dumps({"type": "session_meta", "payload": {"id": name, "cwd": str(folder)}}) + "\n")
                os.utime(file, (stamp, stamp))
            with patch.object(native.Path, "home", return_value=home):
                self.assertEqual(native.discover("codex", cwd)[0], "two")
                self.assertEqual(native.discover("codex", cwd, "one")[0], "one")

    def test_claude_discovery_and_bounded_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            cwd = home / "work"
            cwd.mkdir()
            folder = home / ".claude/projects" / native.re.sub(r"[^a-zA-Z0-9]", "-", str(cwd))
            folder.mkdir(parents=True)
            file = folder / "native.jsonl"
            file.write_text(json.dumps({"sessionId": "native", "cwd": str(cwd), "message": {
                "role": "user", "content": "Remember my task token=private-secret"}}) + "\n")
            with patch.object(native.Path, "home", return_value=home):
                self.assertEqual(native.discover("claude", cwd)[0], "native")
            text = native.summary("claude", "native", file, cwd)
            self.assertNotIn("private-secret", text)
            self.assertIn("Remember my task", text)
            self.assertLessEqual(len(text), 8000)


if __name__ == "__main__":
    unittest.main()
