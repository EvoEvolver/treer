#!/usr/bin/env python3
"""Verify the exact-SHA native staging contract before copying into the image.

Metadata/checksums prove consistency, not the trustworthiness of their producer.
CI must download artifacts from the trusted native build of this exact commit.
"""
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import sys


PLATFORMS = {"linux-x86_64": 62, "linux-aarch64": 183}
MACHINE_FILES = {"treer", "treer-agent-host", "treer-agent-server"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def directory(path):
    require(stat.S_ISDIR(path.lstat().st_mode), "input directory must not be a symlink")
    return {entry.name for entry in path.iterdir()}


def read_file(path, limit):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size <= limit,
            "invalid input file type or size")
    return path.read_bytes()


def hash_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def input_hashes(source):
    return {path.relative_to(source).as_posix(): hash_file(path)
            for path in sorted(source.rglob("*")) if path.is_file() and path.name != ".gitignore"}


def input_digest(source):
    rows = "".join(f"{digest}  {name}\n" for name, digest in input_hashes(source).items())
    return hashlib.sha256(rows.encode("ascii")).hexdigest()


def verify_leaf(path, files, platform, commit, version):
    require(directory(path) == files | {"SHA256SUMS", "build-metadata.json"}, "unexpected or missing input files")
    metadata = json.loads(read_file(path / "build-metadata.json", 16384), object_pairs_hook=strict_object)
    require(isinstance(metadata, dict) and {"schema_version", "git_commit", "version", "platform"} <= metadata.keys()
            and metadata.keys() <= {"schema_version", "git_commit", "version", "platform", "rustc"},
            "unexpected metadata fields")
    require("rustc" not in metadata or isinstance(metadata["rustc"], str) and len(metadata["rustc"]) <= 1024,
            "invalid rustc metadata")
    require(type(metadata.get("schema_version")) is int and metadata.get("schema_version") == 1 and metadata.get("git_commit") == commit
            and metadata.get("version") == version and metadata.get("platform") == platform,
            "native metadata SHA/version/platform mismatch")
    rows = read_file(path / "SHA256SUMS", 4096).decode("ascii").splitlines()
    digests = {}
    for row in rows:
        match = re.fullmatch(r"([0-9a-f]{64})  ([a-z0-9-]+)", row)
        require(match is not None, "invalid checksum row")
        digest, name = match.groups()
        require(name in files and name not in digests, "unexpected or duplicate checksum entry")
        digests[name] = digest
    require(set(digests) == files, "missing checksum entries")
    for name, expected in digests.items():
        artifact = path / name
        info = artifact.lstat()
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and 64 <= info.st_size <= 512 * 1024 * 1024,
                "invalid native binary type or size")
        with artifact.open("rb") as stream:
            header = stream.read(64)
            require(header[:6] == b"\x7fELF\x02\x01" and int.from_bytes(header[18:20], "little") == PLATFORMS[platform],
                    "native binary architecture mismatch")
            digest = hashlib.sha256(header)
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(chunk)
        require(digest.hexdigest() == expected, "native binary checksum mismatch")


def verify(source, output, commit, version, architecture, cargo, expected_input_digest):
    require(re.fullmatch(r"[0-9a-f]{40}", commit), "exact 40-hex commit required")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "release version required")
    require(re.fullmatch(r"[0-9a-f]{64}", expected_input_digest), "trusted native input digest required")
    section = re.search(r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", cargo.read_text())
    require(section is not None, "workspace package version missing")
    versions = re.findall(r'^version\s*=\s*"([0-9.]+)"\s*$', section.group(1), re.MULTILINE)
    require(versions == [version], "native version differs from source Cargo.toml")
    platform = {"amd64": "linux-x86_64", "arm64": "linux-aarch64"}.get(architecture)
    require(platform is not None, "unsupported image architecture")
    require(directory(source) - {".gitignore"} == {"machines", "proxy"}, "invalid native input root")
    require(directory(source / "machines") == set(PLATFORMS), "both Linux machine platforms are required")
    proxy_platforms = directory(source / "proxy")
    require(platform in proxy_platforms and proxy_platforms <= set(PLATFORMS), "matching native Proxy is required")
    for machine_platform in PLATFORMS:
        verify_leaf(source / "machines" / machine_platform, MACHINE_FILES, machine_platform, commit, version)
    for proxy_platform in proxy_platforms:
        verify_leaf(source / "proxy" / proxy_platform, {"treer-proxy"}, proxy_platform, commit, version)
    require(input_digest(source) == expected_input_digest, "native input digest differs from trusted job output")
    # Copy only after ALL inputs verify. Do not emit a partially verified manifest.
    require(not output.exists(), "output must not already exist")
    output.mkdir(parents=True)
    shutil.copytree(source / "machines", output / "dist")
    (output / "bin").mkdir()
    shutil.copyfile(source / "proxy" / platform / "treer-proxy", output / "bin" / "treer-proxy")
    for binary in (output / "dist").glob("*/treer*"):
        binary.chmod(0o755)
    (output / "bin" / "treer-proxy").chmod(0o755)
    (output / "build.json").write_text(json.dumps({"schema_version": 1, "git_commit": commit,
                                                "version": version, "platform": platform,
                                                "input_sha256": expected_input_digest,
                                                "input_files": input_hashes(source)}) + "\n")
    checksums = []
    for binary in sorted(output.rglob("*")):
        if binary.is_file():
            digest = hash_file(binary)
            checksums.append(f"{digest}  {binary.relative_to(output)}\n")
    (output / "SHA256SUMS").write_text("".join(checksums))


if __name__ == "__main__":
    try:
        if len(sys.argv) != 8:
            raise ValueError("usage: verify-inputs.py INPUT OUTPUT COMMIT VERSION ARCH CARGO_TOML TRUSTED_INPUT_SHA256")
        verify(Path(sys.argv[1]), Path(sys.argv[2]), *sys.argv[3:6], Path(sys.argv[6]), sys.argv[7])
    except (OSError, ValueError, KeyError, TypeError):
        sys.exit("single-container native artifact verification failed")
