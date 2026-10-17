import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("verify_inputs", Path(__file__).parents[1] / "verify-inputs.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
COMMIT = "1" * 40
VERSION = "0.1.14"


class InputTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.source = Path(self.tmp.name) / "input"
        self.output = Path(self.tmp.name) / "output"
        self.cargo = Path(self.tmp.name) / "Cargo.toml"
        self.cargo.write_text('[workspace.package]\nversion = "' + VERSION + '"\n[workspace.dependencies]\n')
        for platform, machine in module.PLATFORMS.items():
            self.leaf(self.source / "machines" / platform, module.MACHINE_FILES, platform, machine)
            self.leaf(self.source / "proxy" / platform, {"treer-proxy"}, platform, machine)

    def tearDown(self):
        self.tmp.cleanup()

    def leaf(self, path, files, platform, machine):
        path.mkdir(parents=True)
        for name in files:
            header = bytearray(64)
            header[:6] = b"\x7fELF\x02\x01"
            header[18:20] = machine.to_bytes(2, "little")
            (path / name).write_bytes(header + name.encode())
        self.checksums(path, files)
        (path / "build-metadata.json").write_text(json.dumps({"schema_version": 1, "git_commit": COMMIT,
                                                            "version": VERSION, "platform": platform}))

    def checksums(self, path, files):
        (path / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256((path / name).read_bytes()).hexdigest()}  {name}\n" for name in sorted(files)))

    def verify(self, arch="arm64"):
        module.verify(self.source, self.output, COMMIT, VERSION, arch, self.cargo, module.input_digest(self.source))

    def rejected(self):
        with self.assertRaises((ValueError, OSError)):
            self.verify()
        self.assertFalse(self.output.exists())

    def test_both_platforms_and_native_proxy_verified_then_executable(self):
        self.verify()
        self.assertEqual(json.loads((self.output / "build.json").read_text())["git_commit"], COMMIT)
        self.assertTrue((self.output / "bin" / "treer-proxy").stat().st_mode & 0o111)
        self.assertEqual({p.name for p in (self.output / "dist").iterdir()}, set(module.PLATFORMS))
        self.assertIn("dist/linux-x86_64/treer-agent-server", (self.output / "SHA256SUMS").read_text())

    def test_amd64_native_proxy_selected(self):
        self.verify("amd64")
        self.assertEqual((self.output / "bin" / "treer-proxy").read_bytes()[18:20], (62).to_bytes(2, "little"))

    def test_wrong_sha_or_version_or_platform_rejected(self):
        path = self.source / "machines/linux-aarch64/build-metadata.json"
        original = json.loads(path.read_text())
        for key, value in (("git_commit", "2" * 40), ("version", "0.1.13"), ("platform", "linux-x86_64"), ("schema_version", 2)):
            path.write_text(json.dumps({**original, key: value}))
            self.rejected()

    def test_changed_bytes_rejected_before_output(self):
        path = self.source / "proxy/linux-aarch64/treer-proxy"
        path.write_bytes(path.read_bytes() + b"tampered")
        self.rejected()

    def test_wrong_machine_even_with_valid_hash_rejected(self):
        leaf = self.source / "proxy/linux-aarch64"
        (leaf / "treer-proxy").write_bytes((self.source / "proxy/linux-x86_64/treer-proxy").read_bytes())
        self.checksums(leaf, {"treer-proxy"})
        self.rejected()

    def test_duplicate_checksum_or_traversal_rejected(self):
        path = self.source / "proxy/linux-aarch64/SHA256SUMS"
        original = path.read_text()
        for content in (original * 2, original.replace("treer-proxy", "../treer-proxy"), ""):
            path.write_text(content)
            self.rejected()

    def test_symlink_native_file_rejected(self):
        path = self.source / "proxy/linux-aarch64/treer-proxy"
        path.rename(path.with_name("outside"))
        path.symlink_to("outside")
        # Restore the expected set; place the target outside the leaf.
        path.with_name("outside").rename(Path(self.tmp.name) / "outside")
        self.rejected()

    def test_missing_platform_rejected(self):
        (self.source / "machines/linux-x86_64/treer").unlink()
        self.rejected()

    def test_unexpected_input_rejected(self):
        (self.source / "proxy/linux-aarch64/not-a-release-file").write_text("no")
        self.rejected()

    def test_source_version_mismatch_rejected(self):
        self.cargo.write_text('[workspace.package]\nversion = "0.1.13"\n')
        self.rejected()

    def test_duplicate_or_extra_metadata_keys_rejected(self):
        path = self.source / "proxy/linux-aarch64/build-metadata.json"
        original = path.read_text()
        for content in (original[:-1] + ', "version":"0.1.14"}', original[:-1] + ', "extra":true}'):
            path.write_text(content)
            self.rejected()

    def test_trusted_job_digest_mismatch_rejects_self_consistent_inputs(self):
        digest = module.input_digest(self.source)
        leaf = self.source / "proxy/linux-aarch64"
        binary = leaf / "treer-proxy"
        binary.write_bytes(binary.read_bytes() + b"tampered")
        self.checksums(leaf, {"treer-proxy"})
        with self.assertRaisesRegex(ValueError, "trusted job"):
            module.verify(self.source, self.output, COMMIT, VERSION, "arm64", self.cargo, digest)
        self.assertFalse(self.output.exists())

    def test_unsupported_architecture_rejected(self):
        with self.assertRaises(ValueError):
            self.verify("riscv64")
        self.assertFalse(self.output.exists())
