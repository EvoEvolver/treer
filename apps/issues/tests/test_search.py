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
import search as fulltext
from auth import Error
from issues import Application
from store import Store

PEOPLE = [{'id': 'ag_a', 'kind': 'agent', 'name': 'a'}, {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}, {'id': 'usr_h', 'kind': 'human', 'name': 'h'}]
A = {'id': 'ag_a', 'kind': 'agent', 'name': 'a'}
B = {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}
H = {'id': 'usr_h', 'kind': 'human', 'name': 'h', 'role': 'admin'}
T0 = 1_800_000_000.0


class SearchBase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-search-')
        self.path = Path(self.directory.name) / 'issues.sqlite3'
        self.now = T0
        self.store = Store(self.path, clock=lambda: self.now)

    def tearDown(self):
        self.directory.cleanup()

    def issue(self, actor=A, **values):
        return self.store.mutate(None, 'create', {'title': 'Untitled', **values}, actor, PEOPLE)[0]

    def edit(self, issue, body, actor=A):
        return self.store.mutate(issue, 'update', body, actor, PEOPLE)

    def comment(self, issue, body, actor=A, title=None):
        return self.store.mutate(issue, 'comment', dict(body=body, **({'title': title} if title else {})), actor, PEOPLE)

    def find(self, q, **params):
        return self.store.search(dict(q=q, **params))['results']

    def ids(self, q, **params):
        return [r['issue']['id'] for r in self.find(q, **params)]


class MatchingTest(SearchBase):
    def test_titles_descriptions_and_comments_with_author_attribution(self):
        one = self.issue(A, title='Compiler crash on startup', description='stack overflow in the parser')
        two = self.issue(B, title='Docs')
        self.comment(two, 'The parser needs a rewrite', B, title='[decision] parser rewrite')
        hits = self.find('parser')
        self.assertEqual({(r['issue']['id'], r['match']['kind'], r['match']['author']) for r in hits},
                         {(one, 'description', 'ag_a'), (two, 'comment', 'ag_b')})
        comment = next(r for r in hits if r['match']['kind'] == 'comment')
        self.assertIsNotNone(comment['match']['event_id'])
        self.assertTrue(all(r['snippet_untrusted'] is True for r in hits))
        self.assertEqual(self.ids('COMPILER'), [one], 'case-insensitive')
        self.assertEqual(self.ids('crash startup'), [one], 'every word must match')

    def test_the_last_editor_is_the_author_of_a_changed_title_or_description(self):
        issue = self.issue(A, title='First title', description='keep this text')
        self.edit(issue, {'title': 'Second title'}, H)
        by_kind = {r['match']['kind']: r['match']['author'] for r in self.find('title')}
        self.assertEqual(by_kind, {'title': 'usr_h'})
        self.assertEqual(self.find('keep this')[0]['match']['author'], 'ag_a', 'unchanged text keeps its author')
        self.edit(issue, {'description': 'new words only'}, A)
        self.assertEqual(self.ids('keep this'), [], 'replaced text is gone from the index')
        self.assertEqual(self.find('new words')[0]['match']['author'], 'ag_a')
        self.assertEqual(self.ids('First title'), [], 'the old title is gone')

    def test_new_writes_are_searchable_at_once(self):
        issue = self.issue(A, title='Fresh')
        self.assertEqual(self.ids('zebra'), [])
        self.comment(issue, 'a zebra appeared')
        self.assertEqual(self.ids('zebra'), [issue])

    def test_cjk_and_short_terms(self):
        issue = self.issue(A, title='数据库设计讨论', description='README 里 a_b 和 100% 的情况')
        self.assertEqual(self.ids('数据库'), [issue], 'three characters use the trigram index')
        self.assertEqual(self.ids('数据'), [issue], 'two characters use the bounded scan')
        self.assertEqual(self.ids('a_'), [issue]); self.assertEqual(self.ids('%'), [issue])
        self.assertEqual(self.ids('_'), [issue]); other = self.issue(A, title='plain words')
        self.assertNotIn(other, self.ids('%'), 'LIKE wildcards are literal')
        self.assertNotIn(other, self.ids('_'))
        self.assertNotIn(other, self.ids('\\'))

    def test_fts_syntax_is_never_interpreted(self):
        issue = self.issue(A, title='alpha beta', description='gamma')
        for q in ('alpha OR zzzzzz', 'alpha"', '"alpha beta', 'NEAR(alpha beta)', 'title:alpha', 'alpha*', '-alpha', 'alpha AND NOT beta', '^alpha', '(((', "alpha'; DROP TABLE issues;--"):
            self.find(q)                                 # none of these may raise or change the meaning of the query
        self.assertEqual(self.ids('alpha OR zzzzzz'), [], 'OR is just another word that must also match')
        self.assertEqual(self.ids('title:alpha'), [])
        self.assertEqual(self.ids('alpha*'), [])
        self.assertEqual(self.ids('alpha beta'), [issue])

    def test_snippets_are_bounded_single_line_plain_text(self):
        text = 'start ' + ('filler ' * 80) + 'NEEDLE' + '\n\x1b[31mred‮​  end\n' + ('tail ' * 80)
        issue = self.issue(A, title='long', description=text)
        snippet = self.find('needle')[0]['snippet']
        self.assertLessEqual(len(snippet), fulltext.SNIPPET_CHARS + 2)
        self.assertIn('NEEDLE', snippet)
        self.assertFalse(set(snippet) & set('\n\r\x1b‮​  '))
        self.assertEqual(snippet, ' '.join(snippet.split()))
        self.assertTrue(snippet.startswith('…') and snippet.endswith('…'))
        self.assertNotIn('<', self.find('start')[0]['snippet'], 'no highlight markup is added')
        self.assertIsNotNone(issue)


class FilterAndValidationTest(SearchBase):
    def test_filters(self):
        a = self.issue(A, title='report one', assignee='ag_a', labels=['bug'])
        self.now += 86400 * 3
        b = self.issue(B, title='report two', assignee='ag_b', status='note')
        self.comment(b, 'a report comment', H)
        self.assertEqual(set(self.ids('report')), {a, b})
        self.assertEqual(self.ids('report', status='note'), [b])
        self.assertEqual(self.ids('report', assignee='ag_a'), [a])
        self.assertEqual(self.ids('report', label='BUG'), [a])
        self.assertEqual({(r['issue']['id'], r['match']['author']) for r in self.find('report', author='usr_h')}, {(b, 'usr_h')})
        day = '2027-01-16'
        self.assertEqual(set(self.ids('report', **{'from': day})), {b})
        self.assertEqual(set(self.ids('report', to=day)), {a})
        self.assertEqual(set(self.ids('report', **{'from': '2027-01-01T00:00', 'to': '2027-12-31'})), {a, b})

    def test_validation(self):
        self.issue(A, title='x')
        for params in ({}, {'q': ''}, {'q': '   '}, {'q': 'a ' * 9}, {'q': 'x' * 201}, {'q': 5}, {'q': 'x', 'limit': 0}, {'q': 'x', 'limit': 51},
                       {'q': 'x', 'limit': 'many'}, {'q': 'x', 'status': 'bogus'}, {'q': 'x', 'from': 'yesterday'}, {'q': 'x', 'to': '2027-13-45'},
                       {'q': 'x', 'from': '2027-02-02', 'to': '2027-01-01'}, {'q': 'x', 'bogus': '1'}, {'q': 'x', 'author': ''}, {'q': 'x', 'label': 5}):
            with self.assertRaises(Error) as caught:
                self.store.search(params)
            self.assertIn(caught.exception.status, (400,), params)

    def test_result_cap_and_ranking(self):
        for n in range(30):
            self.issue(A, title=f'common word number {n}')
        best = self.issue(A, title='needle needle needle needle')
        self.issue(A, title='a longer title that mentions the needle once among many other words')
        self.assertEqual(self.find('needle')[0]['issue']['id'], best, 'bm25 ranks the denser match first')
        self.assertEqual(len(self.find('common', limit=7)), 7)
        self.assertLessEqual(len(self.find('common')), 20)


class IndexTest(SearchBase):
    def count(self):
        with sqlite3.connect(self.path) as db:
            return db.execute('SELECT COUNT(*) FROM search_docs').fetchone()[0], db.execute('SELECT COUNT(*) FROM search_fts').fetchone()[0]

    def test_rebuild_is_idempotent_and_equals_the_incremental_index(self):
        one = self.issue(A, title='Alpha issue', description='about parsers')
        self.comment(one, 'discussion of parsers', B, title='[decision] parsers')
        self.edit(one, {'title': 'Alpha renamed'}, H)
        before = [(r['issue']['id'], r['match']['kind'], r['snippet']) for r in self.find('parsers')]
        counts = self.count()
        with sqlite3.connect(self.path) as db:
            db.execute('DELETE FROM search_meta')
        again = Store(self.path, clock=lambda: self.now)
        self.assertTrue(again.search_ready)
        self.assertEqual(self.count(), counts)
        self.assertEqual([(r['issue']['id'], r['match']['kind'], r['snippet']) for r in again.search(dict(q='parsers'))['results']], before)
        Store(self.path, clock=lambda: self.now)
        self.assertEqual(self.count(), counts, 'a second start changes nothing')

    def test_a_rebuild_that_runs_out_of_time_commits_nothing_and_search_answers_503(self):
        self.issue(A, title='x')
        with sqlite3.connect(self.path) as db:
            db.execute('DELETE FROM search_meta')
        original = fulltext.REBUILD_SECONDS
        fulltext.REBUILD_SECONDS = -1
        try:
            slow = Store(self.path, clock=lambda: self.now)
        finally:
            fulltext.REBUILD_SECONDS = original
        self.assertFalse(slow.search_ready)
        with self.assertRaises(Error) as caught:
            slow.search(dict(q='xyz'))
        self.assertEqual(caught.exception.status, 503)
        self.assertTrue(Store(self.path, clock=lambda: self.now).search_ready, 'the next start finishes the job')


class RateLimitTest(SearchBase):
    def test_per_principal_limit(self):
        app = Application(self.store)
        for _ in range(app.SEARCH_PER_MINUTE):
            app.search_allowed('ag_a', clock=lambda: 1000.0)
        with self.assertRaises(Error) as caught:
            app.search_allowed('ag_a', clock=lambda: 1001.0)
        self.assertEqual(caught.exception.status, 429)
        app.search_allowed('ag_b', clock=lambda: 1001.0)           # other principals are unaffected
        app.search_allowed('ag_a', clock=lambda: 1061.0)           # the window slides


if __name__ == '__main__':
    unittest.main()
