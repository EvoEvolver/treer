#!/usr/bin/env python3
"""Assemble the exact-SHA native input tree consumed by Dockerfile.single.

Reads the artifacts of ONE native workflow run (as downloaded into <artifacts>) and writes
<output>/machines/<platform>/ and <output>/proxy/<platform>/ in the layout deploy/single-container/
verify-inputs.py requires. It only arranges files; verify-inputs.py (run inside the image build)
checks every leaf. It prints the input digest, which the workflow publishes as a job output so the
image jobs never recompute their own trust anchor.
"""
import hashlib
import shutil
import stat
import sys
from pathlib import Path

PLATFORMS = ("linux-x86_64", "linux-aarch64")
MACHINE_FILES = {"treer", "treer-agent-host", "treer-agent-server", "SHA256SUMS", "build-metadata.json"}
PROXY_FILES = {"treer-proxy", "SHA256SUMS", "build-metadata.json"}


def leaf(source, destination, files):
    if not source.is_dir() or source.is_symlink():
        raise SystemExit(f"missing artifact directory {source.name}")
    names = {entry.name for entry in source.iterdir()}
    if names != files:
        raise SystemExit(f"{source.name} must contain exactly {sorted(files)}, found {sorted(names)}")
    destination.mkdir(parents=True)
    for name in sorted(files):
        info = (source / name).lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise SystemExit(f"{source.name}/{name} is not a plain file")
        shutil.copyfile(source / name, destination / name)
        (destination / name).chmod(0o755 if name.startswith("treer") else 0o644)


def input_digest(root):
    rows = "".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(root).as_posix()}\n"
        for path in sorted(root.rglob("*")) if stat.S_ISREG(path.lstat().st_mode) and path.name != ".gitignore")
    return hashlib.sha256(rows.encode("ascii")).hexdigest()


def main(argv):
    if len(argv) != 3:
        raise SystemExit("usage: assemble-single-container-input.py <artifacts-dir> <output-dir>")
    artifacts, output = Path(argv[1]), Path(argv[2])
    # The checkout tracks only a .gitignore placeholder here (the Dockerfile COPYs this directory).
    if output.exists() and (output.is_symlink() or not output.is_dir() or {e.name for e in output.iterdir()} - {".gitignore"}):
        raise SystemExit("output directory must not exist or hold only .gitignore")
    output.mkdir(exist_ok=True)
    for platform in PLATFORMS:
        leaf(artifacts / f"treer-{platform}", output / "machines" / platform, MACHINE_FILES)
        leaf(artifacts / f"treer-proxy-{platform}", output / "proxy" / platform, PROXY_FILES)
    print(input_digest(output))


if __name__ == "__main__":
    main(sys.argv)
