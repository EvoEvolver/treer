import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSEMBLE = ROOT / "scripts" / "assemble-single-container-input.py"
spec = importlib.util.spec_from_file_location("verify_inputs", ROOT / "deploy" / "single-container" / "verify-inputs.py")
verify_inputs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_inputs)
COMMIT = "a" * 40
VERSION = "0.1.14"
MACHINES = {"linux-x86_64": 62, "linux-aarch64": 183}


def binary(machine, name):
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header) + name.encode()


def leaf(directory, files, platform, machine):
    directory.mkdir(parents=True)
    rows = []
    for name in sorted(files):
        (directory / name).write_bytes(binary(machine, name))
        rows.append(f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n")
    (directory / "SHA256SUMS").write_text("".join(rows))
    (directory / "build-metadata.json").write_text(json.dumps(
        {"schema_version": 1, "git_commit": COMMIT, "version": VERSION, "platform": platform, "rustc": "rustc 1.90.0"}))


class AssembleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.artifacts, self.output = base / "artifacts", base / "input"
        for platform, machine in MACHINES.items():
            leaf(self.artifacts / f"treer-{platform}", {"treer", "treer-launch", "treer-agent-host", "treer-agent-server"}, platform, machine)
            leaf(self.artifacts / f"treer-proxy-{platform}", {"treer-proxy"}, platform, machine)
        self.cargo = base / "Cargo.toml"
        self.cargo.write_text(f'[workspace.package]\nversion = "{VERSION}"\n[workspace.dependencies]\n')

    def tearDown(self):
        self.tmp.cleanup()

    def run_assemble(self):
        return subprocess.run([sys.executable, "-I", str(ASSEMBLE), str(self.artifacts), str(self.output)], capture_output=True, text=True)

    def test_output_passes_the_image_verifier_and_digest_matches_it(self):
        result = self.run_assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        digest = result.stdout.strip()
        self.assertEqual(digest, verify_inputs.input_digest(self.output))
        for architecture in ("amd64", "arm64"):
            verified = Path(self.tmp.name) / ("verified-" + architecture)
            verify_inputs.verify(self.output, verified, COMMIT, VERSION, architecture, self.cargo, digest)
            self.assertTrue((verified / "bin" / "treer-proxy").is_file())

    def test_an_output_directory_holding_only_gitignore_is_accepted(self):
        self.output.mkdir()
        (self.output / ".gitignore").write_text("*\n")
        result = self.run_assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), verify_inputs.input_digest(self.output))

    def test_a_non_empty_output_directory_is_refused(self):
        self.output.mkdir()
        (self.output / "stale").write_text("x")
        self.assertNotEqual(self.run_assemble().returncode, 0)

    def test_a_missing_proxy_artifact_is_refused(self):
        import shutil
        shutil.rmtree(self.artifacts / "treer-proxy-linux-aarch64")
        self.assertNotEqual(self.run_assemble().returncode, 0)
        self.assertFalse(self.output.exists() and (self.output / "proxy" / "linux-aarch64").exists())

    def test_an_extra_file_in_an_artifact_is_refused(self):
        (self.artifacts / "treer-linux-x86_64" / "extra").write_text("x")
        result = self.run_assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must contain exactly", result.stderr)

    def test_a_symlink_artifact_directory_is_refused(self):
        real = self.artifacts / "treer-proxy-linux-x86_64"
        moved = self.artifacts / "moved"
        real.rename(moved)
        real.symlink_to(moved)
        self.assertNotEqual(self.run_assemble().returncode, 0)


if __name__ == "__main__":
    unittest.main()
