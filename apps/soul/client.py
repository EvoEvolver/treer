#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tarfile
import tempfile
import time
import uuid
from pathlib import Path, PurePosixPath
from urllib.error import HTTPError
from urllib.request import Request, urlopen


VERSION = "0.1.0"
SESSION_ID_PATTERN = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
)
ENV_NAME_PATTERN = re.compile(r"[A-Za-z_][A-Za-z0-9_]{0,127}")
MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024
MAX_EXPANDED_BYTES = 128 * 1024 * 1024
PROTECTED_ENVIRONMENT = {
    "BASH_ENV",
    "CODEX_HOME",
    "ENV",
    "HOME",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NODE_OPTIONS",
    "PATH",
    "PYTHONPATH",
    "SHELL",
    "TMPDIR",
    "TREER_AGENT_ID",
    "TREER_AGENT_SERVER_URL",
    "TREER_BIN",
    "TREER_SERVER_ID",
    "TREER_SOUL_ID",
    "TREER_SOUL_ROOT",
    "TREER_WORKLOAD_CREDENTIAL",
    "TREER_WORKSPACE_ID",
}


def request(server: str, method: str, path: str, body: bytes | None = None, content_type: str | None = None) -> bytes:
    headers = {"Accept": "application/json"}
    if content_type:
        headers["Content-Type"] = content_type
    target = f"{server.rstrip('/')}{path}"
    try:
        with urlopen(Request(target, data=body, headers=headers, method=method), timeout=60) as response:
            length = response.headers.get("Content-Length")
            if length and int(length) > MAX_DOWNLOAD_BYTES:
                raise RuntimeError("response exceeds client download limit")
            result = response.read(MAX_DOWNLOAD_BYTES + 1)
            if len(result) > MAX_DOWNLOAD_BYTES:
                raise RuntimeError("response exceeds client download limit")
            return result
    except HTTPError as error:
        detail = error.read().decode("utf-8", "replace")
        raise RuntimeError(f"Soul server returned HTTP {error.code}: {detail}") from error


def print_json(value: object) -> None:
    print(json.dumps(value, indent=2, sort_keys=True))


def validate_archive_path(value: str) -> str:
    if not value or "\\" in value or "\x00" in value:
        raise RuntimeError(f"invalid archive path: {value}")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise RuntimeError(f"unsafe archive path: {value}")
    return str(path)


def upload_archive(server: str, archive: Path) -> dict[str, object]:
    return json.loads(request(server, "POST", "/v1/souls", archive.read_bytes(), "application/x-tar"))


def command_upload(args: argparse.Namespace) -> None:
    manifest_path = Path(args.manifest).resolve()
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    mappings: list[tuple[str, Path]] = []
    for mapping in args.file:
        archive_path, separator, local_path = mapping.partition("=")
        if not separator:
            raise RuntimeError("--file must be ARCHIVE_PATH=LOCAL_PATH")
        mappings.append((validate_archive_path(archive_path), Path(local_path).resolve()))
    with tempfile.NamedTemporaryFile(suffix=".tar") as temporary:
        with tarfile.open(fileobj=temporary, mode="w") as archive:
            encoded = json.dumps(manifest, sort_keys=True).encode()
            info = tarfile.TarInfo("manifest.json")
            info.size = len(encoded)
            archive.addfile(info, __import__("io").BytesIO(encoded))
            for archive_path, local_path in mappings:
                if not local_path.is_file():
                    raise RuntimeError(f"upload file does not exist: {local_path}")
                archive.add(local_path, arcname=archive_path, recursive=False)
        temporary.flush()
        print_json(upload_archive(args.server, Path(temporary.name)))


def codex_home() -> Path:
    return Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")).expanduser().resolve()


def session_metadata(path: Path) -> dict[str, object]:
    with path.open("r", encoding="utf-8") as stream:
        first = json.loads(stream.readline())
    if first.get("type") != "session_meta" or not isinstance(first.get("payload"), dict):
        raise RuntimeError(f"Codex rollout has no session_meta header: {path}")
    return first["payload"]


def current_codex_ui_session() -> str | None:
    port = os.environ.get("CODEX_UI_PORT", "4173")
    try:
        with urlopen(f"http://127.0.0.1:{port}/api/state", timeout=1) as response:
            value = json.load(response)
        session_id = value.get("thread", {}).get("id")
        return session_id if isinstance(session_id, str) and SESSION_ID_PATTERN.fullmatch(session_id) else None
    except Exception:  # noqa: BLE001
        return None


def find_codex_session(requested: str | None) -> tuple[Path, dict[str, object]]:
    sessions = codex_home() / "sessions"
    session_id = requested or current_codex_ui_session()
    candidates = list(sessions.rglob("*.jsonl")) if sessions.exists() else []
    if session_id:
        matches = [path for path in candidates if session_id in path.name]
        if len(matches) != 1:
            raise RuntimeError(f"expected one Codex rollout for session {session_id}, found {len(matches)}")
        return matches[0], session_metadata(matches[0])
    cwd = str(Path.cwd().resolve())
    matching: list[tuple[float, Path, dict[str, object]]] = []
    for path in candidates:
        try:
            metadata = session_metadata(path)
        except (OSError, json.JSONDecodeError, RuntimeError):
            continue
        if metadata.get("cwd") == cwd:
            matching.append((path.stat().st_mtime, path, metadata))
    if not matching:
        raise RuntimeError("no Codex session found for the current working directory; pass --session")
    _, path, metadata = max(matching, key=lambda item: item[0])
    return path, metadata


def add_bytes(archive: tarfile.TarFile, name: str, value: bytes) -> None:
    import io

    info = tarfile.TarInfo(name)
    info.size = len(value)
    archive.addfile(info, io.BytesIO(value))


def stable_snapshot(source: Path, destination: Path) -> None:
    for _attempt in range(5):
        before = source.stat()
        shutil.copyfile(source, destination)
        after = source.stat()
        if before.st_size == after.st_size and before.st_mtime_ns == after.st_mtime_ns:
            if destination.stat().st_size:
                with destination.open("rb") as snapshot:
                    snapshot.seek(-1, os.SEEK_END)
                    if snapshot.read(1) != b"\n":
                        raise RuntimeError(f"Codex session ends with a partial JSONL record: {source}")
            return
        time.sleep(0.05)
    raise RuntimeError(f"Codex session changed continuously while being captured: {source}")


def command_capture_codex(args: argparse.Namespace) -> None:
    rollout, metadata = find_codex_session(args.session)
    session_id = str(metadata.get("id", ""))
    if not SESSION_ID_PATTERN.fullmatch(session_id):
        raise RuntimeError("Codex session metadata contains an invalid id")
    sessions_root = codex_home() / "sessions"
    relative = rollout.relative_to(sessions_root).as_posix()
    environment = {"CODEX_SOUL_SESSION_FILE": "files/session.jsonl"}
    adapter: dict[str, object] = {
        "name": "codex",
        "session_id": session_id,
        "session_file": "files/session.jsonl",
        "session_relative_path": relative,
        "cli_version": str(metadata.get("cli_version", "unknown")),
        "cwd": str(metadata.get("cwd", "")),
    }
    shell_snapshot = None
    if args.include_shell_snapshot:
        shell_candidates = list((codex_home() / "shell_snapshots").glob(f"{session_id}.*.sh"))
        if shell_candidates:
            shell_snapshot = max(shell_candidates, key=lambda path: path.stat().st_mtime)
            environment["CODEX_SOUL_SHELL_SNAPSHOT"] = "files/shell_snapshot.sh"
            adapter["shell_snapshot_file"] = "files/shell_snapshot.sh"
            adapter["shell_snapshot_name"] = shell_snapshot.name
    manifest = {
        "schema_version": 1,
        "name": args.name or f"Codex {session_id[:8]}",
        "environment": environment,
        "adapter": adapter,
    }
    with tempfile.TemporaryDirectory(prefix="treer-soul-codex-") as temporary_name:
        temporary_root = Path(temporary_name)
        rollout_snapshot = temporary_root / "session.jsonl"
        stable_snapshot(rollout, rollout_snapshot)
        shell_snapshot_copy = temporary_root / "shell_snapshot.sh"
        if shell_snapshot:
            shutil.copyfile(shell_snapshot, shell_snapshot_copy)
        archive_path = temporary_root / "soul.tar"
        with tarfile.open(archive_path, mode="w") as archive:
            add_bytes(archive, "manifest.json", json.dumps(manifest, sort_keys=True).encode())
            archive.add(rollout_snapshot, arcname="files/session.jsonl", recursive=False)
            if shell_snapshot:
                archive.add(shell_snapshot_copy, arcname="files/shell_snapshot.sh", recursive=False)
        print_json(upload_archive(args.server, archive_path))


def safe_extract(archive_path: Path, destination: Path) -> dict[str, object]:
    destination.mkdir(parents=True, exist_ok=False, mode=0o700)
    manifest: dict[str, object] | None = None
    expanded_size = 0
    with tarfile.open(archive_path, "r:*") as archive:
        for member in archive.getmembers():
            name = validate_archive_path(member.name)
            target = destination.joinpath(*PurePosixPath(name).parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True, mode=0o700)
                continue
            if not member.isfile():
                raise RuntimeError(f"archive entry is not a regular file: {name}")
            expanded_size += member.size
            if expanded_size > MAX_EXPANDED_BYTES:
                raise RuntimeError("expanded soul archive exceeds the client limit")
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            source = archive.extractfile(member)
            if source is None:
                raise RuntimeError(f"cannot read archive entry: {name}")
            with target.open("xb") as output:
                shutil.copyfileobj(source, output)
            target.chmod(0o600)
            if name == "manifest.json":
                manifest = json.loads(target.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict):
        raise RuntimeError("archive has no manifest.json")
    return manifest


def same_file(left: Path, right: Path) -> bool:
    def digest(path: Path) -> bytes:
        value = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                value.update(chunk)
        return value.digest()

    return left.stat().st_size == right.stat().st_size and digest(left) == digest(right)


def install_codex_session(root: Path, adapter: dict[str, object]) -> str:
    session_id = str(adapter.get("session_id", ""))
    if not SESSION_ID_PATTERN.fullmatch(session_id):
        raise RuntimeError("Codex soul has an invalid session id")
    source = root / validate_archive_path(str(adapter.get("session_file", "")))
    relative = validate_archive_path(str(adapter.get("session_relative_path", "")))
    target = codex_home() / "sessions" / Path(relative)
    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if target.exists() and not same_file(source, target):
        raise RuntimeError(f"target Codex session already exists with different content: {target}")
    if not target.exists():
        shutil.copy2(source, target)
        target.chmod(0o600)
    shell_file = adapter.get("shell_snapshot_file")
    shell_name = adapter.get("shell_snapshot_name")
    if isinstance(shell_file, str) and isinstance(shell_name, str) and Path(shell_name).name == shell_name:
        shell_source = root / validate_archive_path(shell_file)
        shell_target = codex_home() / "shell_snapshots" / shell_name
        shell_target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if not shell_target.exists():
            shutil.copy2(shell_source, shell_target)
            shell_target.chmod(0o600)
    return session_id


def command_run(args: argparse.Namespace) -> None:
    state_root = Path(
        os.environ.get("TREER_SOUL_STATE_DIR", Path.home() / ".local/state/treer-soul")
    ).expanduser().resolve()
    state_root.mkdir(parents=True, exist_ok=True, mode=0o700)
    incarnation_root = state_root / "incarnations" / uuid.uuid4().hex
    with tempfile.NamedTemporaryFile(suffix=".tar") as archive:
        archive.write(request(args.server, "GET", f"/v1/souls/{args.soul_id}/archive"))
        archive.flush()
        manifest = safe_extract(Path(archive.name), incarnation_root)
    environment = os.environ.copy()
    bindings = manifest.get("environment", {})
    if not isinstance(bindings, dict):
        raise RuntimeError("soul environment is invalid")
    for name, path in bindings.items():
        if not isinstance(name, str) or not ENV_NAME_PATTERN.fullmatch(name) or not isinstance(path, str):
            raise RuntimeError("soul environment binding is invalid")
        if name in PROTECTED_ENVIRONMENT or name.startswith("TREER_"):
            raise RuntimeError(f"soul attempts to replace protected environment variable: {name}")
        target = incarnation_root / validate_archive_path(path)
        if not target.is_file():
            raise RuntimeError(f"soul environment file is missing: {path}")
        environment[name] = str(target)
    environment["TREER_SOUL_ID"] = args.soul_id
    environment["TREER_SOUL_ROOT"] = str(incarnation_root)
    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    adapter = manifest.get("adapter")
    if not command and isinstance(adapter, dict) and adapter.get("name") == "codex":
        session_id = install_codex_session(incarnation_root, adapter)
        command = [
            environment.get("CODEX_BIN", "codex"),
            "resume",
            session_id,
            "--dangerously-bypass-approvals-and-sandbox",
            "-C",
            str(Path.cwd()),
        ]
    if not command:
        raise RuntimeError("generic soul requires a command after --")
    os.execvpe(command[0], command, environment)


def command_list(args: argparse.Namespace) -> None:
    print_json(json.loads(request(args.server, "GET", "/v1/souls")))


def command_show(args: argparse.Namespace) -> None:
    print_json(json.loads(request(args.server, "GET", f"/v1/souls/{args.soul_id}")))


def command_incarnate(args: argparse.Namespace) -> None:
    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    body = json.dumps(
        {"machine": args.machine, "name": args.name, "cwd": args.cwd, "command": command}
    ).encode()
    print_json(
        json.loads(
            request(
                args.server,
                "POST",
                f"/v1/souls/{args.soul_id}/incarnations",
                body,
                "application/json",
            )
        )
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description="Upload and incarnate Agent soul bundles")
    result.add_argument("--version", action="version", version=f"treer-soul {VERSION}")
    result.add_argument("--server", default=os.environ.get("TREER_SOUL_URL", "http://soul.internal"))
    commands = result.add_subparsers(dest="subcommand", required=True)

    upload = commands.add_parser("upload", help="upload a manifest and named files")
    upload.add_argument("--manifest", required=True)
    upload.add_argument("--file", action="append", default=[], metavar="ARCHIVE_PATH=LOCAL_PATH")
    upload.set_defaults(function=command_upload)

    capture = commands.add_parser("capture-codex", help="upload a Codex rollout as a soul")
    capture.add_argument("--session", help="Codex session UUID; defaults to the current Codex UI thread")
    capture.add_argument("--name")
    capture.add_argument(
        "--include-shell-snapshot",
        action="store_true",
        help="include Codex's shell snapshot, which may contain sensitive environment data",
    )
    capture.set_defaults(function=command_capture_codex)

    listing = commands.add_parser("list", help="list uploaded souls")
    listing.set_defaults(function=command_list)

    show = commands.add_parser("show", help="show soul metadata")
    show.add_argument("soul_id")
    show.set_defaults(function=command_show)

    incarnate = commands.add_parser("incarnate", help="create a Treer Agent from a soul")
    incarnate.add_argument("soul_id")
    incarnate.add_argument("--machine", default="self")
    incarnate.add_argument("--name", required=True)
    incarnate.add_argument("--cwd", default=".")
    incarnate.set_defaults(function=command_incarnate, command=[])

    run = commands.add_parser("run", help=argparse.SUPPRESS)
    run.add_argument("soul_id")
    run.add_argument("command", nargs=argparse.REMAINDER)
    run.set_defaults(function=command_run)
    return result


def main() -> None:
    arguments = sys.argv[1:]
    incarnation_command: list[str] | None = None
    if "incarnate" in arguments:
        subcommand_index = arguments.index("incarnate")
        try:
            divider = arguments.index("--", subcommand_index + 1)
        except ValueError:
            pass
        else:
            incarnation_command = arguments[divider + 1 :]
            arguments = arguments[:divider]
    args = parser().parse_args(arguments)
    if args.subcommand == "incarnate" and incarnation_command is not None:
        args.command = incarnation_command
    try:
        args.function(args)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"treer-soul: {error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    main()
