#!/usr/bin/env python3
"""Stage and activate a Canary machine update; default preserves Host and Agents."""
import argparse
import base64
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import re
import shutil
import shlex
import socket
import stat
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import uuid

BINARIES = ("treer-agent-server", "treer", "treer-agent-host")
CANARY = "https://proxy.canary.treer.ai"


def clean_env(home, bin_dir=None):
    # Deliberately independent of os.environ: no workload identity, proxies or UI.
    path = f"{Path(home) / '.local/bin'}:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    if bin_dir:
        path = f"{bin_dir}:{path}"
    return {"HOME": str(home), "PATH": path, "USER": Path(home).name,
            "LANG": "C.UTF-8" if sys.platform != "darwin" else "en_US.UTF-8"}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_):
        raise RuntimeError("redirect refused; use the exact Canary artifact endpoint")


def validate_url(url):
    parsed = urllib.parse.urlsplit(url)
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise RuntimeError("URL must not contain credentials, queries or fragments")
    loopback = parsed.hostname == "localhost"
    try:
        loopback |= ipaddress.ip_address(parsed.hostname or "").is_loopback
    except ValueError:
        pass
    if parsed.scheme != "https" and not (parsed.scheme == "http" and loopback):
        raise RuntimeError("HTTPS required except loopback test servers")


def fetch(url, headers=None, limit=256 * 1024 * 1024):
    validate_url(url)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(urllib.request.Request(url, headers=headers or {}), timeout=20) as response:
        data = response.read(limit + 1)
        if len(data) > limit:
            raise RuntimeError("response exceeds size limit")
        return data


def manifest(data):
    rows = {}
    for line in data.decode("ascii").splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *](treer(?:-agent-(?:server|host))?)", line)
        if not match or match[2] in rows:
            raise RuntimeError("invalid or duplicate SHA256SUMS entry")
        rows[match[2]] = match[1].lower()
    if set(rows) != set(BINARIES):
        raise RuntimeError("SHA256SUMS must list exactly the three machine binaries")
    return rows


def digest(path):
    with Path(path).open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def write_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.chmod(0o600)
    os.replace(temporary, path)


def run(argv, home, timeout=20, bin_dir=None, config_dir=None):
    environment = clean_env(home, bin_dir)
    if config_dir:
        directory = Path(config_dir)
        if directory.parts[-2:] != ("treer", "agent-servers"):
            raise RuntimeError("config directory must end in treer/agent-servers")
        environment["XDG_CONFIG_HOME"] = str(directory.parent.parent)
    if bin_dir and sys.platform == "linux":
        # systemctl --user needs the target user's own bus even under env -i.
        # Derive it on the target; never forward the caller's bus/environment.
        runtime = Path(f"/run/user/{os.getuid()}")
        try:
            info = runtime.stat()
            if info.st_uid == os.getuid() and stat.S_ISDIR(info.st_mode) and not info.st_mode & 0o077:
                environment["XDG_RUNTIME_DIR"] = str(runtime)
                bus = runtime / "bus"
                if bus.is_socket():
                    environment["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={bus}"
        except FileNotFoundError:
            pass
    result = subprocess.run(argv, env=environment, stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        # Do not persist command output, which can contain machine configuration.
        raise RuntimeError(f"{Path(argv[0]).name} operation failed (exit {result.returncode})")
    return result.stdout.strip()


def version(path, home):
    value = run([str(path), "--version"], home)
    if len(value) > 512 or not re.search(r"\b[0-9]+\.[0-9]+\.[0-9]+\b", value):
        raise RuntimeError("binary did not return a bounded package version")
    return value


def source_commit(value):
    match = re.search(r"\b[0-9a-f]{7,40}\b", value)
    if not match:
        raise RuntimeError("binary version must include its source commit")
    return match[0]


SHARED_BINARIES_NOTICE = ("Shared binaries will be replaced: this affects other workspaces on this machine "
                          "on their next start. Only the selected workspace's Controller is activated; "
                          "coordinate other Controllers and version compatibility before continuing.")


def installed_configs(config_dir):
    configs = []
    for path in sorted(Path(config_dir).glob("*-controller.json")):
        config = json.loads(path.read_text())
        if config.get("install_hostname") != socket.gethostname():
            continue
        configs.append((path, config))
    return configs


def installed(config_dir, workspace=None, accept_shared_binaries=False, machine="local"):
    configs = installed_configs(config_dir)
    if len(configs) > 1 and not workspace:
        choices = []
        for _, config in configs:
            identifier = config.get("workspace", "")
            if not re.fullmatch(r"ws_[A-Za-z0-9_-]{1,96}", identifier):
                continue
            choices.append(f"python3 scripts/update-machine.py --machine {shlex.quote(machine)} "
                           f"--workspace {shlex.quote(identifier)} "
                           f"--config-dir {shlex.quote(str(config_dir))}")
        raise RuntimeError("multiple local installs; select --workspace explicitly. Candidates:\n" + "\n".join(choices))
    matches = [(path, config) for path, config in configs if not workspace or config["workspace"] == workspace]
    if len(matches) != 1:
        raise RuntimeError("workspace selection must match exactly one local install; no binaries were changed")
    if len(configs) > 1:
        print(SHARED_BINARIES_NOTICE, file=sys.stderr, flush=True)
        if accept_shared_binaries is not True:
            raise RuntimeError("multiple local installs require --accept-shared-binaries; no binaries were changed")
    path, config = matches[0]
    host, _, port = config["listen"].rpartition(":")
    host = host.strip("[]")
    if not ipaddress.ip_address(host).is_loopback or not port.isdigit():
        raise RuntimeError("Controller health must use a loopback listen address")
    return path, config


def local_document(config_path, route, operator=False):
    # Read an existing credential only in memory; never serialize or log it.
    config = json.loads(Path(config_path).read_text())
    headers = {"x-treer-operator-credential": config["operator_credential"]} if operator else {}
    value = json.loads(fetch(f"http://{config['listen']}{route}", headers, limit=4 * 1024 * 1024))
    if route == "/api/health" and (value.get("service") != "treer-agent-server" or
            value.get("workspace_id") != config["workspace"] or value.get("server_id") != config["server_id"]):
        raise RuntimeError("health identity does not match installed workspace/server")
    return value


def inventory(config_path):
    # Stable identity only: agent_id, pid (omitted or null means no process) and
    # run_generation. Other fields differ between Controller versions and never
    # take part in the comparison.
    rows = local_document(config_path, "/api/local/agents", operator=True)["agents"]
    return {row["agent_id"]: {"pid": row.get("pid"), "generation": row["run_generation"]}
            for row in rows}


MAX_RECEIPT_IDS = 32


class InventoryError(RuntimeError):
    """A preservation check failed; `code` is a fixed receipt token."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def wait_ready(config_path, timeout, before_epoch=None):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            value = local_document(config_path, "/api/health")
            epoch = value.get("controller_epoch")
            if value.get("proxy_connected") is True and epoch and epoch != before_epoch:
                return value
        except (OSError, ValueError, RuntimeError):
            pass
        time.sleep(1)
    raise RuntimeError("Controller did not reconnect with a fresh epoch before the deadline")


def wait_agents(config_path, before, timeout, cold=False):
    end = time.monotonic() + timeout
    while True:
        try:
            after = inventory(config_path)
            return check_agents(before, after, cold)
        except (OSError, ValueError, RuntimeError):
            if time.monotonic() >= end:
                raise
            time.sleep(1)


def replace_file(source, destination):
    destination = Path(destination)
    temporary = destination.with_name(destination.name + ".update-" + uuid.uuid4().hex)
    try:
        shutil.copyfile(source, temporary)
        temporary.chmod(0o755)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def check_agents(before, after, cold=False):
    """Preservation verdict for the Agents that were LIVE before the update.

    A Controller restart (new or old binary) only relists Agents that still have
    a process or a startup-store entry; exited records it held in memory are
    gone afterwards and are not a regression. Live Agents must be present with
    the same pid/generation (hot) or recreated (cold). Agents created during
    the window are reported, never fatal. The verdict goes into the receipt.
    """
    live = {key: row for key, row in before.items() if row["pid"] is not None}
    missing = sorted(key for key in live if key not in after)
    if missing:
        raise InventoryError("live_agent_missing",
                             f"Agent inventory changed during update: {len(missing)} live Agent(s) missing")
    for key, row in live.items():
        current = after[key]
        if not cold and current != row:
            raise InventoryError("agent_process_changed", "Agent PID/generation changed during hot update")
        if cold and (current["pid"] is None or current["pid"] == row["pid"]
                     or current["generation"] <= row["generation"]):
            raise InventoryError("cold_recovery_incomplete",
                                 "cold recovery did not recreate a previously live Agent")
    stopped = [key for key in before if key not in live]
    not_relisted = sorted(key for key in stopped if key not in after)
    appeared = sorted(key for key in after if key not in before)
    # Receipts stay bounded: counts are exact, id lists are capped.
    return {"live_preserved": len(live),
            "stopped_before": len(stopped),
            "stopped_not_relisted_count": len(not_relisted),
            "stopped_not_relisted": not_relisted[:MAX_RECEIPT_IDS],
            "appeared_count": len(appeared),
            "appeared": appeared[:MAX_RECEIPT_IDS]}


def transaction(job, config):
    home, bindir = Path(config["home"]), Path(config["bin_dir"])
    names = BINARIES if config["host"] else BINARIES[:2]
    config_path, service = installed(config["config_dir"], config.get("workspace"),
                                     config.get("accept_shared_binaries", False))
    health = wait_ready(config_path, config["timeout"])
    before = inventory(config_path)
    endpoint = f"{config['proxy'].rstrip('/')}/artifacts/{config['platform']}/"
    sums = fetch(endpoint + "SHA256SUMS", limit=8192)
    published = manifest(sums)
    if config.get("expected_sums") and hashlib.sha256(sums).hexdigest() != config["expected_sums"]:
        raise RuntimeError("SHA256SUMS differs from the canary_passed release receipt")
    if config.get("expected_files") and published != config["expected_files"]:
        raise RuntimeError("artifact checksums differ from the canary_passed release receipt")
    stage = job / "stage"
    stage.mkdir(exist_ok=True)
    if config.get("artifacts"):
        source = Path(config["artifacts"])
        if (source / config["platform"]).is_dir():
            source /= config["platform"]
        if manifest((source / "SHA256SUMS").read_bytes()) != published:
            raise RuntimeError("local SHA256SUMS differs from Canary")
    else:
        source = None
    staged_versions = {}
    for name in names:
        destination = stage / name
        if source:
            shutil.copyfile(source / name, destination)
        else:
            destination.write_bytes(fetch(endpoint + name))
        if digest(destination) != published[name]:
            raise RuntimeError("artifact SHA256 mismatch: " + name)
        destination.chmod(0o755)
        staged_versions[name] = version(destination, home)
    candidate = source_commit(staged_versions[names[0]])
    if config.get("expected_commit") and not config["expected_commit"].startswith(candidate):
        raise RuntimeError("staged source commit differs from the pinned release")
    if any(source_commit(v) != candidate for v in staged_versions.values()):
        raise RuntimeError("staged binaries have different source commits")
    # Prevent a deployment race from mixing two published sets.
    if manifest(fetch(endpoint + "SHA256SUMS", limit=8192)) != published:
        raise RuntimeError("Canary changed while staging; retry from the new manifest")
    destinations = {name: bindir / name for name in names}
    if config.get("cli_path"):
        destinations["treer"] = Path(config["cli_path"])
    old_versions = {name: version(destinations[name], home) for name in names}
    backup = home / ".local/share/treer-backups" / (
        source_commit(old_versions[names[0]]) + "-" + time.strftime("%Y%m%dT%H%M%S") + "-" + job.name[-8:])
    backup.mkdir(parents=True, mode=0o700)
    for name in names:
        shutil.copy2(destinations[name], backup / name)
    if not config["host"] and (bindir / "treer-agent-host").is_file():
        shutil.copy2(bindir / "treer-agent-host", backup / "treer-agent-host")
    if config.get("startup_store"):
        startup = Path(config["startup_store"])
        if startup.is_symlink() or not startup.is_file():
            raise RuntimeError("startup store must be an existing regular file")
        shutil.copy2(startup, backup / "agent-startup.json")
        (backup / "agent-startup.json").chmod(0o600)
    write_json(backup / "receipt.json", {"workspace": service["workspace"], "versions": old_versions,
                                       "agents": before, "host_changed": config["host"]})
    changed = []
    activation_attempted = False
    controller = str(bindir / "treer-agent-server")
    action = "restart" if config["host"] else "restart-controller"
    command = [controller, "service", "--workspace", service["workspace"], action]
    try:
        replace_file(stage / "treer-agent-server", controller)
        changed.append("treer-agent-server")
        if config["host"]:
            replace_file(stage / "treer-agent-host", bindir / "treer-agent-host")
            changed.append("treer-agent-host")
        activation_attempted = True
        run(command, home, timeout=config["timeout"], bin_dir=bindir, config_dir=config["config_dir"])
        ready = wait_ready(config_path, config["timeout"], health["controller_epoch"])
        verdict = wait_agents(config_path, before, config["timeout"], config["host"])
        reported = ready.get("controller_build", {}).get("git_commit", "")
        if not reported or not (reported.startswith(candidate) or candidate.startswith(reported)):
            raise RuntimeError("running Controller build differs from staged candidate")
        if config["host"]:
            host_commit = ready.get("host_build", {}).get("git_commit", "")
            if not host_commit or not (host_commit.startswith(candidate) or candidate.startswith(host_commit)):
                raise RuntimeError("running Host build differs from staged candidate")
        # A new path-aware CLI must never precede its capable Controller.
        if "agent.path-targets.v1" not in ready.get("cli_capabilities", []):
            raise RuntimeError("new Controller does not advertise path-target capability")
        replace_file(stage / "treer", destinations["treer"])
        changed.append("treer")
        verdict = wait_agents(config_path, before, config["timeout"], config["host"])
        for name in names:
            if digest(destinations[name]) != published[name] or version(destinations[name], home) != staged_versions[name]:
                raise RuntimeError("installed binary verification failed")
    except Exception as error:
        # Restore, reactivate, then verify. Each stage reports its own reason:
        # a verified rollback is `rolled_back` even when exited records were not
        # relisted; `rollback_failed` always carries a fixed code and message.
        rollback_error = None
        verification = None
        try:
            for name in reversed(changed):
                replace_file(backup / name, destinations[name])
            if activation_attempted:
                epoch = None
                try:
                    epoch = local_document(config_path, "/api/health").get("controller_epoch")
                except (OSError, ValueError, RuntimeError):
                    pass
                run(command, home, timeout=config["timeout"], bin_dir=bindir, config_dir=config["config_dir"])
                wait_ready(config_path, config["timeout"], epoch)
            if not config["host"]:
                verification = check_agents(before, inventory(config_path))
        except Exception as rollback:
            rollback_error = {"code": getattr(rollback, "code", None) or type(rollback).__name__,
                              "message": str(rollback)[:200]}
        write_json(job / "result.json", {"status": "rollback_failed" if rollback_error else "rolled_back",
                    "error": str(error), "error_code": getattr(error, "code", None),
                    "rollback_error": rollback_error, "verification": verification, "backup": str(backup),
                    "cold_restart_cannot_restore_original_processes": config["host"]})
        raise
    return {"status": "complete", "commit": candidate, "backup": str(backup),
            "versions": staged_versions, "agents": inventory(config_path), "inventory": verdict,
            "host_changed": config["host"]}


def worker(job):
    job = Path(job)
    config = json.loads((job / "job.json").read_text())
    lock = Path(config["bin_dir"]) / ".treer-machine-update.lock"
    try:
        lock.mkdir(mode=0o700)
    except FileExistsError:
        write_json(job / "result.json", {"status": "failed", "error": "another update holds the binary lock"})
        return 1
    try:
        write_json(job / "progress.json", {"status": "running", "pid": os.getpid()})
        result = transaction(job, config)
        write_json(job / "result.json", result)
        return 0
    except Exception as error:
        if not (job / "result.json").exists():
            write_json(job / "result.json", {"status": "failed", "error": str(error)})
        return 1
    finally:
        lock.rmdir()


class Machine:
    def __init__(self, name, treer="treer"):
        self.name, self.treer = name, treer

    def python(self, code):
        argv = ["python3", "-c", code]
        if self.name not in ("local", "本机"):
            argv = [self.treer, "exec", "--machine", self.name, "--cwd", ".",
                    "--timeout", "25000", "--", *argv]
        result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        if result.returncode:
            raise RuntimeError("machine exec failed; no remote output was persisted")
        value = result.stdout
        if self.name not in ("local", "本机"):
            envelope = json.loads(value)
            if envelope.get("truncated") or envelope.get("timed_out") or envelope.get("exit_code") != 0:
                raise RuntimeError("remote exec failed, timed out or exceeded 64 KiB")
            value = envelope["stdout"]
        return value.strip()

    def upload(self, source, destination):
        # Bounded argv, no stdin, no credentials; each exec performs only one small write.
        self.python(f"from pathlib import Path; p=Path({str(destination)!r}); p.write_bytes(b''); p.chmod(0o600)")
        with Path(source).open("rb") as stream:
            while data := stream.read(24 * 1024):
                encoded = base64.b64encode(data).decode()
                self.python(f"import base64; open({str(destination)!r},'ab').write(base64.b64decode({encoded!r}))")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--machine", required=True, help="local / 本机, or a stable machine ID/name such as mariana")
    parser.add_argument("--workspace", help="installed workspace; inferred only for a unique local install")
    parser.add_argument("--accept-shared-binaries", action="store_true",
                        help="confirm replacing binaries also used by other installed workspaces on their next start")
    parser.add_argument("--artifacts", help="local dist directory (platform subdirectories also accepted)")
    parser.add_argument("--proxy", default=CANARY, help="Canary checksum authority (HTTPS; loopback HTTP for tests)")
    parser.add_argument("--bin-dir", help="target installed binary directory; default resolved treer-agent-server parent")
    parser.add_argument("--config-dir", help="target Controller config directory")
    parser.add_argument("--host", action="store_true", help="WARNING: full Host restart terminates all its Agents and PTYs")
    parser.add_argument("--acknowledge-agent-interruption", action="store_true", help="leader has approved the stated Host interruption")
    parser.add_argument("--apply", action="store_true", help="activate the update; otherwise print a read-only plan")
    parser.add_argument("--timeout", type=int, default=180, help="per-step readiness timeout, seconds")
    parser.add_argument("--resume", metavar="JOB", help="poll an existing target job after losing the caller connection")
    args = parser.parse_args()
    if args.host and not args.acknowledge_agent_interruption:
        parser.error("--host terminates all Agents/PTYS owned by this Host; obtain approval and supply --acknowledge-agent-interruption")
    if not 1 <= args.timeout <= 1800:
        parser.error("--timeout must be between 1 and 1800 seconds")
    if not args.apply and not args.resume:
        print(json.dumps({"machine": args.machine, "host": args.host,
            "order": ["verify Canary checksums", "stage --version", "backup", "Controller", "proxy_connected",
                      "Agent PID/generation", "CLI", "verify / rollback"], "changes": False}, indent=2))
        return 0
    validate_url(args.proxy)
    machine = Machine(args.machine)
    if args.resume:
        job = args.resume
    else:
        probe = json.loads(machine.python("import json,os,platform,shutil; from pathlib import Path; "
            "print(json.dumps({'home':str(Path.home()),'system':platform.system(),'arch':platform.machine(),"
            "'controller':str(Path(shutil.which('treer-agent-server')).resolve()) if shutil.which('treer-agent-server') else None,'config_base':os.environ.get('XDG_CONFIG_HOME',str(Path.home()/'.config'))}))"))
        system = {"Darwin": "darwin", "Linux": "linux"}.get(probe["system"])
        arch = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(probe["arch"])
        if not system or not arch or not probe["controller"]:
            raise RuntimeError("require a Unix machine with python3 and installed treer-agent-server")
        home = probe["home"]
        bindir = args.bin_dir or str(Path(probe["controller"]).parent)
        job = str(Path(home) / ".local/share/treer-updates" / ("job-" + uuid.uuid4().hex))
        machine.python(f"from pathlib import Path; Path({job!r}).mkdir(parents=True,mode=0o700)")
        machine.upload(Path(__file__).resolve(), Path(job) / "worker.py")
        configdir = args.config_dir or str(Path(probe["config_base"]) / "treer/agent-servers")
        # Preflight on the target before staging/activation. The worker repeats
        # this under the binary lock in case installation topology has changed.
        selection = json.loads(machine.python(
            f"import runpy,json; m=runpy.run_path({str(Path(job) / 'worker.py')!r})\n"
            f"shared=len(m['installed_configs']({configdir!r}))>1\n"
            f"try:\n p,c=m['installed']({configdir!r},{args.workspace!r},{args.accept_shared_binaries!r},{args.machine!r}); "
            "result={'workspace':c['workspace'],'shared':shared}\n"
            "except RuntimeError as error:\n result={'error':str(error),'shared':shared}\n"
            "print(json.dumps(result))"))
        if selection["shared"] and args.workspace:
            print(SHARED_BINARIES_NOTICE, file=sys.stderr, flush=True)
        if selection.get("error"):
            raise RuntimeError(selection["error"])
        artifacts = None
        if args.artifacts:
            source = Path(args.artifacts).resolve()
            platform_name = system + "-" + arch
            if (source / platform_name).is_dir():
                source /= platform_name
            artifacts = str(Path(job) / "uploaded")
            machine.python(f"from pathlib import Path; Path({artifacts!r}).mkdir(mode=0o700)")
            for name in (*BINARIES, "SHA256SUMS"):
                if name == "treer-agent-host" and not args.host:
                    continue
                machine.upload(source / name, Path(artifacts) / name)
        config = {"home": home, "bin_dir": bindir, "config_dir": configdir,
                  "workspace": selection["workspace"], "accept_shared_binaries": args.accept_shared_binaries,
                  "host": args.host, "proxy": args.proxy, "platform": system + "-" + arch,
                  "artifacts": artifacts, "timeout": args.timeout}
        machine.python("import json; from pathlib import Path; "
                       f"p=Path({job!r})/'job.json'; p.write_text({json.dumps(config)!r}); p.chmod(0o600)")
        print(f"Impact: {'full Host restart; all owned Agents/PTYS terminate' if args.host else 'Controller reconnect only; preserve Host and Agent processes'}", flush=True)
        print(f"Job: {job}\nResume: {Path(__file__).name} --machine {args.machine} --resume {job}", flush=True)
        # start_new_session calls setsid on Unix. nohup also ignores SIGHUP.
        # env -i prevents the inherited treer exec workload environment reaching the worker.
        environment = clean_env(home, bindir)
        command = ["nohup", "env", "-i", *[f"{k}={v}" for k, v in environment.items()],
                   "python3", str(Path(job) / "worker.py"), "--worker", job]
        machine.python(f"import subprocess; subprocess.Popen({command!r},stdin=subprocess.DEVNULL,"
                       "stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True,close_fds=True)")
    deadline = time.monotonic() + args.timeout * 4 + 120
    while time.monotonic() < deadline:
        try:
            output = machine.python(f"from pathlib import Path; p=Path({job!r})/'result.json'; print(p.read_text() if p.exists() else '{{}}')")
            result = json.loads(output)
            if result:
                print(json.dumps(result, indent=2))
                return 0 if result["status"] == "complete" else 1
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
            pass  # Controller may be reconnecting; the detached worker owns rollback.
        time.sleep(2)
    raise RuntimeError("poll timed out; worker continues independently; use --resume with the printed job path")


if __name__ == "__main__":
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--worker":
            raise SystemExit(worker(sys.argv[2]))
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Machine update failed: {error}", file=sys.stderr)
        raise SystemExit(1)
