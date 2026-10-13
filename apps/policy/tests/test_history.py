"""Version persistence tests; local synthetic policy data only."""
import copy
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('history_policy', ROOT / 'policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class HistoryTests(unittest.TestCase):
    def test_legacy_backup_rollback_and_monotonic_revisions(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'policy.json'
            legacy = {'revision': 12, 'mode': 'monitor', 'published_at': policy.utc_now(),
                      'document': {'schema_version': 1, 'defaults': {'agent.prompt': 'deny'}, 'groups': {}, 'rules': []}}
            path.write_text(json.dumps(legacy))
            store = policy.PolicyStore(path, 'ws_test')
            backup = path.with_name('policy.json.pre-history.bak')
            self.assertEqual(json.loads(backup.read_text()), legacy)
            self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
            changed = copy.deepcopy(legacy['document'])
            changed['defaults']['agent.prompt'] = 'allow'
            store.publish({'expected_revision': 12, 'mode': 'enforce', 'document': changed})
            restored = store.rollback(13, 12)
            self.assertEqual(restored['revision'], 14)
            self.assertEqual(restored['mode'], 'monitor')
            self.assertEqual(restored['document'], legacy['document'])
            history = store.history()['revisions']
            self.assertEqual([row['revision'] for row in history], [14, 13, 12])
            self.assertEqual(history[0]['restored_from'], 12)
            reload = policy.PolicyStore(path, 'ws_test')
            self.assertEqual(reload.bundle(), store.bundle())
            self.assertEqual(reload.history(), store.history())
            with self.assertRaises(policy.PolicyError): store.rollback(13, 12)
            self.assertEqual(store.bundle()['revision'], 14)

    def test_retention_and_returned_snapshots_are_immutable(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = policy.PolicyStore(Path(tmp) / 'policy.json', 'ws_test')
            old_limit = policy.MAX_HISTORY
            policy.MAX_HISTORY = 3
            try:
                for revision in range(1, 5):
                    store.publish({'expected_revision': revision, 'mode': 'monitor', 'document': store.bundle()['document']})
                self.assertEqual([r['revision'] for r in store.history()['revisions']], [5, 4, 3])
                with self.assertRaises(policy.PolicyError): store.revision(1)
                snapshot = store.revision(4)
                snapshot['document']['defaults']['x'] = 'deny'
                bundle = store.bundle()
                bundle['document']['rules'].append({'bad': True})
                self.assertEqual(store.revision(4)['document']['defaults'], {})
                self.assertEqual(store.bundle()['document']['rules'], [])
            finally: policy.MAX_HISTORY = old_limit

    def test_invalid_history_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'policy.json'
            store = policy.PolicyStore(path, 'ws_test')
            store.publish({'expected_revision': 1, 'mode': 'monitor', 'document': store.bundle()['document']})
            data = json.loads(path.read_text())
            data['history'][0]['revision'] = 2
            path.write_text(json.dumps(data))
            with self.assertRaises(RuntimeError): policy.PolicyStore(path, 'ws_test')

if __name__ == '__main__': unittest.main()
