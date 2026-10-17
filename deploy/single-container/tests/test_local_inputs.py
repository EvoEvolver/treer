import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("local_inputs", Path(__file__).parents[1] / "local-inputs.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LocalInputsTests(unittest.TestCase):
    def test_native_architecture_only_with_explicit_local_provenance(self):
        for arch, platform, machine in (("amd64", "linux-x86_64", 62), ("arm64", "linux-aarch64", 183)):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                for name in module.verifier.MACHINE_FILES | {"treer-proxy"}:
                    header = bytearray(64)
                    header[:6] = b"\x7fELF\x02\x01"
                    header[18:20] = machine.to_bytes(2, "little")
                    (root / name).write_bytes(header)
                output = root / "app"
                module.produce(root, output, "a" * 40, "0.1.14", arch)
                self.assertEqual({p.name for p in (output / "dist").iterdir()}, {platform})
                self.assertEqual(json.loads((output / "build.json").read_text())["provenance"], "local-build")
                self.assertEqual((output / "bin/treer-proxy").stat().st_mode & 0o777, 0o755)
                self.assertIn("bin/treer-proxy", (output / "SHA256SUMS").read_text())

    def test_wrong_architecture_rejected_before_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in module.verifier.MACHINE_FILES | {"treer-proxy"}:
                (root / name).write_bytes(b"invalid" * 20)
            with self.assertRaises(ValueError):
                module.produce(root, root / "app", "a" * 40, "0.1.14", "arm64")
            self.assertFalse((root / "app").exists())
