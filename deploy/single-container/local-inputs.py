"""Package locally compiled native binaries; this is NOT release provenance."""
import importlib.util
import json
from pathlib import Path
import re
import shutil
import sys
import tempfile

spec = importlib.util.spec_from_file_location("verified_inputs", Path(__file__).with_name("verify-inputs.py"))
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


def produce(target, output, commit, version, architecture):
    verifier.require(re.fullmatch(r"[0-9a-f]{40}", commit), "exact source commit required")
    verifier.require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "release version required")
    platform = {"amd64": "linux-x86_64", "arm64": "linux-aarch64"}[architecture]
    verifier.require(not output.exists(), "local output already exists")
    with tempfile.TemporaryDirectory() as temporary:
        source = Path(temporary)
        for kind, names in (("machines", verifier.MACHINE_FILES), ("proxy", {"treer-proxy"})):
            leaf = source / kind
            leaf.mkdir()
            for name in names:
                shutil.copyfile(target / name, leaf / name)
            (leaf / "SHA256SUMS").write_text("".join(
                f"{verifier.hash_file(leaf / name)}  {name}\n" for name in sorted(names)))
            (leaf / "build-metadata.json").write_text(json.dumps({
                "schema_version": 1, "git_commit": commit, "version": version, "platform": platform}) + "\n")
            # Reuse the existing strict ELF, fixed-file-set and checksum checks.
            verifier.verify_leaf(leaf, names, platform, commit, version)
        output.mkdir(parents=True)
        shutil.copytree(source / "machines", output / "dist" / platform)
        (output / "bin").mkdir()
        shutil.copyfile(source / "proxy" / "treer-proxy", output / "bin" / "treer-proxy")
        (output / "build.json").write_text(json.dumps({
            "schema_version": 1, "git_commit": commit, "version": version,
            "platform": platform, "provenance": "local-build",
            "input_files": verifier.input_hashes(source)}) + "\n")
    for path in [output, *output.rglob("*")]:
        path.chmod(0o755 if path.is_dir() or path.name in verifier.MACHINE_FILES | {"treer-proxy"} else 0o644)
    (output / "SHA256SUMS").write_text("".join(
        f"{digest}  {name}\n" for name, digest in verifier.input_hashes(output).items()))
    (output / "SHA256SUMS").chmod(0o644)


if __name__ == "__main__":
    produce(Path(sys.argv[1]), Path(sys.argv[2]), *sys.argv[3:6])
