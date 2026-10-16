"""Closed fake-machine tests: no enrolled Controller, Host or Agent is touched."""
import base64
import contextlib
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "update-machine.py"
spec = importlib.util.spec_from_file_location("update_machine", SCRIPT)
update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update)


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home"
        self.bindir = self.home / "bin"
        self.bindir.mkdir(parents=True)
        self.configdir = self.home / ".config/treer/agent-servers"
        self.configdir.mkdir(parents=True)
        self.job = self.home / "job"
        self.job.mkdir()
        self.artifacts = self.root / "dist"
        self.artifacts.mkdir()
        self.state = self.root / "state.json"
        self.initial = {"epoch": "old-epoch", "capable": False,
            "agents": [{"agent_id": "ag_fixture", "pid": 444, "run_generation": 3}], "events": []}
        self.state.write_text(json.dumps(self.initial))
        self.old = "aaaaaaa"
        self.new = "bbbbbbb"
        for name in update.BINARIES:
            self.binary(self.bindir / name, self.old)
            self.binary(self.artifacts / name, self.new)
        checksums = "".join(f"{update.digest(self.artifacts/name)}  {name}\n" for name in update.BINARIES)
        (self.artifacts / "SHA256SUMS").write_text(checksums)
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path.startswith("/artifacts/"):
                    name = self.path.rsplit("/", 1)[1]
                    if fixture.redirect:
                        self.send_response(302)
                        self.send_header("Location", "/redirected")
                        self.end_headers()
                        return
                    data = (fixture.artifacts / name).read_bytes()
                else:
                    state = json.loads(fixture.state.read_text())
                    if self.path == "/api/health":
                        value = {"service": "treer-agent-server", "workspace_id": "ws_fixture",
                            "server_id": "srv_fixture", "proxy_connected": not fixture.disconnect,
                            "host_build": {"git_commit": fixture.new if "bbbbbbb:restart" in state["events"] else fixture.old},
                            "controller_epoch": state["epoch"], "controller_build": {"git_commit": fixture.new if state["epoch"].startswith(fixture.new) else fixture.old}, "cli_capabilities":
                            ["agent.path-targets.v1"] if state["capable"] else []}
                        if state["capable"]:
                            fixture.assertIn(fixture.old, (fixture.bindir / "treer").read_text()) if not fixture.cli_done else None
                    else:
                        fixture.assertEqual(self.headers["x-treer-operator-credential"], "test-only-secret")
                        value = {"agents": state["agents"]}
                    data = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *_):
                pass

        self.redirect = False
        self.disconnect = False
        self.cli_done = True  # order is checked by the fake Controller below.
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.configpath = self.configdir / "fixture-controller.json"
        self.configpath.write_text(json.dumps({"workspace": "ws_fixture", "server_id": "srv_fixture",
            "listen": f"127.0.0.1:{self.server.server_port}", "install_hostname": socket.gethostname(),
            "operator_credential": "test-only-secret"}))
        self.config = {"home": str(self.home), "bin_dir": str(self.bindir),
            "config_dir": str(self.configdir), "workspace": "ws_fixture", "host": False,
            "proxy": f"http://127.0.0.1:{self.server.server_port}", "platform": "linux-x86_64",
            "artifacts": str(self.artifacts), "timeout": 2}

    def binary(self, path, commit, failure=None):
        source = f'''#!{sys.executable}
import json,os,sys
from pathlib import Path
COMMIT={commit!r}
state_path=Path({str(self.state)!r})
if '--version' in sys.argv:
    if {failure!r} == 'installed-cli' and Path(__file__).parent.name == 'bin': sys.exit(4)
    print('treer 0.1.13 ('+COMMIT+')')
else:
    assert sys.argv[1:4] == ['service','--workspace','ws_fixture'],sys.argv
    assert not any(k.startswith(('AGENT_UI_', 'CODEX_UI_', 'TREER_')) for k in os.environ)
    state=json.loads(state_path.read_text())
    state['events'].append(COMMIT+':'+sys.argv[-1])
    # The old CLI must remain installed until Controller readiness.
    if COMMIT == {self.new!r}: assert {self.old!r} in (Path(__file__).parent/'treer').read_text()
    state['epoch']=COMMIT+'-'+str(len(state['events']))
    state['capable']=COMMIT == {self.new!r}
    if {failure!r} == 'pid' and COMMIT == {self.new!r}: state['agents'][0]['pid']+=1
    if {failure!r} == 'capability': state['capable']=False
    if sys.argv[-1]=='restart':
        state['agents'][0]['pid']+=1
        state['agents'][0]['run_generation']+=1
    state_path.write_text(json.dumps(state))
    if {failure!r} == 'activation' and COMMIT == {self.new!r}: sys.exit(4)
'''
        path.write_text(source)
        path.chmod(0o755)

    def refresh_sums(self):
        (self.artifacts / "SHA256SUMS").write_text("".join(
            f"{update.digest(self.artifacts/name)}  {name}\n" for name in update.BINARIES))

    def invoke(self):
        update.write_json(self.job / "job.json", self.config)
        with patch.dict(os.environ, {"TREER_WORKLOAD_CREDENTIAL": "must-not-inherit",
                                     "AGENT_UI_SESSION_ID": "old", "HTTP_PROXY": "http://127.0.0.1:1"}):
            code = update.worker(self.job)
        return code, json.loads((self.job / "result.json").read_text())

    def test_hot_order_checksum_backup_pid_and_clean_environment(self):
        original_host = (self.bindir / "treer-agent-host").read_bytes()
        code, result = self.invoke()
        self.assertEqual(code, 0, result)
        self.assertEqual(result["status"], "complete")
        self.assertEqual(result["commit"], self.new)
        self.assertEqual((self.bindir / "treer-agent-host").read_bytes(), original_host)
        self.assertEqual(json.loads(self.state.read_text())["agents"], self.initial["agents"])
        self.assertEqual(json.loads(self.state.read_text())["events"], ["bbbbbbb:restart-controller"])
        for name in update.BINARIES[:2]:
            self.assertIn(self.old, (Path(result["backup"]) / name).read_text())
        for file in self.job.rglob("*.json"):
            self.assertNotIn("test-only-secret", file.read_text())
            self.assertNotIn("must-not-inherit", file.read_text())

    def test_bad_hash_leaves_every_binary_and_service_untouched(self):
        originals = {name: update.digest(self.bindir / name) for name in update.BINARIES}
        (self.artifacts / "treer").write_text("tampered")
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("SHA256 mismatch", result["error"])
        self.assertEqual(originals, {name: update.digest(self.bindir / name) for name in update.BINARIES})
        self.assertEqual(json.loads(self.state.read_text())["events"], [])

    def test_release_receipt_pins_sums_and_binary_hashes_before_activation(self):
        self.config["expected_sums"] = "0" * 64
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("canary_passed release receipt", result["error"])
        self.assertEqual(json.loads(self.state.read_text())["events"], [])
        (self.job / "result.json").unlink()
        self.config["expected_sums"] = hashlib.sha256((self.artifacts / "SHA256SUMS").read_bytes()).hexdigest()
        self.config["expected_files"] = {name: "0" * 64 for name in update.BINARIES}
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("artifact checksums differ", result["error"])
        self.assertEqual(json.loads(self.state.read_text())["events"], [])

    def test_release_source_commit_must_match_even_with_valid_binary_hashes(self):
        self.config["expected_commit"] = "c" * 40
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("pinned release", result["error"])
        self.assertEqual(json.loads(self.state.read_text())["events"], [])

    def test_startup_store_is_backed_up_without_logging_its_contents(self):
        startup = self.home / "startup.json"
        startup.write_text("synthetic-sensitive-startup")
        self.config["startup_store"] = str(startup)
        code, result = self.invoke()
        self.assertEqual(code, 0, result)
        saved = Path(result["backup"]) / "agent-startup.json"
        self.assertEqual(saved.read_text(), startup.read_text())
        self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
        self.assertNotIn("synthetic-sensitive-startup", (self.job / "result.json").read_text())

    def test_separate_cli_install_path_is_updated_and_backed_up(self):
        cli_directory = self.home / "cli-bin"
        cli_directory.mkdir()
        cli = cli_directory / "treer"
        self.binary(cli, self.old)
        self.config["cli_path"] = str(cli)
        original_alias = (self.bindir / "treer").read_bytes()
        code, result = self.invoke()
        self.assertEqual(code, 0, result)
        self.assertEqual(update.digest(cli), update.digest(self.artifacts / "treer"))
        self.assertEqual((self.bindir / "treer").read_bytes(), original_alias)
        self.assertIn(self.old, (Path(result["backup"]) / "treer").read_text())

    def test_local_manifest_must_match_online(self):
        real_fetch = update.fetch
        changed = (self.artifacts / "SHA256SUMS").read_bytes().replace(
            update.digest(self.artifacts / "treer").encode(), b"0" * 64)
        def online(url, *args, **kwargs):
            return changed if url.endswith("SHA256SUMS") else real_fetch(url, *args, **kwargs)
        with patch.object(update, "fetch", side_effect=online):
            code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("differs from Canary", result["error"])
        self.assertEqual(json.loads(self.state.read_text())["events"], [])

    def test_redirect_is_rejected(self):
        self.redirect = True
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("redirect refused", result["error"])

    def test_cli_failure_rolls_back_and_reactivates_original_controller(self):
        self.binary(self.artifacts / "treer", self.new, "installed-cli")
        self.refresh_sums()
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertEqual(result["status"], "rolled_back")
        for name in update.BINARIES[:2]:
            self.assertIn(self.old, (self.bindir / name).read_text())
        self.assertEqual(json.loads(self.state.read_text())["events"],
                         ["bbbbbbb:restart-controller", "aaaaaaa:restart-controller"])

    def test_pid_change_and_missing_capability_reject_activation(self):
        self.binary(self.artifacts / "treer-agent-server", self.new, "capability")
        self.refresh_sums()
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertEqual(result["status"], "rolled_back")
        self.assertIn("path-target capability", result["error"])
        with self.assertRaisesRegex(RuntimeError, "PID/generation"):
            update.check_agents({"a": {"pid": 1, "generation": 1}}, {"a": {"pid": 2, "generation": 1}})

    def test_changed_agent_process_is_reported_even_after_binary_rollback(self):
        self.binary(self.artifacts / "treer-agent-server", self.new, "pid")
        self.refresh_sums()
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertEqual(result["status"], "rollback_failed")
        self.assertIn("PID/generation", result["error"])
        self.assertIn(self.old, (self.bindir / "treer-agent-server").read_text())

    def test_existing_lock_is_preserved(self):
        lock = self.bindir / ".treer-machine-update.lock"
        lock.mkdir()
        code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertTrue(lock.is_dir())
        self.assertIn("another update", result["error"])

    def test_cold_restart_is_explicit_and_generation_changes_are_expected(self):
        self.config["host"] = True
        code, result = self.invoke()
        self.assertEqual(code, 0, result)
        self.assertEqual(json.loads(self.state.read_text())["events"], ["bbbbbbb:restart"])
        self.assertTrue(result["host_changed"])
        self.assertEqual(result["agents"]["ag_fixture"]["generation"], 4)

    def test_hot_download_path_and_ambiguous_install_guard(self):
        self.config["artifacts"] = None
        code, result = self.invoke()
        self.assertEqual(code, 0, result)
        (self.configdir / "other-controller.json").write_text(self.configpath.read_text())
        with self.assertRaisesRegex(RuntimeError, "select --workspace explicitly"):
            update.installed(self.configdir)

    def other_install(self):
        other = self.configdir / "srv_other-controller.json"
        value = json.loads(self.configpath.read_text())
        value.update(workspace="ws_other", server_id="srv_other")
        other.write_text(json.dumps(value))
        return other

    def test_multiple_installs_need_selection_and_confirmation_before_any_update(self):
        other = self.other_install()
        original = other.read_bytes()
        with self.assertRaisesRegex(RuntimeError, "--workspace ws_other --config-dir"):
            update.installed(self.configdir)
        for confirmation in (False, None, "true", 1):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaisesRegex(RuntimeError, "require --accept-shared-binaries"):
                update.installed(self.configdir, "ws_fixture", confirmation)
        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            update.installed(self.configdir, "ws_missing", True)
        self.assertEqual(update.installed(self.configdir, "ws_other", True)[0], other)
        with patch.object(update, "fetch", side_effect=AssertionError("must fail before fetching")):
            code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertIn("--accept-shared-binaries", result["error"])
        self.assertEqual(json.loads(self.state.read_text())["events"], [])
        self.assertIn(self.old, (self.bindir / "treer").read_text())
        self.assertEqual(other.read_bytes(), original)

    def test_confirmed_multi_install_updates_selected_controller_only(self):
        other = self.other_install()
        original = other.read_bytes()
        self.config["accept_shared_binaries"] = True
        warning = io.StringIO()
        with contextlib.redirect_stderr(warning):
            code, result = self.invoke()
        self.assertEqual(code, 0, result)
        self.assertIn("other workspaces", warning.getvalue())
        self.assertIn("next start", warning.getvalue())
        self.assertNotIn("test-only-secret", warning.getvalue())
        self.assertEqual(other.read_bytes(), original)
        self.assertEqual(json.loads(self.state.read_text())["events"], ["bbbbbbb:restart-controller"])

    def test_confirmed_multi_install_activation_failure_rolls_back_selected_controller_only(self):
        # Give the second workspace its own endpoint and state so accidentally
        # polling it cannot be hidden by two configurations sharing a listener.
        other_requests = []
        other_state = {"controller_epoch": "other-epoch", "agents": [
            {"agent_id": "ag_other", "pid": 555, "run_generation": 7}]}

        class OtherController(BaseHTTPRequestHandler):
            def do_GET(self):
                other_requests.append(self.path)
                data = json.dumps(other_state).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), OtherController)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        other = self.other_install()
        value = json.loads(other.read_text())
        value["listen"] = f"127.0.0.1:{server.server_port}"
        other.write_text(json.dumps(value))
        original_config = other.read_bytes()
        original_state = json.dumps(other_state, sort_keys=True)
        originals = {name: (self.bindir / name).read_bytes() for name in update.BINARIES}
        self.config["accept_shared_binaries"] = True
        # The real fake executable records an attempted activation, then exits
        # unsuccessfully; rollback must execute the restored old Controller.
        self.binary(self.artifacts / "treer-agent-server", self.new, "activation")
        self.refresh_sums()
        with contextlib.redirect_stderr(io.StringIO()), patch.object(update, "run", wraps=update.run) as run:
            code, result = self.invoke()
        self.assertEqual(code, 1)
        self.assertEqual(result["status"], "rolled_back", result)
        self.assertIsNone(result["rollback_error"])
        self.assertEqual(originals, {name: (self.bindir / name).read_bytes() for name in update.BINARIES})
        activations = [call.args[0] for call in run.call_args_list if "service" in call.args[0]]
        self.assertEqual(activations, [[str(self.bindir / "treer-agent-server"), "service",
            "--workspace", "ws_fixture", "restart-controller"]] * 2)
        selected = json.loads(self.state.read_text())
        self.assertEqual(selected["events"], ["bbbbbbb:restart-controller", "aaaaaaa:restart-controller"])
        self.assertTrue(selected["epoch"].startswith(self.old))
        self.assertEqual(selected["agents"], self.initial["agents"])
        self.assertEqual(other.read_bytes(), original_config)
        self.assertEqual(json.dumps(other_state, sort_keys=True), original_state)
        self.assertEqual(other_requests, [])

    def test_candidates_only_select_workspace_and_never_execute_updates(self):
        self.other_install()
        with self.assertRaises(RuntimeError) as failure:
            update.installed(self.configdir)
        diagnostic = str(failure.exception)
        self.assertIn("--workspace ws_other --config-dir", diagnostic)
        self.assertNotIn("--accept-shared-binaries", diagnostic)
        self.assertNotIn("--apply", diagnostic)
        probe = json.dumps({"home": str(self.home), "system": "Linux", "arch": "x86_64",
                            "controller": str(self.bindir / "treer-agent-server"),
                            "config_base": str(self.home / ".config")})
        selection = json.dumps({"error": diagnostic, "shared": True})
        warning = io.StringIO()
        with patch.object(sys, "argv", [str(SCRIPT), "--machine", "local", "--apply"]), \
             patch.object(update.Machine, "python", side_effect=[probe, "", selection]) as python, \
             patch.object(update.Machine, "upload") as upload, \
             patch.object(update.subprocess, "run") as execute, \
             patch.object(update, "replace_file") as replace, \
             contextlib.redirect_stderr(warning):
            with self.assertRaisesRegex(RuntimeError, "select --workspace explicitly"):
                update.main()
            # Discovery uploads only its worker for read-only inspection; no
            # candidate command, artifact upload or detached worker is started.
            self.assertEqual(upload.call_count, 1)
            self.assertEqual(python.call_count, 3)
            self.assertFalse(any("Popen(" in call.args[0] for call in python.call_args_list))
            execute.assert_not_called()
            replace.assert_not_called()
            self.assertNotIn(update.SHARED_BINARIES_NOTICE, warning.getvalue())
        self.assertEqual(json.loads(self.state.read_text())["events"], [])

    def test_apply_in_operator_argv_is_not_a_shared_binary_confirmation(self):
        self.other_install()
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(RuntimeError) as failure:
            update.installed(self.configdir, "ws_fixture")
        diagnostic = str(failure.exception)
        self.assertIn("--accept-shared-binaries", diagnostic)
        probe = json.dumps({"home": str(self.home), "system": "Linux", "arch": "x86_64",
                            "controller": str(self.bindir / "treer-agent-server"),
                            "config_base": str(self.home / ".config")})
        selection = json.dumps({"error": diagnostic, "shared": True})
        warning = io.StringIO()
        with patch.object(sys, "argv", [str(SCRIPT), "--machine", "local", "--workspace", "ws_fixture", "--apply"]), \
             patch.object(update.Machine, "python", side_effect=[probe, "", selection]) as python, \
             patch.object(update.Machine, "upload") as upload, \
             patch.object(update.subprocess, "run") as execute, \
             patch.object(update, "replace_file") as replace, \
             contextlib.redirect_stderr(warning):
            with self.assertRaisesRegex(RuntimeError, "--accept-shared-binaries"):
                update.main()
            self.assertIn("other workspaces", warning.getvalue())
            self.assertEqual(upload.call_count, 1)
            self.assertFalse(any("Popen(" in call.args[0] for call in python.call_args_list))
            execute.assert_not_called()
            replace.assert_not_called()

    def test_workspace_selection_keeps_duplicate_and_foreign_hostname_guards(self):
        other = self.other_install()
        value = json.loads(other.read_text())
        value["install_hostname"] = "foreign-machine"
        other.write_text(json.dumps(value))
        self.assertEqual(update.installed(self.configdir)[0], self.configpath)
        other.write_text(self.configpath.read_text())
        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            update.installed(self.configdir, "ws_fixture", True)


class TransportTest(unittest.TestCase):
    def test_windows_update_is_rejected_before_job_or_binary_writes(self):
        # Native Windows has detached services/shared locked .exe files. This
        # Unix updater must not pretend that Unix rename/activation is supported.
        probe = json.dumps({"home": r"C:\Users\fixture", "system": "Windows", "arch": "AMD64",
                            "controller": r"C:\Users\fixture\Treer\bin\treer-agent-server.exe",
                            "config_base": r"C:\Users\fixture\AppData\Local"})
        with patch.object(sys, "argv", [str(SCRIPT), "--machine", "local", "--apply", "--workspace", "ws_fixture", "--accept-shared-binaries"]), \
             patch.object(update.Machine, "python", return_value=probe) as python, \
             patch.object(update.Machine, "upload") as upload:
            with self.assertRaisesRegex(RuntimeError, "require a Unix machine"):
                update.main()
            self.assertEqual(python.call_count, 1)
            upload.assert_not_called()

    def test_remote_exec_uses_no_stdin_short_timeout_and_checks_envelope(self):
        with patch.object(update.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, json.dumps({"exit_code": 0,
                "stdout": "ok", "timed_out": False, "truncated": False}), "")
            self.assertEqual(update.Machine("mariana").python("print('ok')"), "ok")
            args = run.call_args.args[0]
            self.assertEqual(args[:4], ["treer", "exec", "--machine", "mariana"])
            self.assertIn("25000", args)
            self.assertEqual(run.call_args.kwargs["stdin"], subprocess.DEVNULL)
            for key in ("timed_out", "truncated"):
                value = {"exit_code": 0, "stdout": "", key: True}
                run.return_value.stdout = json.dumps(value)
                with self.assertRaises(RuntimeError):
                    update.Machine("mariana").python("pass")

    def test_upload_chunks_are_bounded_and_byte_exact(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, target = root / "source", root / "target"
            source.write_bytes(os.urandom(90_000))
            machine = update.Machine("local")
            commands = []
            original = machine.python
            def record(code):
                commands.append(code)
                return original(code)
            with patch.object(machine, "python", side_effect=record):
                machine.upload(source, target)
            self.assertEqual(source.read_bytes(), target.read_bytes())
            self.assertLess(max(map(len, commands)), 34_000)

    def test_clean_service_command_derives_only_the_private_target_user_bus(self):
        for owner, mode, allowed in [(1000, 0o40700, True), (1001, 0o40700, False), (1000, 0o40777, False)]:
            info = os.stat_result((mode, 0, 0, 1, owner, 1000, 0, 0, 0, 0))
            with patch.object(update.sys, "platform", "linux"), patch.object(update.os, "getuid", return_value=1000), \
                 patch.object(Path, "stat", return_value=info), patch.object(Path, "is_socket", return_value=True), \
                 patch.object(update.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "ok", "")) as run:
                update.run(["fake-controller", "service"], "/home/test", bin_dir="/home/test/bin")
                environment = run.call_args.kwargs["env"]
                self.assertEqual("XDG_RUNTIME_DIR" in environment, allowed)
                self.assertEqual("DBUS_SESSION_BUS_ADDRESS" in environment, allowed)
                if allowed:
                    self.assertEqual(environment["DBUS_SESSION_BUS_ADDRESS"], "unix:path=/run/user/1000/bus")
                self.assertNotIn("TREER_WORKLOAD_CREDENTIAL", environment)

    def test_detached_worker_survives_launcher_and_has_clean_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "environment.json"
            probe = root / "probe.py"
            probe.write_text("import os,json,time; time.sleep(.1); "
                f"open({str(output)!r},'w').write(json.dumps({{'keys':list(os.environ),'sid':os.getsid(0),'pid':os.getpid()}}))")
            command = ["nohup", "env", "-i", *[f"{k}={v}" for k,v in update.clean_env(root).items()],
                       sys.executable, str(probe)]
            update.Machine("local").python(f"import subprocess; subprocess.Popen({command!r},"
                "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)")
            import time
            for _ in range(50):
                if output.exists():
                    break
                time.sleep(.02)
            value = json.loads(output.read_text())
            self.assertEqual(value["sid"], value["pid"])
            self.assertFalse(any(k.startswith(("TREER_", "AGENT_UI_", "CODEX_UI_")) for k in value["keys"]))

    def test_dry_run_and_host_warning_do_not_contact_machine(self):
        for args in (["--machine", "mariana"], ["--machine", "local", "--host"]):
            result = subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True)
            if "--host" in args:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("terminates all Agents", result.stderr)
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(json.loads(result.stdout)["changes"])


if __name__ == "__main__":
    unittest.main()
