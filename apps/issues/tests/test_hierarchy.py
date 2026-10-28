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
import hierarchy
from auth import Error
from store import Store

PEOPLE = [{'id': 'ag_a', 'kind': 'agent', 'name': 'a'}, {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}, {'id': 'ag_c', 'kind': 'agent', 'name': 'c'},
          {'id': 'usr_h', 'kind': 'human', 'name': 'h'}]
A = {'id': 'ag_a', 'kind': 'agent', 'name': 'a'}
B = {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}
C = {'id': 'ag_c', 'kind': 'agent', 'name': 'c'}
H = {'id': 'usr_h', 'kind': 'human', 'name': 'h', 'role': 'admin'}
T0 = 1_800_000_000.0


class Base(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-hier-')
        self.path = Path(self.directory.name) / 'issues.sqlite3'
        self.now = T0
        self.store = Store(self.path, clock=lambda: self.now)

    def tearDown(self):
        self.directory.cleanup()

    def issue(self, actor=A, **values):
        self.now += 1
        return self.store.mutate(None, 'create', {'title': 'Untitled', **values}, actor, PEOPLE)[0]

    def edit(self, issue, body, actor=A):
        self.now += 1
        return self.store.mutate(issue, 'update', body, actor, PEOPLE)

    def link(self, parent, children, actor=A):
        self.now += 1
        return self.store.link_children(parent, {'add': children}, actor)

    def unlink(self, parent, children, actor=A):
        self.now += 1
        return self.store.link_children(parent, {'remove': children}, actor)

    def parents(self, issue):
        return self.store.show(issue)['issue']['parent_ids']

    def code(self, call, *args, **kwargs):
        with self.assertRaises(Error) as caught:
            call(*args, **kwargs)
        return caught.exception.status, caught.exception.code


class MigrationTest(Base):
    def test_single_parent_id_moves_into_the_relation_table_losslessly_and_idempotently(self):
        root, mid, leaf, orphan = self.issue(), self.issue(), self.issue(), self.issue()
        with sqlite3.connect(self.path) as db:               # the storage format before this version: parent_id inside the JSON
            for child, parent in ((mid, root), (leaf, mid), (orphan, 9999), (root, root)):
                data = json.loads(db.execute('SELECT data FROM issues WHERE id=?', (child,)).fetchone()[0]); data['parent_id'] = parent
                db.execute('UPDATE issues SET data=? WHERE id=?', (json.dumps(data), child))
            db.execute('DELETE FROM issue_parents'); db.execute('DELETE FROM hierarchy_meta')
        again = Store(self.path, clock=lambda: self.now)
        self.assertEqual([again.show(i)['issue']['parent_ids'] for i in (root, mid, leaf, orphan)], [[], [root], [mid], []], 'dangling and self parents are dropped')
        with sqlite3.connect(self.path) as db:
            stored = [json.loads(r[0]) for r in db.execute('SELECT data FROM issues')]
            self.assertTrue(all('parent_id' not in d for d in stored), 'parent_id is no longer stored: it is derived')
            self.assertEqual(db.execute('SELECT COUNT(*) FROM issue_parents').fetchone()[0], 2)
        Store(self.path, clock=lambda: self.now)
        with sqlite3.connect(self.path) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM issue_parents').fetchone()[0], 2, 'a second start changes nothing')
        self.assertEqual(again.show(leaf)['issue']['parent_id'], mid)


class LinkTest(Base):
    def test_attach_existing_issues_leaves_a_trace_in_both_and_changes_nothing_else_about_the_child(self):
        epic, child = self.issue(A, title='Summary epic'), self.issue(B, title='Existing work', assignee='ag_b')
        before = self.store.show(child)['issue']
        shown = self.link(epic, [child], A)
        self.assertEqual(self.parents(child), [epic])
        self.assertEqual([c['id'] for c in shown['children']], [child])
        after = self.store.show(child)['issue']
        self.assertEqual((before['version'], before['updated_at'], before['status'], before['assignee']), (after['version'], after['updated_at'], after['status'], after['assignee']))
        trace = lambda i, kind: [e for e in self.store.show(i)['activity'] if e['kind'] == kind]
        self.assertEqual((trace(epic, 'link')[0]['actor'], trace(epic, 'link')[0]['changes']), ('ag_a', {'child': child}))
        self.assertEqual((trace(child, 'link')[0]['actor'], trace(child, 'link')[0]['changes']), ('ag_a', {'parent': epic}), 'the child side sees who attached it, and where')
        self.link(epic, [child], A)                                                      # idempotent
        self.assertEqual(len(trace(epic, 'link')), 1)
        self.unlink(epic, [child], B)
        self.assertEqual(self.parents(child), [])
        self.assertEqual(len(trace(child, 'unlink')), 1); self.assertEqual(len(trace(epic, 'unlink')), 1)

    def test_one_issue_can_be_a_child_of_several_epics_and_the_old_api_still_works(self):
        e1, e2, child = self.issue(A), self.issue(A), self.issue(A)
        self.link(e1, [child]); self.link(e2, [child])
        issue = self.store.show(child)['issue']
        self.assertEqual((issue['parent_ids'], issue['parent_id']), ([e1, e2], e1), 'parent_id is the earliest parent, derived')
        for parent in (e1, e2):
            self.assertEqual([i['id'] for i in self.store.list({'parent_id': str(parent)})['issues']], [child])
        self.edit(child, {'parent_id': e2})                                              # legacy: replaces the set
        self.assertEqual(self.parents(child), [e2])
        self.edit(child, {'parent_ids': [e1, e2]})
        self.assertEqual(sorted(self.parents(child)), [e1, e2])
        self.assertEqual(self.store.show(child)['issue']['parent_id'], e2, 'the link that has existed longest comes first')
        self.edit(child, {'parent_id': None})
        self.assertEqual(self.parents(child), [])
        made = self.issue(A, parent_ids=[e1, e2]); self.assertEqual(sorted(self.parents(made)), [e1, e2])
        self.assertEqual(self.parents(self.issue(A, parent_id=e1)), [e1])
        self.assertEqual(self.code(self.edit, child, {'parent_id': e1, 'parent_ids': [e1]})[0], 400)
        for bad in ([0], ['x'], [e1, e1], 'nope', [999999]):
            self.assertIn(self.code(self.edit, child, {'parent_ids': bad})[0], (400, 404), bad)

    def test_anyone_may_link_and_unlink(self):
        mine, theirs = self.issue(A), self.issue(B)
        child = self.issue(C, assignee='ag_c')
        self.link(theirs, [child], A)                    # no ownership: any principal arranges any hierarchy
        self.unlink(theirs, [child], B)
        self.link(mine, [child], A); self.unlink(mine, [child], H)
        self.assertEqual(self.parents(child), [])
        for body in ({}, {'add': []}, {'add': ['x']}, {'add': [0]}, {'bogus': 1}, {'add': list(range(1, 60))}):
            self.assertEqual(self.code(self.store.link_children, mine, body, A)[0], 400, body)
        self.assertEqual(self.code(self.link, 99999, [child], A)[0], 404)
        self.assertEqual(self.code(self.link, mine, [99999], A)[0], 404)

    def test_relations_leave_other_fields_alone(self):
        a = self.issue(A, assignee='ag_a')
        epic = self.issue(B, assignee='ag_b')
        self.link(epic, [a], B)
        shown = self.store.show(a)['issue']
        self.assertEqual((shown['assignee'], shown['blocked_by'], shown['status']), ('ag_a', [], 'open'))
        dep = self.issue(A)
        self.edit(a, {'blocked_by': [dep]}, A)
        self.assertEqual(self.store.show(a)['issue']['blocked_by'], [dep])


class GraphTest(Base):
    def test_cycles_are_refused_across_the_whole_dag(self):
        a, b, c, d = self.issue(), self.issue(), self.issue(), self.issue()
        self.link(a, [b]); self.link(b, [c]); self.link(a, [d]); self.link(d, [c])      # a diamond is fine
        for parent, child in ((c, a), (b, a), (a, a), (c, b), (d, a)):
            self.assertEqual(self.code(self.link, parent, [child])[1], 'hierarchy_cycle', (parent, child))
        self.assertEqual(self.code(self.edit, a, {'parent_ids': [c]})[1], 'hierarchy_cycle')

    def test_rollups_count_each_descendant_once_and_skip_notes(self):
        root, x, y, leaf, done, note = (self.issue() for _ in range(6))
        for parent, child in ((root, x), (root, y), (x, leaf), (y, leaf), (x, done), (root, note)):
            self.link(parent, [child])
        self.edit(done, {'status': 'done'}); self.edit(note, {'status': 'note'})
        progress = self.store.show(root)['issue']['progress']
        self.assertEqual((progress['total'], progress['completed'], progress['truncated']), (4, 1, False), 'x, y, leaf (once) and done; the note is not counted')
        self.assertEqual(self.store.show(x)['issue']['progress']['total'], 2)

    def test_limits_keep_the_graph_bounded(self):
        child = self.issue()
        parents = [self.issue() for _ in range(hierarchy.MAX_PARENTS + 2)]
        for p in parents[:hierarchy.MAX_PARENTS]:
            self.store.link_hits.clear()
            self.link(p, [child])
        self.assertEqual(self.code(self.link, parents[-1], [child])[1], 'too_many_parents', 'a child holds at most 50 parents')
        self.edit(child, {'parent_ids': [parents[-1]]})                                      # the child's owner can still replace the set
        self.assertEqual(self.parents(child), [parents[-1]])
        self.unlink(parents[-1], [child])
        busy = self.issue()
        kids = [self.issue() for _ in range(hierarchy.MAX_CHILDREN + 1)]
        for k in kids[:hierarchy.MAX_CHILDREN]:
            self.store.link_hits.clear()
            self.link(busy, [k])
        self.assertEqual(self.code(self.link, busy, [kids[-1]])[1], 'too_many_children')
        self.unlink(busy, [kids[0]])
        self.link(busy, [kids[-1]])

    def test_walks_are_bounded(self):
        a = self.issue()
        original, hierarchy.MAX_NODES = hierarchy.MAX_NODES, 3
        try:
            chain = [self.issue() for _ in range(6)]
            for parent, child in zip(chain, chain[1:]):
                self.store.link_hits.clear()
                self.link(parent, [child])
            self.assertTrue(self.store.show(chain[0])['issue']['progress']['truncated'], 'a rollup stops at the node cap and says so')
            self.assertEqual(self.code(self.link, a, [chain[0]]), (409, 'hierarchy_too_large'), 'a cycle check never walks an unbounded graph')
        finally:
            hierarchy.MAX_NODES = original

    def test_relation_changes_are_rate_limited_per_principal(self):
        epic = self.issue(A)
        kids = [self.issue(B) for _ in range(Store.LINKS_PER_MINUTE + 1)]
        self.store.link_hits.clear()
        for k in kids[:Store.LINKS_PER_MINUTE]:
            self.link(epic, [k], A)
        self.assertEqual(self.code(self.link, epic, [kids[-1]], A)[0], 429)
        self.link(self.issue(B), [kids[0]], B)                       # another principal is unaffected
        self.now += 61
        self.link(epic, [kids[-1]], A)                              # the window slides


class WeightedRateTest(Base):
    def test_one_request_changing_many_links_costs_that_many(self):
        epic = self.issue(A)
        kids = [self.issue(B) for _ in range(Store.LINKS_PER_MINUTE + 5)]
        self.store.link_hits.clear()
        self.link(epic, kids[:Store.LINKS_PER_MINUTE - 5], A)                      # 25 links in one request
        self.assertEqual(self.code(self.link, epic, kids[25:31], A)[0], 429, '6 more would be 31 in the minute')
        self.link(epic, kids[25:30], A)                                            # exactly the remaining 5
        self.assertEqual(self.code(self.link, epic, [kids[30]], A)[0], 429)
        self.now += 61
        self.link(epic, [kids[30]], A)
        spread = self.issue(A)
        self.store.link_hits.clear()
        self.edit(spread, {'parent_ids': [epic]}, A)
        self.assertEqual(len(self.store.link_hits['ag_a']), 1, 'a PATCH counts the links it adds')
        self.edit(spread, {'parent_ids': []}, A)
        self.assertEqual(len(self.store.link_hits['ag_a']), 1, 'removing links is free: a full set can always be cleared')


if __name__ == '__main__':
    unittest.main()
