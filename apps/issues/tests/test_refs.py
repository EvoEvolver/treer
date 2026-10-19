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
import refs
from store import Store

PEOPLE = [{'id': 'ag_a', 'kind': 'agent', 'name': 'a'}, {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}, {'id': 'usr_h', 'kind': 'human', 'name': 'h'}]
A = {'id': 'ag_a', 'kind': 'agent', 'name': 'a'}
B = {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}
H = {'id': 'usr_h', 'kind': 'human', 'name': 'h', 'role': 'admin'}
T0 = 1_800_000_000.0


class RefsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-refs-')
        self.path = Path(self.directory.name) / 'issues.sqlite3'
        self.now = T0
        self.store = Store(self.path, clock=lambda: self.now)
        self.target = self.issue(title='Target')

    def tearDown(self):
        self.directory.cleanup()

    def issue(self, actor=A, **values):
        self.now += 1
        return self.store.mutate(None, 'create', {'title': 'Untitled', **values}, actor, PEOPLE)[0]

    def comment(self, issue, body, actor=A, title=None):
        self.now += 1
        return self.store.mutate(issue, 'comment', dict(body=body, **({'title': title} if title else {})), actor, PEOPLE)

    def edit(self, issue, body, actor=A):
        self.now += 1
        return self.store.mutate(issue, 'update', body, actor, PEOPLE)

    def backlinks(self, issue=None):
        return self.store.show(issue or self.target)['referenced_by']

    def test_title_description_and_comment_references_become_backlinks(self):
        t, d, c = self.issue(title=f'See #{self.target}'), self.issue(description=f'depends on #{self.target}'), self.issue()
        self.comment(c, f'related to #{self.target}', B, title='note')
        links = {(l['issue']['id'], l['source'], l['author']) for l in self.backlinks()}
        self.assertEqual(links, {(t, 'title', 'ag_a'), (d, 'description', 'ag_a'), (c, 'comment', 'ag_b')})
        self.assertIsNotNone(next(l for l in self.backlinks() if l['source'] == 'comment')['event_id'])
        self.assertEqual([l['issue']['id'] for l in self.backlinks()][0], c, 'newest first')

    def test_parsing_boundaries(self):
        ok = self.issue(description=f'(#{self.target}) and ,#{self.target}.')
        bad = self.issue(description=f'a#{self.target} #{self.target}x /issues/#{self.target} &#{self.target} ##{self.target} -#{self.target} #0 #99999999999 #-1 ISS-{self.target}')
        ids = [l['issue']['id'] for l in self.backlinks()]
        self.assertIn(ok, ids)
        self.assertNotIn(bad, ids, 'only a standalone #N counts')
        self.assertEqual(self.backlinks(ok), [], 'nothing references the referencing issue')
        own = self.issue(title='self')
        self.edit(own, {'description': f'I am #{own}'})
        self.assertEqual(self.backlinks(own), [], 'an issue never links to itself')

    def test_unknown_issues_and_the_per_source_cap(self):
        self.issue(description='#9999 #8888')
        self.assertEqual(self.backlinks(), [])
        many = [self.issue() for _ in range(25)]
        hub = self.issue(description=' '.join(f'#{n}' for n in many))
        counts = sum(1 for n in many if any(l['issue']['id'] == hub for l in self.backlinks(n)))
        self.assertEqual(counts, refs.MAX_PER_SOURCE, 'a single text links at most 20 issues')

    def test_rewriting_the_text_removes_old_links_and_comments_keep_theirs(self):
        other = self.issue(title='Other')
        source = self.issue(description=f'#{self.target}')
        self.comment(source, f'also #{other}')
        self.assertEqual([l['issue']['id'] for l in self.backlinks()], [source])
        self.edit(source, {'description': f'now about #{other}'})
        self.assertEqual(self.backlinks(), [], 'the description no longer mentions the target')
        self.assertEqual([l['issue']['id'] for l in self.backlinks(other)], [source])
        self.edit(source, {'title': f'renamed #{self.target}'})
        self.assertEqual([(l['source']) for l in self.backlinks()], ['title'])

    def test_one_entry_per_referencing_issue_capped_at_fifty_with_plain_bounded_titles(self):
        hostile = self.issue(title='<b>' + 'x' * 200 + '</b> ‮\n' + f'#{self.target}')
        entry = self.backlinks()[0]
        self.assertLessEqual(len(entry['issue']['title']), refs.TITLE_CHARS)
        self.assertNotIn('\n', entry['issue']['title'])
        for _ in range(60):
            self.issue(description=f'#{self.target}')
        self.assertEqual(len(self.backlinks()), refs.MAX_REFERENCED_BY)
        self.comment(hostile, f'again #{self.target}'); self.comment(hostile, f'and again #{self.target}')
        self.assertEqual(len([l for l in self.backlinks() if l['issue']['id'] == hostile]), 1, 'one entry per issue')

    def test_links_only_no_side_effects_on_the_referenced_issue(self):
        before = self.store.show(self.target)['issue']
        self.issue(description=f'#{self.target}')
        self.comment(self.issue(), f'#{self.target}')
        after = self.store.show(self.target)['issue']
        self.assertEqual((before['version'], before['updated_at'], before['status']), (after['version'], after['updated_at'], after['status']))

    def test_derivation_from_stored_text_is_idempotent(self):
        source = self.issue(description=f'#{self.target}')
        self.comment(source, f'#{self.target}')
        before = self.backlinks()
        with sqlite3.connect(self.path) as db:
            db.execute('DELETE FROM refs_meta'); db.execute('DELETE FROM issue_refs')
        again = Store(self.path, clock=lambda: self.now)
        self.assertEqual([(l['issue']['id'], l['source'], l['author']) for l in again.show(self.target)['referenced_by']],
                         [(l['issue']['id'], l['source'], l['author']) for l in before])
        Store(self.path, clock=lambda: self.now)
        with sqlite3.connect(self.path) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM issue_refs').fetchone()[0], 2)


if __name__ == '__main__':
    unittest.main()
