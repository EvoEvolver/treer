"""Actual wrapper entry controls; synthetic Soul/provider images only."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ENTRY = Path(__file__).with_name('soul-continue.py')


class Continuation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='soul-continue-')
        self.home = Path(self.temp.name).resolve()
        self.root = self.home / 'existing-incarnation'
        self.root.mkdir()
        self.cwd = self.home / 'unchanged-workspace'
        self.cwd.mkdir()
        self.restore = self.root / 'restore.sh'
        self.restore.write_text('exit 97 # must never execute archive restoration\n')
        self.image = self.home / 'fake-native'
        self.image.write_text('#!' + sys.executable + '\nimport os,json,sys\n'
                              'print(json.dumps(dict(cwd=os.getcwd(),args=sys.argv[1:],'
                              'soul=os.environ.get("TREER_SOUL_ID"),'
                              'root=os.environ.get("TREER_SOUL_ROOT"),'
                              'restore=os.environ.get("SOUL_RESTORE"),'
                              'credential=os.environ.get("SYNTHETIC_CREDENTIAL"),'
                              'seed=os.environ.get("RUST_CLAUDE_IMPORT_SNAPSHOT"),'
                              'override=os.environ.get("CLAUDE_BIN"))))\n')
        self.image.chmod(0o700)
        self.shell = str(Path(shutil.which('sh')).resolve())
        self.old_entry = self.home / 'old-node-entry.sh'
        self.old_entry.write_text('test "$TREER_SOUL_ID" = soul-test || exit 98\n'
                                  'test "$1" = --harness || exit 99\n'
                                  'printf "original_node_continued\\n"\n')
        env = dict(AGENT_UI_HARNESS='claude', AGENT_UI_CWD=str(self.cwd),
                   AGENT_UI_SESSION_ID='same-session')
        self.value = dict(schema='treer.soul-continuation/v1', agent='agent',
                          server='server', workspace='workspace', session='same-session',
                          home=str(self.home), cwd=str(self.cwd), soulId='soul-test',
                          soulRoot=str(self.root), restore=str(self.restore),
                          restoreSha256=self.sha(self.restore),
                          images={str(p): self.sha(p) for p in
                                  [self.image, self.old_entry, Path(self.shell)]},
                          modes={
                              'rust-import': dict(executable=str(self.image),
                                                  args=['--import-claude-state'],
                                                  env={**env, 'RUST_CLAUDE_IMPORT_SNAPSHOT': str(self.home / 'seed')}),
                              'rust': dict(executable=str(self.image), args=[], env=env),
                              'node-rollback': dict(executable=self.shell,
                                                    args=[str(self.old_entry), '--harness', 'claude',
                                                          '--session-id', 'same-session', '--cwd', str(self.cwd),
                                                          '--name', 'Claude'], env=env)})
        self.manifest = self.home / 'manifest.json'

    @staticmethod
    def sha(path):
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def tearDown(self):
        self.temp.cleanup()

    def run_entry(self, mode='rust', **changes):
        self.manifest.write_text(json.dumps(self.value))
        self.manifest.chmod(0o600)
        env = dict(PATH=os.defpath, TREER_AGENT_ID='agent', TREER_SERVER_ID='server',
                   TREER_WORKSPACE_ID='workspace', SYNTHETIC_CREDENTIAL='inherited-only',
                   CLAUDE_BIN='must-not-override-native', RUST_CLAUDE_IMPORT_SNAPSHOT='stale')
        env.update(changes)
        return subprocess.run([sys.executable, '-I', '-B', str(ENTRY), str(self.manifest), mode],
                              env=env, capture_output=True, text=True, timeout=5)

    def test_actual_exec_preserves_incarnation_and_credentials_without_restore(self):
        result = self.run_entry()
        self.assertEqual(result.returncode, 0, result.stderr)
        view = json.loads(result.stdout)
        self.assertEqual(view['cwd'], str(self.cwd))
        self.assertEqual(view['soul'], 'soul-test')
        self.assertEqual(view['root'], str(self.root))
        self.assertEqual(view['restore'], str(self.restore))
        self.assertEqual(view['credential'], 'inherited-only')
        self.assertIsNone(view['seed'])
        self.assertIsNone(view['override'])
        self.assertEqual(view['args'], [])

    def test_import_only_has_seed_and_flag(self):
        result = self.run_entry('rust-import')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['args'], ['--import-claude-state'])
        self.assertEqual(json.loads(result.stdout)['seed'], str(self.home / 'seed'))

    def test_original_node_entry_can_continue_without_restoring_archive(self):
        result = self.run_entry('node-rollback')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, 'original_node_continued\n')

    def test_wrong_agent_refuses_before_exec(self):
        result = self.run_entry(TREER_AGENT_ID='other')
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stderr.strip(), 'soul_continuation_refused')
        self.assertEqual(result.stdout, '')

    def test_changed_restore_refuses_without_replaying_it(self):
        self.restore.write_text('exit 96\n')
        self.assertEqual(self.run_entry().returncode, 2)

    def test_changed_image_refuses(self):
        self.image.write_text('exit 95\n')
        self.assertEqual(self.run_entry().returncode, 2)

    def test_session_override_refuses(self):
        self.value['modes']['rust']['env']['AGENT_UI_SESSION_ID'] = 'different'
        self.assertEqual(self.run_entry().returncode, 2)

    def test_linked_restore_refuses(self):
        moved = self.root / 'original'
        self.restore.rename(moved)
        self.restore.symlink_to(moved)
        self.assertEqual(self.run_entry().returncode, 2)


if __name__ == '__main__':
    unittest.main()
