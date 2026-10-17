"""Run via the machine-wide gate: local one-click source build -> real health."""
import http.client
import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]


def main():
    if os.environ.get("TREER_GATE_HELD") != "1":
        raise SystemExit("Run this heavy smoke through just single-image-smoke (gate lock required)")
    # Resolve Docker transport before replacing HOME; never forward Agent,
    # workload, harness or cloud credentials to a subprocess/container.
    endpoint = subprocess.check_output(
        ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"], text=True).strip()
    if not endpoint.startswith("unix://"):
        raise SystemExit("This smoke requires a local Unix-socket Docker engine")
    with tempfile.TemporaryDirectory(prefix="t132-build-smoke-", dir="/tmp") as temporary:
        root = Path(temporary)
        (root / "home").mkdir(); (root / "tmp").mkdir()
        plugin = Path.home() / ".docker/cli-plugins/docker-buildx"
        if plugin.is_file():
            plugins = root / "home/.docker/cli-plugins"
            plugins.mkdir(parents=True)
            (plugins / "docker-buildx").symlink_to(plugin.resolve())
        env = {k: os.environ[k] for k in ("PATH", "LANG", "SHELL") if k in os.environ}
        env.update(HOME=str(root / "home"), TMPDIR=str(root / "tmp"), DOCKER_HOST=endpoint)
        result = subprocess.run(["sh", str(ROOT / "scripts/build-single-image.sh")],
                                cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE, check=True)
        print(result.stdout, flush=True)
        commit = subprocess.check_output(["git", "rev-parse", "--short=12", "HEAD"], cwd=ROOT, env=env, text=True).strip()
        image = "treer-single:" + commit
        name = "treer-build-smoke-" + secrets.token_hex(5)
        volume = name + "-data"

        def docker(*args):
            return subprocess.check_output(["docker", *args], env=env, text=True).strip()

        created = False
        docker("volume", "create", volume)
        try:
            docker("run", "-d", "--name", name, "--read-only", "--tmpfs", "/tmp:rw,nosuid,nodev",
                   "--cap-drop=ALL", "--cap-add=CHOWN", "--cap-add=DAC_OVERRIDE", "--cap-add=SETUID",
                   "--cap-add=SETGID", "--cap-add=KILL", "--security-opt=no-new-privileges:true",
                   "-p", "127.0.0.1::8080", "--mount", f"type=volume,src={volume},dst=/data", image)
            created = True
            port = int(docker("port", name, "8080/tcp").split(":")[-1])
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                connection = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                try:
                    connection.request("GET", "/api/health", headers={"Host": "proxy.treer.localhost:8080"})
                    response = connection.getresponse()
                    if response.status == 200:
                        json.loads(response.read())
                        print("PASS local build script -> real single-container health", flush=True)
                        return
                except (OSError, http.client.HTTPException):
                    pass
                finally:
                    connection.close()
                time.sleep(0.5)
            raise SystemExit("single-container build smoke: health timed out")
        finally:
            if created:
                docker("stop", "-t", "65", name)
                docker("rm", "-v", name)
            docker("volume", "rm", volume)


if __name__ == "__main__":
    main()
