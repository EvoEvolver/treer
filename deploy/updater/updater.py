#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, urlopen


CHANNEL_PATTERN = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
IMAGE_PATTERN = re.compile(r"^ghcr\.io/[a-z0-9._-]+/[a-z0-9._-]+$")
SERVICES = ("proxy", "app", "updater")
APPLY_SERVICES = ("proxy", "app")


class UpdaterError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message


def require_channel(value: str) -> str:
    if not CHANNEL_PATTERN.fullmatch(value):
        raise UpdaterError(400, "invalid_channel", "update channel is invalid")
    if value == "latest":
        raise UpdaterError(400, "invalid_channel", "the mutable latest tag is not a Treer channel")
    return value


def require_image(value: str) -> str:
    if not IMAGE_PATTERN.fullmatch(value):
        raise UpdaterError(400, "invalid_image", f"unsupported image name: {value}")
    return value


def first_repo_digest(inspect: dict[str, Any]) -> str | None:
    for item in inspect.get("RepoDigests") or []:
        if isinstance(item, str) and "@" in item:
            return item.split("@", 1)[1]
    return None


def add_digest(refs: set[str], value: object) -> None:
    if not isinstance(value, str) or not value:
        return
    refs.add(value)
    if not value.startswith("sha256:") and len(value) >= 64:
        refs.add(f"sha256:{value}")


def running_image_refs(
    inspect: dict[str, Any], image_inspect: dict[str, Any] | None = None
) -> set[str]:
    refs: set[str] = set()
    add_digest(refs, inspect.get("Image"))
    descriptor = inspect.get("ImageManifestDescriptor")
    if isinstance(descriptor, dict):
        add_digest(refs, descriptor.get("digest"))
    add_digest(refs, first_repo_digest(inspect))
    if image_inspect:
        add_digest(refs, image_inspect.get("Id"))
        add_digest(refs, first_repo_digest(image_inspect))
        for item in image_inspect.get("RepoDigests") or []:
            if isinstance(item, str) and "@" in item:
                add_digest(refs, item.split("@", 1)[1])
    return refs


def display_digest(inspect: dict[str, Any], image_inspect: dict[str, Any] | None = None) -> str | None:
    if image_inspect:
        digest = first_repo_digest(image_inspect)
        if digest:
            return digest
        image_id = image_inspect.get("Id")
        if isinstance(image_id, str) and image_id:
            return image_id
    descriptor = inspect.get("ImageManifestDescriptor")
    if isinstance(descriptor, dict) and isinstance(descriptor.get("digest"), str):
        return descriptor["digest"]
    image_id = inspect.get("Image")
    return image_id if isinstance(image_id, str) and image_id else None


def service_record(
    name: str,
    inspect: dict[str, Any] | None,
    image_inspect: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if inspect is None:
        return {"name": name, "present": False}
    config = inspect.get("Config") or {}
    labels = config.get("Labels") or {}
    return {
        "name": name,
        "present": True,
        "image": config.get("Image"),
        "digest": display_digest(inspect, image_inspect),
        "image_id": inspect.get("Image") if isinstance(inspect.get("Image"), str) else None,
        "version": labels.get("org.opencontainers.image.version"),
        "revision": labels.get("org.opencontainers.image.revision"),
        "refs": sorted(running_image_refs(inspect, image_inspect)),
    }


class Updater:
    def __init__(
        self,
        token: str,
        channel: str,
        images: dict[str, str],
        compose_file: str,
        compose_project: str,
        run_docker: Callable[[list[str]], str] | None = None,
        registry_digest: Callable[[str, str], str] | None = None,
    ) -> None:
        if not token:
            raise UpdaterError(500, "updater_misconfigured", "TREER_UPDATER_TOKEN is required")
        self.token = token
        self.channel = require_channel(channel)
        self.images = {name: require_image(image) for name, image in images.items()}
        self.compose_file = compose_file
        self.compose_project = compose_project
        self._run_docker = run_docker or self._docker
        self._registry_digest = registry_digest or ghcr_manifest_digest
        self._lock = threading.Lock()
        self._job: dict[str, Any] | None = None

    def status(self) -> dict[str, Any]:
        return {
            "channel": self.channel,
            "services": [self._inspect(name) for name in SERVICES],
            "job": self._job,
        }

    def check(self) -> dict[str, Any]:
        services = []
        update_available = False
        for name in SERVICES:
            record = self._inspect(name)
            image = self.images[name]
            channel_digests = self._channel_digests(image, self.channel)
            channel_digest = sorted(channel_digests)[0] if channel_digests else None
            record["channel_digest"] = channel_digest
            running_refs = set(record.get("refs") or [])
            record["update_available"] = bool(
                record.get("present")
                and channel_digests
                and running_refs
                and running_refs.isdisjoint(channel_digests)
            )
            update_available = update_available or bool(record["update_available"])
            services.append(record)
        return {
            "channel": self.channel,
            "services": services,
            "update_available": update_available,
            "job": self._job,
        }

    def apply(self) -> dict[str, Any]:
        report = self.check()
        if not report["update_available"]:
            raise UpdaterError(409, "already_current", "this channel is already running")
        with self._lock:
            if self._job and self._job.get("state") == "running":
                raise UpdaterError(409, "update_in_progress", "a control-plane update is already running")
            job_id = uuid.uuid4().hex
            self._job = {"id": job_id, "state": "running", "error": None}
        thread = threading.Thread(target=self._apply_job, args=(job_id,), daemon=True)
        thread.start()
        return {"job": self._job, "channel": self.channel}

    def _apply_job(self, job_id: str) -> None:
        try:
            for name in SERVICES:
                self._run_docker(["pull", f"{self.images[name]}:{self.channel}"])
            running_ids = {name: self._running_image_id(name) for name in SERVICES}
            pulled_ids = {
                name: self._local_image_id(f"{self.images[name]}:{self.channel}")
                for name in SERVICES
            }
            recreate = [
                name
                for name in APPLY_SERVICES
                if pulled_ids.get(name) and running_ids.get(name) != pulled_ids.get(name)
            ]
            if recreate:
                self._compose(["up", "-d", "--no-deps", "--pull", "never", *recreate])
            updater_changed = bool(
                pulled_ids.get("updater")
                and running_ids.get("updater") != pulled_ids.get("updater")
            )
            if updater_changed:
                self._recreate_updater_detached()
            with self._lock:
                if self._job and self._job.get("id") == job_id:
                    self._job = {"id": job_id, "state": "succeeded", "error": None}
        except Exception as error:  # noqa: BLE001
            with self._lock:
                if self._job and self._job.get("id") == job_id:
                    self._job = {"id": job_id, "state": "failed", "error": str(error)}

    def _inspect(self, name: str) -> dict[str, Any]:
        container_id = self._compose(["ps", "-q", "--status", "running", name]).strip()
        if not container_id:
            return service_record(name, None)
        inspect = self._inspect_json(container_id)
        if inspect is None:
            return service_record(name, None)
        image_id = inspect.get("Image") if isinstance(inspect.get("Image"), str) else None
        image_inspect = self._inspect_json(image_id) if image_id else None
        return service_record(name, inspect, image_inspect)

    def _inspect_json(self, target: str) -> dict[str, Any] | None:
        try:
            payload = json.loads(self._run_docker(["inspect", target]))
        except (UpdaterError, json.JSONDecodeError):
            return None
        inspect = payload[0] if isinstance(payload, list) and payload else None
        return inspect if isinstance(inspect, dict) else None

    def _running_image_id(self, name: str) -> str | None:
        record = self._inspect(name)
        image_id = record.get("image_id")
        return image_id if isinstance(image_id, str) and image_id else None

    def _local_image_id(self, ref: str) -> str | None:
        inspect = self._inspect_json(ref)
        image_id = inspect.get("Id") if inspect else None
        return image_id if isinstance(image_id, str) and image_id else None

    def _channel_digests(self, image: str, tag: str) -> set[str]:
        value = self._registry_digest(image, tag)
        if isinstance(value, str):
            return {value} if value else set()
        return {item for item in value if isinstance(item, str) and item}

    def _compose_host_file(self) -> str:
        hostname = os.environ.get("HOSTNAME", "").strip()
        if hostname:
            inspect = self._inspect_json(hostname)
            compose_dir = "/compose"
            if inspect:
                for mount in inspect.get("Mounts") or []:
                    if not isinstance(mount, dict):
                        continue
                    destination = mount.get("Destination")
                    source = mount.get("Source")
                    if destination == compose_dir and isinstance(source, str) and source:
                        return str(Path(source) / Path(self.compose_file).name)
                    if destination == self.compose_file and isinstance(source, str) and source:
                        return source
        return self.compose_file

    def _recreate_updater_detached(self) -> None:
        host_file = self._compose_host_file()
        host_dir = str(Path(host_file).parent)
        image = f"{self.images['updater']}:{self.channel}"
        try:
            self._run_docker(["rm", "-f", "treer-updater-recreate"])
        except UpdaterError:
            pass
        self._run_docker(
            [
                "run",
                "--rm",
                "-d",
                "--name",
                "treer-updater-recreate",
                "--entrypoint",
                "docker",
                "-v",
                "/var/run/docker.sock:/var/run/docker.sock",
                "-v",
                f"{host_dir}:{host_dir}:ro",
                image,
                "compose",
                "-f",
                host_file,
                "-p",
                self.compose_project,
                "up",
                "-d",
                "--no-deps",
                "--pull",
                "never",
                "updater",
            ]
        )

    def _compose(self, args: list[str]) -> str:
        return self._run_docker(
            ["compose", "-f", self.compose_file, "-p", self.compose_project, *args]
        )

    def _docker(self, args: list[str]) -> str:
        try:
            completed = subprocess.run(
                ["docker", *args],
                check=False,
                capture_output=True,
                text=True,
                timeout=600,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise UpdaterError(502, "docker_unavailable", f"docker command failed: {error}") from error
        if completed.returncode != 0:
            detail = (completed.stderr or completed.stdout).strip()[-2000:]
            raise UpdaterError(502, "docker_failed", detail or "docker command failed")
        return completed.stdout


def ghcr_manifest_digest(image: str, tag: str) -> set[str]:
    repository = image.removeprefix("ghcr.io/")
    token = _ghcr_token(repository)
    request = Request(f"https://ghcr.io/v2/{repository}/manifests/{tag}", method="GET")
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header(
        "Accept",
        "application/vnd.oci.image.index.v1+json, "
        "application/vnd.docker.distribution.manifest.list.v2+json, "
        "application/vnd.oci.image.manifest.v1+json, "
        "application/vnd.docker.distribution.manifest.v2+json",
    )
    try:
        with urlopen(request, timeout=30) as response:
            header_digest = response.headers.get("Docker-Content-Digest")
            payload = json.loads(response.read().decode())
    except HTTPError as error:
        raise UpdaterError(
            502,
            "registry_unavailable",
            f"GHCR returned HTTP {error.code} for {image}:{tag}",
        ) from error
    except (URLError, json.JSONDecodeError) as error:
        raise UpdaterError(502, "registry_unavailable", f"cannot reach GHCR: {error}") from error
    digests: set[str] = set()
    add_digest(digests, header_digest)
    if isinstance(payload, dict):
        add_digest(digests, payload.get("digest"))
        for manifest in payload.get("manifests") or []:
            if isinstance(manifest, dict):
                add_digest(digests, manifest.get("digest"))
    if not digests:
        raise UpdaterError(502, "registry_unavailable", f"GHCR omitted a digest for {image}:{tag}")
    return digests


def _ghcr_token(repository: str) -> str:
    configured = os.environ.get("TREER_GHCR_TOKEN", "").strip()
    if configured:
        return configured
    request = Request(
        f"https://ghcr.io/token?service=ghcr.io&scope=repository:{repository}:pull",
        method="GET",
    )
    try:
        with urlopen(request, timeout=15) as response:
            payload = json.loads(response.read().decode())
    except (HTTPError, URLError, json.JSONDecodeError) as error:
        raise UpdaterError(502, "registry_unavailable", "cannot obtain a GHCR pull token") from error
    token = payload.get("token")
    if not isinstance(token, str) or not token:
        raise UpdaterError(502, "registry_unavailable", "GHCR token response was empty")
    return token


def authorize(handler: BaseHTTPRequestHandler, token: str) -> None:
    header = handler.headers.get("Authorization", "")
    if header != f"Bearer {token}":
        raise UpdaterError(401, "unauthorized", "updater token is missing or invalid")


class UpdaterHandler(BaseHTTPRequestHandler):
    updater: Updater

    def do_GET(self) -> None:  # noqa: N802
        self._dispatch("GET")

    def do_POST(self) -> None:  # noqa: N802
        self._dispatch("POST")

    def log_message(self, format: str, *args: object) -> None:
        print(f"updater {self.address_string()} {format % args}", file=sys.stderr)

    def _dispatch(self, method: str) -> None:
        try:
            path = urlsplit(self.path).path.rstrip("/") or "/"
            if path == "/health":
                self._json(200, {"ok": True})
                return
            authorize(self, self.updater.token)
            if method == "GET" and path == "/v1/status":
                self._json(200, self.updater.status())
                return
            if method == "GET" and path == "/v1/check":
                self._json(200, self.updater.check())
                return
            if method == "POST" and path == "/v1/apply":
                self._json(202, self.updater.apply())
                return
            raise UpdaterError(404, "not_found", "route not found")
        except UpdaterError as error:
            self._json(error.status, {"error": {"code": error.code, "message": error.message}})
        except Exception as error:  # noqa: BLE001
            print(f"updater request failed: {error}", file=sys.stderr)
            self._json(500, {"error": {"code": "internal_error", "message": "internal server error"}})

    def _json(self, status: int, value: object) -> None:
        body = (json.dumps(value, sort_keys=True) + "\n").encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


def parse_listen(value: str) -> tuple[str, int]:
    host, separator, port_text = value.rpartition(":")
    if not separator or not host:
        raise ValueError("TREER_LISTEN must be HOST:PORT")
    return host, int(port_text)


def load_updater() -> Updater:
    owner = os.environ.get("TREER_GHCR_OWNER", "evoevolver").lower()
    return Updater(
        token=os.environ.get("TREER_UPDATER_TOKEN", ""),
        channel=os.environ.get("TREER_UPDATE_CHANNEL", os.environ.get("TREER_IMAGE_TAG", "stable")),
        images={
            "proxy": os.environ.get("TREER_PROXY_IMAGE", f"ghcr.io/{owner}/treer-proxy"),
            "app": os.environ.get("TREER_APP_IMAGE", f"ghcr.io/{owner}/treer-app"),
            "updater": os.environ.get("TREER_UPDATER_IMAGE", f"ghcr.io/{owner}/treer-updater"),
        },
        compose_file=os.environ.get("TREER_COMPOSE_FILE", "/compose/compose.yaml"),
        compose_project=os.environ.get("TREER_COMPOSE_PROJECT", "treer"),
    )


def main() -> None:
    listen = parse_listen(os.environ.get("TREER_LISTEN", "0.0.0.0:7420"))
    updater = load_updater()
    server = ThreadingHTTPServer(listen, UpdaterHandler)
    server.updater = updater  # type: ignore[attr-defined]
    UpdaterHandler.updater = updater
    print(f"Treer updater listening on http://{listen[0]}:{listen[1]}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
