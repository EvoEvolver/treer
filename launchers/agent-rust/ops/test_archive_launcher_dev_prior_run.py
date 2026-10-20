"""Synthetic-only preservation/undo checks; never opens the compiled live path."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


class ArchiveTests(unittest.TestCase):
    def fixture(self, base):
        spec = importlib.util.spec_from_file_location('archive_fixture', Path(__file__).with_name('archive-launcher-dev-prior-run.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.ROOT = base / 'private'
        module.AUDIT = base / 'audit'
        module.ARCHIVE = module.AUDIT / 'prior-run'
        module.ROOT.mkdir(mode=0o700)
        for name in ['images', 'rust-images', 'node-backup-v1', 'rust-journal-history']:
            (module.ROOT / name).mkdir(mode=0o700)
        for name, text in [('.rust-launcher.lock', ''), ('rust-spike.json', 'synthetic fence'),
                           ('.node-backup-restored', 'synthetic marker'),
                           ('node-backup-v1/queue.json', 'synthetic backup'),
                           ('rust-journal-history/00000000000000000001.json', 'synthetic history')]:
            (module.ROOT / name).write_text(text)
            (module.ROOT / name).chmod(0o600)
        return module

    def run_helper(self, module, undo=False):
        # Only replace the exact PID existence probe. All filesystem checks,
        # lease acquisition, hashes and renames run against this fixture root.
        with patch.object(sys, 'argv', ['archive-fixture', '--stopped-pid', '12345'] + (['--undo'] if undo else [])), \
             patch.object(module.os, 'kill', side_effect=ProcessLookupError):
            module.main()

    def test_archive_and_undo_preserve_tuple_history_and_lock_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            module = self.fixture(Path(directory).resolve())
            lock_inode = (module.ROOT / '.rust-launcher.lock').stat().st_ino
            self.run_helper(module)
            before = json.loads((module.AUDIT / 'prior-before.json').read_text())
            after = json.loads((module.AUDIT / 'prior-after.json').read_text())
            self.assertEqual(before, after)
            self.assertEqual(set(os.listdir(module.ROOT)), {'images', '.rust-launcher.lock'})
            self.assertEqual((module.ROOT / '.rust-launcher.lock').stat().st_ino, lock_inode)
            self.assertTrue((module.ARCHIVE / 'rust-journal-history/00000000000000000001.json').is_file())
            self.run_helper(module, True)
            self.assertEqual((module.ROOT / 'rust-spike.json').read_text(), 'synthetic fence')
            self.assertEqual((module.ROOT / '.rust-launcher.lock').stat().st_ino, lock_inode)

    def test_unsafe_evidence_refuses_without_moves(self):
        for case in ['symlink', 'hardlink', 'unexpected', 'nonempty-images']:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                module = self.fixture(Path(directory).resolve())
                history = module.ROOT / 'rust-journal-history'
                if case == 'symlink': (history / '00000000000000000002.json').symlink_to(history / '00000000000000000001.json')
                if case == 'hardlink': os.link(history / '00000000000000000001.json', history / '00000000000000000002.json')
                if case == 'unexpected': (module.ROOT / 'unrelated').touch(mode=0o600)
                if case == 'nonempty-images': (module.ROOT / 'images/draft').touch(mode=0o600)
                with self.assertRaises(AssertionError): self.run_helper(module)
                self.assertTrue((module.ROOT / 'rust-spike.json').exists())
                self.assertFalse(module.ARCHIVE.exists())

    def test_undo_refuses_new_import_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            module = self.fixture(Path(directory).resolve())
            self.run_helper(module)
            (module.ROOT / 'rust-spike.json').write_text('new import')
            (module.ROOT / 'rust-spike.json').chmod(0o600)
            with self.assertRaises(AssertionError): self.run_helper(module, True)
            self.assertEqual((module.ROOT / 'rust-spike.json').read_text(), 'new import')
            self.assertTrue((module.ARCHIVE / 'node-backup-v1/queue.json').exists())


if __name__ == '__main__':
    unittest.main()
