#!/usr/bin/env python3
"""Real PG + supervisor, fake Proxy. Disposable containers/volumes, no live Treer.

docker build --target runtime-base -f Dockerfile.single -t treer-single-runtime .
python3 -I deploy/single-container/tests/runtime-smoke.py treer-single-runtime
"""
import json
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time


image = sys.argv[1]
prefix = "treer-single-test-" + secrets.token_hex(5)
containers, volumes = [], []
root = Path(__file__).parents[1]


def docker(*args):
    result = subprocess.run(["docker", *args], text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise RuntimeError("docker " + args[0] + " failed (captured output intentionally not printed)")
    return (result.stdout + (result.stderr if args[0] == "logs" else "")).strip()


def wait(fn):
    for _ in range(150):
        if fn():
            return
        time.sleep(0.2)
    raise AssertionError("runtime readiness timed out")


try:
    with tempfile.TemporaryDirectory(prefix=prefix) as temporary:
        fixture = Path(temporary)
        (fixture / "build.json").write_text(json.dumps({"version": "0.1.14"}))
        (fixture / "proxy").write_text("#!/usr/bin/python3 -I\nimport time\nprint('FAKE_PROXY_READY',flush=True)\ntime.sleep(3600)\n")
        (fixture / "proxy").chmod(0o755)
        for index in range(2):
            name, volume = f"{prefix}-{index}", f"{prefix}-data-{index}"
            docker("volume", "create", volume)
            volumes.append(volume)
            docker("run", "-d", "--name", name, "--read-only", "--tmpfs", "/tmp:rw,nosuid,nodev",
                   "--cap-drop=ALL", "--cap-add=CHOWN", "--cap-add=DAC_OVERRIDE",
                   "--cap-add=SETUID", "--cap-add=SETGID", "--cap-add=KILL",
                   "--security-opt=no-new-privileges:true", "--mount", f"type=volume,src={volume},dst=/data",
                   "--mount", f"type=bind,src={fixture / 'proxy'},dst=/usr/local/bin/treer-proxy,readonly",
                   "--mount", f"type=bind,src={fixture / 'build.json'},dst=/app/build.json,readonly",
                   "--mount", f"type=bind,src={root / 'supervisor.py'},dst=/usr/local/lib/treer/supervisor.py,readonly",
                   "--entrypoint", "/usr/bin/tini", image, "--", "python3", "-I", "/usr/local/lib/treer/supervisor.py")
            containers.append(name)
            wait(lambda: "FAKE_PROXY_READY" in docker("logs", name))
            # Never retrieve credential bytes: compare hashes and assert permissions.
            checks = '''import os,pwd,stat,json,hashlib
from pathlib import Path
u=pwd.getpwnam('treer').pw_uid
for n in ('database-password','admin-password'):
 p=Path('/data/private')/n
 assert p.stat().st_uid==u and stat.S_IMODE(p.stat().st_mode)==0o600
print(json.dumps([hashlib.sha256((Path('/data/private')/n).read_bytes()).hexdigest() for n in ('database-password','admin-password')]))
'''
            hashes = json.loads(docker("exec", name, "python3", "-I", "-c", checks))
            if index == 0:
                first_hashes = hashes
                docker("stop", "-t", "65", name)
                logs = docker("logs", name)
                assert logs.index("stopping Proxy") < logs.index("stopping PostgreSQL")
                assert "database system is shut down" in logs
                docker("start", name)
                wait(lambda: docker("logs", name).count("FAKE_PROXY_READY") == 2)
                assert json.loads(docker("exec", name, "python3", "-I", "-c", checks)) == first_hashes
                role = docker("exec", "-u", "postgres", name, "psql", "-h", "/data/socket", "-U", "postgres", "-Atc",
                              "SELECT rolsuper,rolcreaterole,rolcreatedb,rolreplication FROM pg_roles WHERE rolname='treer'")
                assert role == "f|f|f|f"
                docker("exec", "-u", "postgres", name, "psql", "-h", "/data/socket", "-U", "postgres", "-d", "treer",
                       "-v", "ON_ERROR_STOP=1", "-c", "SET ROLE treer; CREATE TABLE smoke_proof(value text); INSERT INTO smoke_proof VALUES ('survives');")
                proc_check = '''import os,pwd
from pathlib import Path
uid=pwd.getpwnam('treer').pw_uid
for p in Path('/proc').iterdir():
 if p.name.isdigit() and p.stat().st_uid==uid:
  try: e=(p/'environ').read_bytes()
  except (PermissionError,FileNotFoundError): continue
  if b'DATABASE_URL=' in e:
   print(p.name);break
else: raise AssertionError('Proxy process missing')
'''
                proxy_pid = docker("exec", "-u", "treer", name, "python3", "-I", "-c", proc_check)
                docker("exec", "-u", "postgres", name, "python3", "-I", "-c",
                       f"from pathlib import Path\np=Path('/proc/{proxy_pid}/environ')\ntry: p.read_bytes()\nexcept PermissionError: pass\nelse: raise AssertionError('PG can read Proxy environment')")
                # Kill only this disposable PG postmaster, then require Proxy stop
                # before crash recovery and a new Proxy start. No process-name kill.
                docker("exec", name, "python3", "-I", "-c",
                       "import os,signal; from pathlib import Path; os.kill(int(Path('/data/pg/postmaster.pid').read_text().splitlines()[0]),signal.SIGKILL)")
                wait(lambda: docker("logs", name).count("FAKE_PROXY_READY") == 3)
                logs = docker("logs", name)
                restart = logs.rsplit("stopping Proxy", 1)[1]
                assert restart.index("starting PostgreSQL") < restart.index("starting Proxy")
                proof = docker("exec", "-u", "postgres", name, "psql", "-h", "/data/socket", "-U", "postgres", "-d", "treer", "-Atc", "SELECT value FROM smoke_proof")
                assert proof == "survives"
                docker("stop", "-t", "65", name)
                docker("run", "--rm", "--mount", f"type=volume,src={volume},dst=/data", "--entrypoint", "python3", image,
                       "-I", "-c", "from pathlib import Path; Path('/data/pg/PG_VERSION').write_text('18\\n')")
                docker("start", name)
                wait(lambda: docker("inspect", "-f", "{{.State.Running}}", name) == "false")
                assert "major mismatch" in docker("logs", name)
                assert docker("inspect", "-f", "{{.State.ExitCode}}", name) == "1"
                for change, expected in [
                    ("Path('/data/pg/PG_VERSION').write_text('17\\n'); Path('/data/private/initialized').write_text('storage-v999\\n')", "unsupported data storage"),
                    ("Path('/data/private/initialized').write_text('storage-v1-pg17\\n'); Path('/data/private/database-password').unlink()", "persisted database password missing"),
                ]:
                    docker("run", "--rm", "--mount", f"type=volume,src={volume},dst=/data", "--entrypoint", "python3", image,
                           "-I", "-c", "from pathlib import Path; " + change)
                    docker("start", name)
                    wait(lambda: docker("inspect", "-f", "{{.State.Running}}", name) == "false")
                    assert expected in docker("logs", name)
                    assert docker("logs", name).count("FAKE_PROXY_READY") == 3
            else:
                assert all(a != b for a, b in zip(first_hashes, hashes))
                logs = docker("logs", name)
                assert "postgres://" not in logs and "PASSWORD" not in logs
        print("PASS real PG init/restart/crash recovery/private credentials/ordered shutdown/major+storage+lost-secret refusal")
finally:
    for name in reversed(containers):
        docker("rm", "-f", "-v", name)
    for volume in volumes:
        docker("volume", "rm", volume)
