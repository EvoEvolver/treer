#!/usr/bin/env python3
"""Real-process Core Message, Mail, and Telegram end-to-end harness."""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable


ROOT = Path(__file__).resolve().parents[1]
TREER = ROOT / "target/debug/treer"
PROXY = ROOT / "target/debug/treer-proxy"
CONTROLLER = ROOT / "target/debug/treer-agent-server"
HOST = ROOT / "target/debug/treer-agent-host"
DEFAULT_DATABASE_URL = "postgres://treer:treer@127.0.0.1:55432/treer_test"
PROCESS_TIMEOUT = 30
WAIT_TIMEOUT = 30.0


class E2EFailure(RuntimeError):
    pass


def log(message: str) -> None:
    print(f"e2e: {message}", flush=True)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise E2EFailure(message)


def json_write(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + f".{uuid.uuid4().hex}.tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def wait_for(
    predicate: Callable[[], Any],
    description: str,
    *,
    timeout: float = WAIT_TIMEOUT,
    interval: float = 0.1,
) -> Any:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:  # Transient process/network failures are expected while starting.
            last_error = error
        time.sleep(interval)
    detail = f": {last_error}" if last_error else ""
    raise E2EFailure(f"timed out waiting for {description}{detail}")


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def command(
    argv: list[str],
    *,
    env: dict[str, str] | None = None,
    stdin: str | None = None,
    timeout: int = PROCESS_TIMEOUT,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        argv,
        cwd=ROOT,
        env=env,
        input=stdin,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    if check and completed.returncode != 0:
        rendered = " ".join(argv)
        raise E2EFailure(
            f"command failed ({completed.returncode}): {rendered}\n"
            f"stdout: {completed.stdout[-2000:]}\nstderr: {completed.stderr[-2000:]}"
        )
    return completed


class ManagedProcess:
    def __init__(
        self,
        label: str,
        argv: list[str],
        log_path: Path,
        *,
        env: dict[str, str] | None = None,
    ) -> None:
        self.label = label
        self.log_path = log_path
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self._log = log_path.open("ab")
        self.process = subprocess.Popen(
            argv,
            cwd=ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=self._log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    def require_running(self) -> bool:
        status = self.process.poll()
        if status is None:
            return True
        raise E2EFailure(
            f"{self.label} exited with {status}; log tail:\n{self.tail()}"
        )

    def tail(self, maximum: int = 4000) -> str:
        self._log.flush()
        try:
            return self.log_path.read_text(encoding="utf-8", errors="replace")[-maximum:]
        except OSError:
            return "<log unavailable>"

    def stop(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=5)
        self._log.close()


def request_url(
    url: str,
    method: str = "GET",
    body: Any | None = None,
    headers: dict[str, str] | None = None,
    *,
    timeout: float = 5.0,
) -> tuple[int, dict[str, str], Any | None]:
    parsed = urllib.parse.urlsplit(url)
    require(parsed.scheme == "http", f"E2E only supports local HTTP URLs, got {url}")
    host = parsed.hostname
    require(host is not None, f"URL has no host: {url}")
    port = parsed.port or 80
    path = urllib.parse.urlunsplit(("", "", parsed.path or "/", parsed.query, ""))
    request_headers = dict(headers or {})
    encoded: bytes | None = None
    if body is not None:
        encoded = json.dumps(body, separators=(",", ":")).encode("utf-8")
        request_headers.setdefault("content-type", "application/json")
    connection = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        connection.request(method, path, body=encoded, headers=request_headers)
        response = connection.getresponse()
        raw = response.read()
        response_headers = {key.lower(): value for key, value in response.getheaders()}
    finally:
        connection.close()
    value: Any | None = None
    if raw:
        try:
            value = json.loads(raw)
        except ValueError:
            value = raw.decode("utf-8", errors="replace")
    return response.status, response_headers, value


def require_json_response(
    url: str,
    method: str = "GET",
    body: Any | None = None,
    headers: dict[str, str] | None = None,
) -> dict[str, Any]:
    status, _, value = request_url(url, method, body, headers)
    if not 200 <= status < 300 or not isinstance(value, dict):
        raise E2EFailure(f"{method} {url} returned HTTP {status}: {value}")
    return value


def database_url_for(base_url: str, database: str) -> str:
    parsed = urllib.parse.urlsplit(base_url)
    require(parsed.scheme in {"postgres", "postgresql"}, "test database URL must be PostgreSQL")
    return urllib.parse.urlunsplit(
        (parsed.scheme, parsed.netloc, f"/{database}", parsed.query, parsed.fragment)
    )


def psql(
    database_url: str,
    sql: str,
    *,
    variables: dict[str, str] | None = None,
    tuples: bool = False,
) -> str:
    argv = ["psql", database_url, "--no-psqlrc", "--set", "ON_ERROR_STOP=1"]
    for key, value in (variables or {}).items():
        argv.extend(["--set", f"{key}={value}"])
    if tuples:
        argv.extend(["--tuples-only", "--no-align"])
    return command(argv, stdin=sql, timeout=60).stdout.strip()


class TemporaryDatabases:
    def __init__(self, base_url: str) -> None:
        suffix = uuid.uuid4().hex[:12]
        self.base_url = base_url
        self.core_name = f"treer_e2e_{suffix}"
        self.legacy_name = f"treer_legacy_{suffix}"
        self.core_url = database_url_for(base_url, self.core_name)
        self.legacy_url = database_url_for(base_url, self.legacy_name)

    def create(self) -> None:
        psql(
            self.base_url,
            f"CREATE DATABASE {self.core_name};\nCREATE DATABASE {self.legacy_name};\n",
        )

    def drop(self) -> None:
        for name in (self.core_name, self.legacy_name):
            try:
                psql(self.base_url, f"DROP DATABASE IF EXISTS {name} WITH (FORCE);\n")
            except Exception as error:
                print(f"e2e: warning: failed to drop temporary database {name}: {error}", file=sys.stderr)


class AgentDriver:
    def __init__(self, control_dir: Path, agent: dict[str, Any]) -> None:
        self.control_dir = control_dir
        self.agent = agent
        self.sequence = 0

    @property
    def agent_id(self) -> str:
        return str(self.agent["agent_id"])

    def invoke(self, request: dict[str, Any], *, timeout: float = WAIT_TIMEOUT) -> dict[str, Any]:
        self.sequence += 1
        name = f"{self.sequence:05d}"
        command_path = self.control_dir / "commands" / f"{name}.json"
        result_path = self.control_dir / "results" / f"{name}.json"
        json_write(command_path, request)

        def completed() -> dict[str, Any] | None:
            if not result_path.is_file():
                return None
            value = json.loads(result_path.read_text(encoding="utf-8"))
            return value if isinstance(value, dict) else None

        return wait_for(completed, f"Agent {self.agent_id} command {name}", timeout=timeout)

    def run(
        self,
        argv: list[str],
        *,
        stdin: str | None = None,
        env: dict[str, str] | None = None,
        timeout_seconds: int = PROCESS_TIMEOUT,
        check: bool = True,
    ) -> dict[str, Any]:
        result = self.invoke(
            {
                "action": "run",
                "argv": argv,
                "stdin": stdin,
                "env": env or {},
                "timeout_seconds": timeout_seconds,
            },
            timeout=timeout_seconds + 10,
        )
        if check and result.get("returncode") != 0:
            raise E2EFailure(
                f"Agent {self.agent_id} command failed: {argv}\n"
                f"stdout: {result.get('stdout', '')}\nstderr: {result.get('stderr', '')}"
            )
        return result

    def run_json(self, argv: list[str], **kwargs: Any) -> dict[str, Any]:
        result = self.run(argv, **kwargs)
        try:
            value = json.loads(str(result.get("stdout", "")))
        except ValueError as error:
            raise E2EFailure(f"Agent {self.agent_id} returned invalid JSON: {result}") from error
        require(isinstance(value, dict), "Treer CLI result must be a JSON object")
        return value

    def start(self, process_id: str, argv: list[str], *, env: dict[str, str] | None = None) -> None:
        result = self.invoke(
            {"action": "start", "process_id": process_id, "argv": argv, "env": env or {}}
        )
        require(result.get("started") is True, f"Agent child {process_id} did not start: {result}")

    def stop(self, process_id: str) -> None:
        result = self.invoke({"action": "stop", "process_id": process_id})
        require(result.get("stopped") is True, f"Agent child {process_id} did not stop: {result}")


class MachineStack:
    def __init__(
        self,
        root: Path,
        label: str,
        proxy_url: str,
        workspace_id: str,
        enrollment: dict[str, Any],
        shared_environment: dict[str, str],
    ) -> None:
        self.label = label
        self.workspace_id = workspace_id
        self.proxy_url = proxy_url
        self.server_id = str(enrollment["server_id"])
        self.machine_token = str(enrollment["machine_token"])
        self.listen_port = free_port()
        self.local_url = f"http://127.0.0.1:{self.listen_port}/"
        self.operator_credential = f"opc_{uuid.uuid4().hex}{uuid.uuid4().hex}"
        self.host_socket = root / label / "host.sock"
        controller_config = root / label / "controller.json"
        host_config = root / label / "host.json"
        json_write(
            controller_config,
            {
                "proxy": proxy_url,
                "workspace": workspace_id,
                "server_id": self.server_id,
                "machine_token": self.machine_token,
                "operator_credential": self.operator_credential,
                "root": str(ROOT),
                "listen": f"127.0.0.1:{self.listen_port}",
                "host_socket": str(self.host_socket),
                "install_hostname": socket.gethostname(),
            },
        )
        json_write(
            host_config,
            {
                "socket_path": str(self.host_socket),
                "controller_path": str(CONTROLLER),
                "controller_config_path": str(controller_config),
                "root": str(ROOT),
            },
        )
        environment = dict(shared_environment)
        environment["TREER_NETWORK_MODE"] = "proxy-env"
        environment["RUST_LOG"] = "treer_agent_host=info,treer_agent_server=info"
        self.process = ManagedProcess(
            f"Host/Controller {label}",
            [str(HOST), "run", "--config", str(host_config)],
            root / "logs" / f"host-{label}.log",
            env=environment,
        )
        wait_for(self.healthy, f"Host/Controller {label}")

    def healthy(self) -> bool:
        self.process.require_running()
        status, _, value = request_url(urllib.parse.urljoin(self.local_url, "api/health"))
        return (
            status == 200
            and isinstance(value, dict)
            and value.get("workspace_id") == self.workspace_id
            and value.get("server_id") == self.server_id
        )

    def cli_environment(self, shared_environment: dict[str, str]) -> dict[str, str]:
        environment = dict(shared_environment)
        environment.update(
            {
                "TREER_AGENT_SERVER_URL": self.local_url,
                "TREER_WORKSPACE_ID": self.workspace_id,
                "TREER_OPERATOR_CREDENTIAL": self.operator_credential,
            }
        )
        environment.pop("TREER_AGENT_ID", None)
        environment.pop("TREER_WORKLOAD_CREDENTIAL", None)
        return environment

    def cli_json(
        self,
        shared_environment: dict[str, str],
        arguments: list[str],
        *,
        check: bool = True,
    ) -> dict[str, Any]:
        completed = command(
            [str(TREER), "--url", self.local_url, "--workspace", self.workspace_id, *arguments],
            env=self.cli_environment(shared_environment),
            check=check,
        )
        if not check and completed.returncode != 0:
            return {
                "returncode": completed.returncode,
                "stdout": completed.stdout,
                "stderr": completed.stderr,
            }
        value = json.loads(completed.stdout)
        require(isinstance(value, dict), "operator CLI result must be a JSON object")
        return value

    def create_driver(
        self,
        shared_environment: dict[str, str],
        control_dir: Path,
        name: str,
    ) -> AgentDriver:
        control_dir.mkdir(parents=True, exist_ok=True)
        value = self.cli_json(
            shared_environment,
            [
                "create",
                "--server",
                self.server_id,
                "--kind",
                "command",
                "--name",
                name,
                "--cwd",
                ".",
                "--",
                "python3",
                str(Path(__file__).resolve()),
                "--agent-driver",
                str(control_dir),
            ],
        )
        wait_for(lambda: (control_dir / "ready.json").is_file(), f"Agent driver {name}")
        ready = json.loads((control_dir / "ready.json").read_text(encoding="utf-8"))
        require(ready.get("agent_id") == value.get("agent_id"), "Agent driver identity mismatch")
        return AgentDriver(control_dir, value)

    def restart_controller(self) -> None:
        previous = require_json_response(urllib.parse.urljoin(self.local_url, "api/health"))[
            "controller_epoch"
        ]
        command([str(HOST), "restart-controller", "--socket", str(self.host_socket)])

        def restarted() -> bool:
            if not self.healthy():
                return False
            current = require_json_response(urllib.parse.urljoin(self.local_url, "api/health"))
            return current.get("controller_epoch") != previous

        wait_for(restarted, f"Controller restart for {self.label}")

    def stop(self) -> None:
        self.process.stop()


class FakeTelegram:
    def __init__(self, port: int) -> None:
        self.lock = threading.Lock()
        self.updates: list[dict[str, Any]] = []
        self.sent: list[dict[str, Any]] = []
        self.next_message_id = 101
        state = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                try:
                    length = int(self.headers.get("content-length", "0"))
                    payload = json.loads(self.rfile.read(length) or b"{}")
                    method = self.path.rsplit("/", 1)[-1]
                    if not self.path.startswith("/bot999:test/"):
                        self._reply(404, {"ok": False, "error_code": 404})
                    elif method == "getMe":
                        self._reply(
                            200,
                            {"ok": True, "result": {"id": 999, "is_bot": True, "username": "treer_e2e"}},
                        )
                    elif method == "getUpdates":
                        offset = int(payload.get("offset", 0))
                        with state.lock:
                            updates = [item for item in state.updates if int(item["update_id"]) >= offset]
                        if not updates:
                            time.sleep(0.05)
                        self._reply(200, {"ok": True, "result": updates})
                    elif method == "sendMessage":
                        with state.lock:
                            message_id = state.next_message_id
                            state.next_message_id += 1
                            state.sent.append({"message_id": message_id, "payload": payload})
                        self._reply(
                            200,
                            {
                                "ok": True,
                                "result": {
                                    "message_id": message_id,
                                    "date": int(time.time()),
                                    "chat": {"id": payload.get("chat_id"), "type": "private"},
                                    "text": payload.get("text", ""),
                                },
                            },
                        )
                    else:
                        self._reply(404, {"ok": False, "error_code": 404})
                except Exception:
                    self._reply(500, {"ok": False, "error_code": 500})

            def _reply(self, status: int, value: dict[str, Any]) -> None:
                encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{port}"

    def add_update(
        self,
        update_id: int,
        telegram_message_id: int,
        text: str,
        *,
        reply_to: int | None = None,
    ) -> None:
        message: dict[str, Any] = {
            "message_id": telegram_message_id,
            "message_thread_id": 7,
            "date": int(time.time()),
            "chat": {"id": 42, "type": "supergroup"},
            "from": {"id": 7, "is_bot": False, "first_name": "E2E"},
            "text": text,
        }
        if reply_to is not None:
            message["reply_to_message"] = {"message_id": reply_to}
        with self.lock:
            self.updates.append({"update_id": update_id, "message": message})

    def sent_message(self, text: str) -> dict[str, Any] | None:
        with self.lock:
            return next(
                (dict(item) for item in self.sent if item.get("payload", {}).get("text") == text),
                None,
            )

    def stop(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def driver_main(control_dir: Path) -> int:
    commands = control_dir / "commands"
    results = control_dir / "results"
    logs = control_dir / "logs"
    for directory in (commands, results, logs):
        directory.mkdir(parents=True, exist_ok=True)
    agent_id = os.environ.get("TREER_AGENT_ID", "")
    if not agent_id or not os.environ.get("TREER_WORKLOAD_CREDENTIAL"):
        print("agent driver requires a managed Treer Agent identity", file=sys.stderr)
        return 2
    children: dict[str, tuple[subprocess.Popen[bytes], Any]] = {}
    stopping = False

    def stop_children() -> None:
        for process_id in list(children):
            process, output = children.pop(process_id)
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            output.close()

    def stop(_signum: int, _frame: object) -> None:
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    json_write(control_dir / "ready.json", {"agent_id": agent_id, "pid": os.getpid()})
    try:
        while not stopping:
            pending = sorted(path for path in commands.glob("*.json") if not (results / path.name).exists())
            if not pending:
                time.sleep(0.05)
                continue
            for path in pending:
                result_path = results / path.name
                try:
                    request = json.loads(path.read_text(encoding="utf-8"))
                    action = request.get("action")
                    if action == "run":
                        environment = os.environ.copy()
                        environment.update({str(key): str(value) for key, value in request.get("env", {}).items()})
                        completed = subprocess.run(
                            [str(value) for value in request["argv"]],
                            input=request.get("stdin"),
                            text=True,
                            capture_output=True,
                            timeout=int(request.get("timeout_seconds", PROCESS_TIMEOUT)),
                            check=False,
                            env=environment,
                            cwd=ROOT,
                        )
                        result = {
                            "returncode": completed.returncode,
                            "stdout": completed.stdout,
                            "stderr": completed.stderr,
                        }
                    elif action == "start":
                        process_id = str(request["process_id"])
                        require(process_id not in children, f"child {process_id} is already running")
                        environment = os.environ.copy()
                        environment.update({str(key): str(value) for key, value in request.get("env", {}).items()})
                        output = (logs / f"{process_id}.log").open("ab")
                        process = subprocess.Popen(
                            [str(value) for value in request["argv"]],
                            cwd=ROOT,
                            env=environment,
                            stdin=subprocess.DEVNULL,
                            stdout=output,
                            stderr=subprocess.STDOUT,
                        )
                        children[process_id] = (process, output)
                        time.sleep(0.1)
                        require(process.poll() is None, f"child {process_id} exited during startup")
                        result = {"started": True, "pid": process.pid}
                    elif action == "stop":
                        process_id = str(request["process_id"])
                        require(process_id in children, f"child {process_id} is not running")
                        process, output = children.pop(process_id)
                        if process.poll() is None:
                            process.terminate()
                            try:
                                process.wait(timeout=5)
                            except subprocess.TimeoutExpired:
                                process.kill()
                                process.wait(timeout=5)
                        output.close()
                        result = {"stopped": True, "returncode": process.returncode}
                    elif action == "exit":
                        result = {"exiting": True}
                        stopping = True
                    else:
                        raise E2EFailure(f"unknown agent driver action {action}")
                except Exception as error:
                    result = {"driver_error": f"{type(error).__name__}: {error}"}
                json_write(result_path, result)
    finally:
        stop_children()
    return 0


class Harness:
    def __init__(self, root: Path, database_url: str) -> None:
        self.root = root
        self.databases = TemporaryDatabases(database_url)
        self.proxy_port = free_port()
        self.mail_port = free_port()
        self.proxy_url = f"http://127.0.0.1:{self.proxy_port}/"
        self.shared_environment = os.environ.copy()
        self.shared_environment.update(
            {
                "XDG_DATA_HOME": str(root / "xdg/data"),
                "XDG_STATE_HOME": str(root / "xdg/state"),
                "XDG_CONFIG_HOME": str(root / "xdg/config"),
                "XDG_CACHE_HOME": str(root / "xdg/cache"),
                "TREER_INVITATION_REQUIRED": "false",
            }
        )
        self.proxy: ManagedProcess | None = None
        self.machines: list[MachineStack] = []
        self.telegram: FakeTelegram | None = None
        self.user_headers: dict[str, str] = {}
        self.user_id = ""
        self.organization_id = ""
        self.workspace_a = f"e2e-a-{uuid.uuid4().hex[:8]}"
        self.workspace_b = f"e2e-b-{uuid.uuid4().hex[:8]}"

    def start_proxy(self) -> None:
        environment = dict(self.shared_environment)
        environment.update(
            {
                "DATABASE_URL": self.databases.core_url,
                "RUST_LOG": "treer_proxy=info",
            }
        )
        self.proxy = ManagedProcess(
            "Proxy",
            [
                str(PROXY),
                "--admin-password",
                "e2e-admin-password",
                "--listen",
                f"127.0.0.1:{self.proxy_port}",
                "--public-url",
                self.proxy_url,
                "--app-public-url",
                self.proxy_url,
                "--ingress-public-url",
                f"http://ingress.test:{self.mail_port}/",
            ],
            self.root / "logs/proxy.log",
            env=environment,
        )
        wait_for(self.proxy_healthy, "Proxy health")

    def proxy_healthy(self) -> bool:
        require(self.proxy is not None, "Proxy process is missing")
        self.proxy.require_running()
        status, _, value = request_url(urllib.parse.urljoin(self.proxy_url, "api/auth/config"))
        return status == 200 and isinstance(value, dict)

    def restart_proxy(self) -> None:
        require(self.proxy is not None, "Proxy process is missing")
        self.proxy.stop()
        self.proxy = None
        self.start_proxy()
        for machine in self.machines:
            wait_for(
                lambda machine=machine: self.controller_reaches_proxy(machine),
                f"{machine.label} reconnect to Proxy",
            )

    def controller_reaches_proxy(self, machine: MachineStack) -> bool:
        value = machine.cli_json(self.shared_environment, ["discover"])
        return value.get("workspace", {}).get("workspace_id") == machine.workspace_id

    def setup_identity_and_workspaces(self) -> None:
        status, headers, user = request_url(
            urllib.parse.urljoin(self.proxy_url, "api/auth/register"),
            "POST",
            {
                "invite": None,
                "email": "e2e@treer.invalid",
                "preferred_name": "Messaging E2E",
                "password": "e2e-user-password",
            },
        )
        require(status == 200 and isinstance(user, dict), f"E2E user registration failed: {user}")
        cookie = headers.get("set-cookie", "").split(";", 1)[0]
        require(cookie.startswith("treer_session="), "E2E registration did not issue a user session")
        self.user_headers = {"cookie": cookie}
        self.user_id = str(user["user_id"])
        organization = require_json_response(
            urllib.parse.urljoin(self.proxy_url, "api/organizations"),
            "POST",
            {"name": "Treer messaging E2E"},
            self.user_headers,
        )["organization"]
        self.organization_id = str(organization["organization_id"])
        for workspace_id, name in (
            (self.workspace_a, "Messaging E2E A"),
            (self.workspace_b, "Messaging E2E B"),
        ):
            require_json_response(
                urllib.parse.urljoin(self.proxy_url, "api/workspaces"),
                "POST",
                {
                    "organization_id": self.organization_id,
                    "workspace_id": workspace_id,
                    "name": name,
                },
                self.user_headers,
            )

    def enroll(self, workspace_id: str, label: str) -> dict[str, Any]:
        bootstrap = require_json_response(
            urllib.parse.urljoin(self.proxy_url, f"api/workspaces/{workspace_id}/bootstrap"),
            "POST",
            headers=self.user_headers,
        )
        return require_json_response(
            urllib.parse.urljoin(self.proxy_url, "api/machines/enroll"),
            "POST",
            {"installation_id": f"mid_{uuid.uuid4().hex}", "name": f"E2E {label}"},
            {"authorization": f"Bearer {bootstrap['enrollment_key']}"},
        )

    def install_plugins(self) -> None:
        for plugin in ("mail", "telegram"):
            value = json.loads(
                command(
                    [str(TREER), "plugin", "install", str(ROOT / "plugins" / plugin)],
                    env=self.shared_environment,
                ).stdout
            )
            require(value.get("installed") is True, f"plugin {plugin} was not installed")

    @staticmethod
    def message_cli(*arguments: str) -> list[str]:
        return [str(TREER), "message", *arguments]

    def receive(self, driver: AgentDriver) -> list[dict[str, Any]]:
        response = driver.run_json(self.message_cli("receive", "--wait", "0", "--limit", "100"))
        deliveries = response.get("deliveries")
        require(isinstance(deliveries, list), "message receive returned no delivery list")
        return [item for item in deliveries if isinstance(item, dict)]

    def wait_delivery(self, driver: AgentDriver, body: str, *, timeout: float = WAIT_TIMEOUT) -> dict[str, Any]:
        def find() -> dict[str, Any] | None:
            for delivery in self.receive(driver):
                message = delivery.get("message", {})
                if isinstance(message, dict) and message.get("body") == body:
                    return delivery
            return None

        return wait_for(find, f"delivery {body!r} to {driver.agent_id}", timeout=timeout, interval=0.2)

    def ack(self, driver: AgentDriver, delivery: dict[str, Any], operation: str) -> None:
        driver.run_json(
            self.message_cli(
                "ack",
                str(delivery["delivery_id"]),
                "--operation-id",
                operation,
            )
        )

    def test_core(
        self,
        machine_a: MachineStack,
        machine_b: MachineStack,
        target: AgentDriver,
        sender: AgentDriver,
        other_workspace: AgentDriver,
    ) -> None:
        log("Core Message CLI, DAG, acknowledgement, restart, and workspace isolation")
        sent = sender.run_json(
            self.message_cli(
                "send",
                "--to",
                target.agent_id,
                "--idempotency-key",
                "e2e-core-root",
                "--body",
                "core root",
            )
        )
        root_message = sent["message"]
        first = self.wait_delivery(target, "core root")
        repeated = self.wait_delivery(target, "core root")
        require(first["delivery_id"] == repeated["delivery_id"], "unacknowledged delivery was not repeatable")
        reply = target.run_json(
            self.message_cli(
                "reply",
                str(root_message["message_id"]),
                "--body",
                "core reply",
                "--idempotency-key",
                "e2e-core-reply",
            )
        )
        require(
            reply["message"]["context_ids"] == [root_message["message_id"]],
            "Core reply did not preserve its DAG edge",
        )
        sender_delivery = self.wait_delivery(sender, "core reply")
        self.ack(target, first, "e2e-ack-core-root")
        self.ack(sender, sender_delivery, "e2e-ack-core-reply")

        machine_a.restart_controller()
        whoami = sender.run_json([str(TREER), "whoami"])
        require(whoami["agent"]["agent_id"] == sender.agent_id, "Host did not preserve Agent identity")

        replay_before = sender.run_json(
            self.message_cli(
                "send",
                "--to",
                target.agent_id,
                "--idempotency-key",
                "e2e-proxy-restart",
                "--body",
                "survives proxy restart",
            )
        )
        self.restart_proxy()
        replay_after = sender.run_json(
            self.message_cli(
                "send",
                "--to",
                target.agent_id,
                "--idempotency-key",
                "e2e-proxy-restart",
                "--body",
                "survives proxy restart",
            )
        )
        require(replay_after.get("idempotent_replay") is True, "send replay was not marked idempotent")
        require(
            replay_before["message"]["message_id"] == replay_after["message"]["message_id"],
            "idempotency result changed across Proxy restart",
        )
        restarted_delivery = self.wait_delivery(target, "survives proxy restart")
        self.ack(target, restarted_delivery, "e2e-ack-proxy-restart")

        isolated = sender.run(
            self.message_cli("send", "--to", other_workspace.agent_id, "--body", "must stay isolated"),
            check=False,
        )
        require(isolated.get("returncode") != 0, "cross-workspace recipient unexpectedly resolved")
        require("message_recipient_unavailable" in str(isolated.get("stderr")), "wrong isolation error")
        cross_context = other_workspace.run(
            self.message_cli(
                "send",
                "--to",
                other_workspace.agent_id,
                "--context",
                str(root_message["message_id"]),
                "--body",
                "cross context",
            ),
            check=False,
        )
        require(cross_context.get("returncode") != 0, "cross-workspace context unexpectedly resolved")
        require("message_context_not_found" in str(cross_context.get("stderr")), "wrong context error")
        require(machine_b.healthy(), "workspace B Controller stopped during isolation test")

    def create_legacy_sqlite(self, path: Path, workspace_id: str) -> None:
        sql = (ROOT / "plugins/mail/tests/fixtures/legacy-mail-v1.sqlite.sql").read_text(
            encoding="utf-8"
        )
        with sqlite3.connect(path) as connection:
            connection.executescript(sql)
            connection.execute("UPDATE messages SET workspace_id = ?", (workspace_id,))
            connection.execute("UPDATE recipients SET workspace_id = ?", (workspace_id,))
            connection.execute("UPDATE human_sessions SET workspace_id = ?", (workspace_id,))

    def test_migrations(self, machine_a: MachineStack, machine_b: MachineStack) -> None:
        log("restartable SQLite and real-psql PostgreSQL legacy Mail migration")
        sqlite_source = self.root / "legacy-mail.sqlite3"
        self.create_legacy_sqlite(sqlite_source, self.workspace_a)
        sqlite_report = self.root / "sqlite-migration.json"
        sqlite_export = self.root / "sqlite-export.jsonl"
        migration = [
            "python3",
            str(ROOT / "plugins/mail/migrate.py"),
            "--source",
            str(sqlite_source),
            "--workspace",
            self.workspace_a,
            "--treer",
            str(TREER),
            "--url",
            machine_a.local_url,
            "--report",
            str(sqlite_report),
            "--export-file",
            str(sqlite_export),
            "--batch-size",
            "2",
        ]
        environment_a = machine_a.cli_environment(self.shared_environment)
        command(migration, env=environment_a, timeout=120)
        first_report = json.loads(sqlite_report.read_text(encoding="utf-8"))
        command(migration, env=environment_a, timeout=120)
        repeated_report = json.loads(sqlite_report.read_text(encoding="utf-8"))
        require(first_report["message_count"] == 4, "SQLite migration message count changed")
        require(first_report["context_edge_count"] == 4, "SQLite migration edge count changed")
        require(first_report["read_delivery_count"] == 2, "SQLite migration read state changed")
        require(
            first_report["structural_sha256"] == repeated_report["structural_sha256"],
            "SQLite migration was not restartable",
        )

        fixture = (ROOT / "plugins/mail/tests/fixtures/legacy-mail-v1.postgres.sql").read_text(
            encoding="utf-8"
        )
        psql(self.databases.legacy_url, fixture)
        psql(
            self.databases.legacy_url,
            "UPDATE messages SET workspace_id = :'workspace';\n"
            "UPDATE recipients SET workspace_id = :'workspace';\n"
            "UPDATE human_sessions SET workspace_id = :'workspace';\n",
            variables={"workspace": self.workspace_b},
        )
        postgres_report = self.root / "postgres-migration.json"
        command(
            [
                "python3",
                str(ROOT / "plugins/mail/migrate.py"),
                "--source",
                self.databases.legacy_url,
                "--source-kind",
                "postgres",
                "--workspace",
                self.workspace_b,
                "--treer",
                str(TREER),
                "--url",
                machine_b.local_url,
                "--report",
                str(postgres_report),
            ],
            env=machine_b.cli_environment(self.shared_environment),
            timeout=120,
        )
        postgres = json.loads(postgres_report.read_text(encoding="utf-8"))
        require(postgres["message_count"] == 4, "PostgreSQL migration message count changed")
        require(postgres["delivery_count"] == 5, "PostgreSQL migration delivery count changed")
        counts = psql(
            self.databases.core_url,
            "SELECT workspace_id || ':' || count(*) FROM core_messages "
            "WHERE message_id LIKE 'legacy_%' GROUP BY workspace_id ORDER BY workspace_id;\n",
            tuples=True,
        ).splitlines()
        require(
            counts == [f"{self.workspace_a}:4", f"{self.workspace_b}:4"],
            f"Core migration counts are wrong: {counts}",
        )

    def test_mail(
        self,
        machine: MachineStack,
        target: AgentDriver,
        mail_bridge: AgentDriver,
    ) -> None:
        log("Mail plugin browser OAuth, directory, send, inbox acknowledgement, and logout")
        service = mail_bridge.run_json(
            [
                str(TREER),
                "service",
                "register",
                "E2E Mail",
                "--machine",
                machine.server_id,
                "--target-host",
                "127.0.0.1",
                "--port",
                str(self.mail_port),
                "--protocol",
                "http",
            ],
        )["service"]
        service_id = str(service["service_id"])
        ingress = mail_bridge.run_json(
            [
                str(TREER),
                "publish",
                "create",
                service_id,
                "--slug",
                "mail",
                "--access",
                "workspace",
            ],
        )["ingress"]
        mail_config = self.root / "mail.json"
        json_write(
            mail_config,
            {
                "listen": f"127.0.0.1:{self.mail_port}",
                "service_id": service_id,
                "public_url": ingress["url"],
                "proxy_public_url": self.proxy_url,
            },
        )
        mail_bridge.start(
            "mail",
            [str(TREER), "plugin", "run", "mail", "--config", str(mail_config)],
        )
        local_mail = f"http://127.0.0.1:{self.mail_port}/"
        wait_for(
            lambda: request_url(urllib.parse.urljoin(local_mail, "api/health"))[0] == 200,
            "Mail plugin health",
        )
        status, headers, oauth_start = request_url(
            urllib.parse.urljoin(local_mail, "api/auth/start?return_to=%2Finbox")
        )
        require(
            status == 302 and "location" in headers,
            f"Mail OAuth start did not redirect: HTTP {status}: {oauth_start}",
        )
        status, authorize_headers, authorize = request_url(
            headers["location"], headers=self.user_headers
        )
        require(
            status == 303 and "location" in authorize_headers,
            f"Core OAuth did not authorize: HTTP {status}: {authorize}",
        )
        callback = urllib.parse.urlsplit(authorize_headers["location"])
        local_callback = urllib.parse.urlunsplit(
            ("http", f"127.0.0.1:{self.mail_port}", callback.path, callback.query, "")
        )
        status, callback_headers, _ = request_url(local_callback)
        require(status == 302, "Mail OAuth callback did not redirect")
        cookie = callback_headers.get("set-cookie", "").split(";", 1)[0]
        require(cookie.startswith("treer_mail_session="), "Mail did not issue its browser cookie")
        authenticated = {"cookie": cookie}
        session = require_json_response(
            urllib.parse.urljoin(local_mail, "api/auth/session"), headers=authenticated
        )
        require(session["user"]["id"] == self.user_id, "Mail OAuth used the wrong human")
        directory = require_json_response(
            urllib.parse.urljoin(local_mail, "api/directory"), headers=authenticated
        )
        principal_ids = {item["id"] for item in directory["principals"]}
        require(
            target.agent_id in principal_ids and self.user_id in principal_ids,
            "Mail directory is incomplete",
        )
        sent = require_json_response(
            urllib.parse.urljoin(local_mail, "api/messages"),
            "POST",
            {"recipients": [target.agent_id], "context_ids": [], "body": "mail to target"},
            {**authenticated, "idempotency-key": "e2e-mail-send"},
        )
        require(
            sent["message"]["sender"]["id"] == self.user_id,
            "Mail did not use human Message identity",
        )
        target_delivery = self.wait_delivery(target, "mail to target")
        target.run_json(
            self.message_cli(
                "reply",
                str(sent["message"]["message_id"]),
                "--body",
                "mail reply",
                "--idempotency-key",
                "e2e-mail-reply",
            )
        )
        self.ack(target, target_delivery, "e2e-ack-mail-target")
        inbox = require_json_response(
            urllib.parse.urljoin(local_mail, "api/inbox"),
            "POST",
            {"limit": 50},
            authenticated,
        )
        replies = [item["message"] for item in inbox["deliveries"]]
        require(any(item.get("body") == "mail reply" for item in replies), "Mail inbox missed Core reply")
        require(inbox["remaining_unread"] == 0, "Mail did not acknowledge its inbox")
        status, logout_headers, _ = request_url(
            urllib.parse.urljoin(local_mail, "api/auth/logout"), "POST", {}, authenticated
        )
        require(status == 204 and "max-age=0" in logout_headers.get("set-cookie", "").lower(), "Mail logout failed")
        status, _, _ = request_url(
            urllib.parse.urljoin(local_mail, "api/auth/session"), headers=authenticated
        )
        require(status == 401, "revoked Mail session remained usable")
        mail_bridge.stop("mail")

    def install_deny_policy(self, bridge_id: str, target_id: str) -> None:
        document = {
            "schema_version": 1,
            "defaults": {},
            "groups": {},
            "rules": [
                {
                    "id": "deny-telegram-send",
                    "priority": 100,
                    "effect": "deny",
                    "subjects": [{"kind": "agent", "id": bridge_id}],
                    "actions": ["message.send"],
                    "resources": [{"kind": "message.mailbox", "id": target_id}],
                }
            ],
        }
        psql(
            self.databases.core_url,
            """
INSERT INTO workspace_policies(
  workspace_id, revision, schema_version, mode, document, updated_at,
  updated_by_kind, updated_by_id
) VALUES (:'workspace', 1, 1, 'enforce', :'document'::jsonb,
          '2026-01-01T00:00:00Z', 'human', :'user_id')
ON CONFLICT(workspace_id) DO UPDATE SET
  revision = workspace_policies.revision + 1,
  mode = 'enforce',
  document = excluded.document,
  updated_at = excluded.updated_at,
  updated_by_kind = excluded.updated_by_kind,
  updated_by_id = excluded.updated_by_id;
""",
            variables={
                "workspace": self.workspace_a,
                "document": json.dumps(document),
                "user_id": self.user_id,
            },
        )

    def clear_policy(self) -> None:
        psql(
            self.databases.core_url,
            "DELETE FROM workspace_policies WHERE workspace_id = :'workspace';\n",
            variables={"workspace": self.workspace_a},
        )

    def test_telegram(
        self,
        target: AgentDriver,
        telegram_bridge: AgentDriver,
    ) -> None:
        log("Telegram plugin policy denial, native replies, DAG mapping, acknowledgement, and restart")
        bot_port = free_port()
        self.telegram = FakeTelegram(bot_port)
        telegram_config = self.root / "telegram.json"
        json_write(
            telegram_config,
            {
                "api_base_url": self.telegram.url,
                "allowed_user_ids": [7],
                "bindings": [
                    {
                        "chat_id": 42,
                        "message_thread_id": 7,
                        "target_agent_id": target.agent_id,
                        "wake_agent": False,
                    }
                ],
                "poll_timeout_seconds": 1,
                "receive_wait_milliseconds": 100,
                "batch_size": 20,
                "retry_initial_seconds": 0.05,
                "retry_max_seconds": 0.2,
                "ambiguous_retry_seconds": 0.1,
                "respond_to_denied": True,
            },
        )
        self.install_deny_policy(telegram_bridge.agent_id, target.agent_id)
        time.sleep(5.2)
        self.telegram.add_update(10, 100, "telegram root")
        telegram_bridge.start(
            "telegram",
            [str(TREER), "plugin", "run", "telegram", "--config", str(telegram_config)],
            env={"TELEGRAM_BOT_TOKEN": "999:test"},
        )
        time.sleep(1.0)
        denied = [
            item
            for item in self.receive(target)
            if item.get("message", {}).get("body") == "telegram root"
        ]
        require(not denied, "workspace Policy did not deny the brokered Telegram send")
        self.clear_policy()
        time.sleep(5.2)
        first = self.wait_delivery(target, "telegram root", timeout=20)
        message_1 = first["message"]
        require(message_1["external_source"]["channel"] == "telegram", "Telegram source annotation missing")
        self.ack(target, first, "e2e-ack-telegram-root")

        response = target.run_json(
            self.message_cli(
                "reply",
                str(message_1["message_id"]),
                "--to",
                telegram_bridge.agent_id,
                "--body",
                "telegram agent reply",
                "--idempotency-key",
                "e2e-telegram-reply",
            )
        )
        message_2 = response["message"]
        outbound = wait_for(
            lambda: self.telegram.sent_message("telegram agent reply") if self.telegram else None,
            "Telegram native reply",
        )
        payload = outbound["payload"]
        require(payload.get("message_thread_id") == 7, "Telegram topic was not preserved")
        require(
            payload.get("reply_parameters", {}).get("message_id") == 100,
            "Core context did not become a Telegram native reply",
        )
        telegram_message_2 = int(outbound["message_id"])
        self.telegram.add_update(11, 102, "telegram follow-up", reply_to=telegram_message_2)
        third = self.wait_delivery(target, "telegram follow-up")
        require(
            third["message"]["context_ids"] == [message_2["message_id"]],
            "Telegram reply did not reference the outbound Core Message",
        )
        self.ack(target, third, "e2e-ack-telegram-follow-up")

        telegram_bridge.stop("telegram")
        self.telegram.add_update(12, 103, "telegram after restart", reply_to=telegram_message_2)
        telegram_bridge.start(
            "telegram",
            [str(TREER), "plugin", "run", "telegram", "--config", str(telegram_config)],
            env={"TELEGRAM_BOT_TOKEN": "999:test"},
        )
        restarted = self.wait_delivery(target, "telegram after restart")
        require(
            restarted["message"]["context_ids"] == [message_2["message_id"]],
            "Telegram mapping did not survive plugin restart",
        )
        self.ack(target, restarted, "e2e-ack-telegram-restart")
        telegram_bridge.stop("telegram")

    def verify_safe_metadata(self) -> None:
        known_bodies = [
            "core root",
            "core reply",
            "mail to target",
            "telegram root",
            "telegram agent reply",
        ]
        payloads = psql(
            self.databases.core_url,
            "SELECT envelope::text FROM core_message_outbox ORDER BY created_at;\n",
            tuples=True,
        )
        for body in known_bodies:
            require(body not in payloads, f"Message body leaked into the outbox: {body}")
        if self.proxy is not None:
            proxy_log = self.proxy.log_path.read_text(encoding="utf-8", errors="replace")
            for body in known_bodies:
                require(body not in proxy_log, f"Message body leaked into Proxy logs: {body}")

    def run(self) -> None:
        for binary in (TREER, PROXY, CONTROLLER, HOST):
            require(binary.is_file(), f"missing {binary}; run `cargo build --workspace`")
        require(shutil.which("psql") is not None, "psql is required for the E2E harness")
        self.databases.create()
        self.start_proxy()
        self.setup_identity_and_workspaces()
        machine_a = MachineStack(
            self.root,
            "machine-a",
            self.proxy_url,
            self.workspace_a,
            self.enroll(self.workspace_a, "machine A"),
            self.shared_environment,
        )
        self.machines.append(machine_a)
        machine_b = MachineStack(
            self.root,
            "machine-b",
            self.proxy_url,
            self.workspace_b,
            self.enroll(self.workspace_b, "machine B"),
            self.shared_environment,
        )
        self.machines.append(machine_b)
        self.install_plugins()
        target = machine_a.create_driver(self.shared_environment, self.root / "agents/target", "target")
        sender = machine_a.create_driver(self.shared_environment, self.root / "agents/sender", "sender")
        mail_bridge = machine_a.create_driver(
            self.shared_environment, self.root / "agents/mail", "mail-bridge"
        )
        telegram_bridge = machine_a.create_driver(
            self.shared_environment, self.root / "agents/telegram", "telegram-bridge"
        )
        other = machine_b.create_driver(self.shared_environment, self.root / "agents/other", "other")

        self.test_core(machine_a, machine_b, target, sender, other)
        self.test_migrations(machine_a, machine_b)
        self.test_mail(machine_a, target, mail_bridge)
        self.test_telegram(target, telegram_bridge)
        self.verify_safe_metadata()
        log("all real-process messaging and plugin checks passed")

    def close(self) -> None:
        if self.telegram is not None:
            self.telegram.stop()
            self.telegram = None
        for machine in reversed(self.machines):
            machine.stop()
        self.machines.clear()
        if self.proxy is not None:
            self.proxy.stop()
            self.proxy = None
        self.databases.drop()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent-driver", type=Path, help=argparse.SUPPRESS)
    parser.add_argument(
        "--database-url",
        default=os.environ.get("TREER_TEST_DATABASE_URL", DEFAULT_DATABASE_URL),
        help="administrative PostgreSQL URL used to create isolated temporary databases",
    )
    parser.add_argument("--keep-temp", action="store_true", help="retain process logs and fixture state")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.agent_driver is not None:
        return driver_main(args.agent_driver)
    temporary = tempfile.TemporaryDirectory(prefix="treer-messaging-e2e-")
    root = Path(temporary.name)
    harness = Harness(root, args.database_url)
    try:
        harness.run()
        if args.keep_temp:
            retained = Path(tempfile.gettempdir()) / f"treer-messaging-e2e-retained-{uuid.uuid4().hex[:8]}"
            shutil.copytree(root, retained)
            log(f"retained E2E artifacts at {retained}")
        return 0
    except Exception as error:
        print(f"e2e: FAILED: {error}", file=sys.stderr)
        for process in ([harness.proxy] if harness.proxy is not None else []):
            print(f"\n--- {process.label} log ---\n{process.tail(12000)}", file=sys.stderr)
        for machine in harness.machines:
            print(
                f"\n--- {machine.process.label} log ---\n{machine.process.tail(12000)}",
                file=sys.stderr,
            )
        retained = Path(tempfile.gettempdir()) / f"treer-messaging-e2e-failed-{uuid.uuid4().hex[:8]}"
        try:
            shutil.copytree(root, retained)
            print(f"e2e: retained failure artifacts at {retained}", file=sys.stderr)
        except OSError:
            pass
        return 1
    finally:
        harness.close()
        temporary.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
