"""The slim App starts on a database written by the previous releases and leaves every existing row alone.

fixtures/legacy-issues.db is a database written by the code the live deployment runs (branch
issues-default-cap-245: origin/main's schema plus the stall_* / delivery_credits tables), with issues, comments,
leases, waiting_on, notification policies and windows, reminders, shipped signals and stall rows.
"""
import json
import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'tests'))
from hermetic import hermetic_env
clean = hermetic_env(); os.environ.clear(); os.environ.update(clean)
from store import LEGACY_FIELDS, Store
import issues as server

FIXTURE = ROOT / 'tests/fixtures/legacy-issues.db'
RETIRED = ('reminders', 'notify_windows', 'notify_proven', 'notify_budget', 'notify_policy', 'notify_policy_audit',
           'notify_ask_budget', 'shipped_signals', 'reminder_budget', 'reminder_backoff', 'stall_state', 'stall_outbox',
           'stall_leader_budget', 'delivery_credits')
PEOPLE = [{'id': 'ag_a', 'kind': 'agent', 'name': 'builder'}, {'id': 'ag_b', 'kind': 'agent', 'name': 'reviewer'},
          {'id': 'usr_o', 'kind': 'human', 'name': 'owner', 'role': 'owner'}]


def snapshot(path):
    db = sqlite3.connect(path)
    try:
        tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")]
        return {t: sorted(map(repr, db.execute(f'SELECT * FROM "{t}"').fetchall())) for t in tables}
    finally:
        db.close()


class LegacyDatabaseTest(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix='issues-migrate-')
        self.path = Path(self.folder.name) / 'issues.sqlite3'
        self.path.write_bytes(FIXTURE.read_bytes())      # a private copy: the fixture itself is never opened for writing
        self.before = snapshot(self.path)

    def tearDown(self):
        self.folder.cleanup()

    def test_fixture_has_the_retired_state(self):
        for table in RETIRED:
            self.assertTrue(self.before.get(table), f'{table} has rows in the fixture')
        stored = [json.loads(eval(r)[1]) for r in self.before['issues']]
        self.assertTrue(any(i.get('claim') for i in stored) and any(i.get('waiting_on') for i in stored))

    def test_start_read_and_write_leave_existing_rows_untouched(self):
        store = Store(self.path)
        after_start = snapshot(self.path)
        self.assertEqual(set(after_start) - set(self.before), {'wakes'}, 'starting only adds the wake table')
        self.assertEqual({t: v for t, v in after_start.items() if t != 'wakes'}, self.before, 'starting rewrites nothing')

        listed = store.list({'limit': '100'})
        self.assertEqual(listed['total'], len(self.before['issues']))
        for issue in listed['issues']:
            shown = store.show(issue['id'], limit=100)
            self.assertFalse(set(LEGACY_FIELDS) & set(shown['issue']), 'retired fields are not shown')
            count = sum(1 for r in self.before['activity'] if eval(r)[1] == issue['id'])
            self.assertEqual(len(shown['activity']), count, 'every activity entry is still listed')
        self.assertTrue(store.search({'q': 'reasoning'})['count'] >= 1, 'the existing search index answers')
        self.assertEqual(snapshot(self.path), after_start, 'reads write nothing')

        leased = next(json.loads(eval(r)[1]) | {'id': eval(r)[0]} for r in self.before['issues'] if json.loads(eval(r)[1]).get('claim'))
        # a lease or wait recorded by the old code no longer blocks anyone: another principal edits and comments
        store.mutate(leased['id'], 'update', {'title': 'Edited after the upgrade'}, PEOPLE[1], PEOPLE)
        store.mutate(leased['id'], 'comment', {'body': 'new comment'}, PEOPLE[1], PEOPLE)
        store.set_hidden(leased['id'], None, True, PEOPLE[2])
        store.set_hidden(leased['id'], None, False, PEOPLE[2])
        after = snapshot(self.path)
        for table in RETIRED + ('oauth', 'sessions', 'hierarchy_meta', 'refs_meta', 'search_meta'):
            self.assertEqual(after[table], self.before[table], f'{table} is left as it was')
        self.assertTrue(set(self.before['activity']) <= set(after['activity']), 'no existing activity entry changed or vanished')
        old_issues = {eval(r)[0]: r for r in self.before['issues']}
        new_issues = {eval(r)[0]: r for r in after['issues']}
        self.assertEqual({k: v for k, v in new_issues.items() if k != leased['id']}, {k: v for k, v in old_issues.items() if k != leased['id']})
        stored = json.loads(eval(new_issues[leased['id']])[1])
        self.assertEqual(stored['title'], 'Edited after the upgrade')
        self.assertEqual(stored['claim'], leased['claim'], 'retired fields stay in storage, untouched')

    def test_old_command_line_still_starts(self):
        arguments = server.parse_args(['--proxy-url', 'https://proxy.example/', '--leader-agent', 'ag_' + '1' * 32, '--release-agent', 'ag_' + '1' * 32])
        self.assertEqual(arguments.proxy_url, 'https://proxy.example/')


if __name__ == '__main__':
    unittest.main()
