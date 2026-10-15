import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from hermetic import hermetic_env

ROOT = Path(__file__).resolve().parents[1]
clean = hermetic_env(); os.environ.clear(); os.environ.update(clean)
sys.path.insert(0, str(ROOT))
from auth import Error
from issues import Application
from notify import WINDOW, Digest, digest_body, enabled, resolve_mentions
from store import Store

DIRECTORY = [
    {'id': 'ag_author', 'kind': 'agent', 'name': 'author', 'path': '/dev/author'},
    {'id': 'ag_work', 'kind': 'agent', 'name': 'worker', 'path': '/dev/worker'},
    {'id': 'ag_rev', 'kind': 'agent', 'name': 'Reviewer', 'path': '/sec/Reviewer'},
    {'id': 'ag_rev2', 'kind': 'agent', 'name': 'reviewer', 'path': '/ops/reviewer'},
    {'id': 'usr_lead', 'kind': 'human', 'name': 'leader', 'role': 'admin'},
]
LEAD = {'id': 'usr_lead', 'kind': 'human', 'role': 'admin', 'name': 'leader'}
AUTHOR = {'id': 'ag_author', 'kind': 'agent', 'name': 'author'}
WORKER = {'id': 'ag_work', 'kind': 'agent', 'name': 'worker'}
T0 = 1_800_000_000.0


class FakeAuth:
    service_id, public_url = 'svc_issues', 'https://issues.example/'
    def __init__(self):
        self.requests, self.fail = [], None
    def request(self, method, path, token, body=None):
        if self.fail:
            raise Error(503, self.fail, 'x')
        self.requests.append(body)
        return {}


class Base(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-notify-')
        self.now = T0
        self.store = Store(Path(self.directory.name) / 'issues.sqlite3', clock=lambda: self.now)
        self.auth = FakeAuth()
        self.app = Application(self.store, self.auth)
        os.environ.pop('ISSUES_NOTIFY', None)

    def tearDown(self):
        os.environ.pop('ISSUES_NOTIFY', None)
        self.directory.cleanup()

    def mutate(self, issue_id, action, body, actor):
        return self.app.mutation(issue_id, action, body, actor, DIRECTORY, 'token')

    def create(self, actor=LEAD, **values):
        return self.mutate(None, 'create', dict(title='Secret title', description='Secret body', **values), actor)['issue']['id']

    def last(self):
        return self.auth.requests[-1]


class MentionTest(unittest.TestCase):
    def test_resolution(self):
        r = lambda text: resolve_mentions(text, DIRECTORY)
        self.assertEqual(r('please look @author'), {'ag_author'})
        self.assertEqual(r('@worker, @AUTHOR.'), {'ag_work', 'ag_author'}, 'trailing punctuation and case')
        self.assertEqual(r('@/sec/Reviewer and @ag_rev2'), {'ag_rev', 'ag_rev2'}, 'path and stable ID disambiguate')
        self.assertEqual(r('@reviewer'), set(), 'ambiguous names are ignored, never guessed')
        self.assertEqual(r('mail me at a@author.example or foo@author'), set(), 'e-mail style text is not a mention')
        self.assertEqual(r('@leader @nobody @'), set(), 'humans and unknown names are not mentioned')
        self.assertEqual(r(''), set())


class RecipientTest(Base):
    def test_assignee_hears_everything_creator_only_status_assignee_and_mentions(self):
        id = self.create(assignee='ag_work')            # created by LEAD, assigned to worker
        self.assertEqual(self.last()['recipients'], ['ag_work'])
        self.now += WINDOW + 1
        self.mutate(id, 'comment', {'body': 'plain remark'}, AUTHOR)
        self.assertEqual(self.last()['recipients'], ['ag_work'], 'a plain comment reaches the assignee, not the creator')
        self.now += WINDOW + 1
        self.mutate(id, 'update', {'status': 'in_progress'}, WORKER)
        self.assertEqual(self.last()['recipients'], ['usr_lead'], 'creator hears status changes; the actor never hears themself')
        self.assertIn('Status open→in_progress · by worker', self.last()['body'])
        self.now += WINDOW + 1
        self.mutate(id, 'update', {'assignee': 'ag_author'}, LEAD)
        self.assertEqual(self.last()['recipients'], ['ag_author', 'ag_work'], 'old and new assignee, not the actor/creator')
        self.assertIn('Assigned to author', self.last()['body'])
        self.now += WINDOW + 1
        self.mutate(id, 'update', {'priority': 'high'}, LEAD)
        self.assertEqual(self.last()['recipients'], ['ag_author'], 'other edits go to the assignee only')
        self.assertIn('Updated priority', self.last()['body'])

    def test_mentions_reach_non_assignees_but_never_the_actor_and_only_when_new(self):
        id = self.create(assignee='ag_work')
        self.now += WINDOW + 1
        result = self.mutate(id, 'comment', {'body': 'ping @author and @worker and myself @leader'}, LEAD)
        self.assertEqual(self.last()['recipients'], ['ag_author', 'ag_work'])
        self.assertIn('mentions @author, @worker', self.last()['body'])
        self.assertEqual(result['activity'][0]['mentions'], ['ag_author', 'ag_work'])
        self.now += WINDOW + 1
        self.mutate(id, 'update', {'description': 'see @author'}, LEAD)
        n = len(self.auth.requests)
        self.now += WINDOW + 1
        self.mutate(id, 'update', {'description': 'see @author again, more words'}, LEAD)
        self.assertNotIn('ag_author', self.last()['recipients'], 'an @ that was already there is not new')
        self.assertEqual(len(self.auth.requests), n + 1)

    def test_no_op_updates_and_claims_do_not_notify(self):
        id = self.create(assignee='ag_work')
        n = len(self.auth.requests)
        result = self.mutate(id, 'update', {'status': 'open', 'assignee': 'ag_work'}, LEAD)
        self.assertEqual(result['notification']['status'], 'no_recipients')
        result = self.mutate(id, 'claim', {'action': 'acquire', 'seconds': 60}, WORKER)
        self.assertEqual(result['notification']['status'], 'not_requested')
        self.assertEqual(len(self.auth.requests), n)

    def test_messages_never_contain_titles_or_bodies(self):
        id = self.create(assignee='ag_work')
        self.mutate(id, 'comment', {'body': 'SECRET COMMENT'}, AUTHOR)
        for request in self.auth.requests:
            self.assertNotIn('Secret', request['body']); self.assertNotIn('SECRET', request['body'])
            self.assertIn(f'https://issues.example/#issue={id}', request['body'])


class WindowTest(Base):
    def test_first_event_is_immediate_then_coalesced_and_one_event_is_one_message(self):
        id = self.create(assignee='ag_work')
        self.assertEqual(len(self.auth.requests), 1)
        for text in ('a', 'b', 'c'):
            result = self.mutate(id, 'comment', {'body': text}, AUTHOR)
            self.assertEqual(result['notification']['status'], 'coalesced')
        self.assertEqual(len(self.auth.requests), 1, 'nothing is sent twice for an event, and nothing in the window')
        self.assertEqual(result['activity'][0]['notification']['status'], 'coalesced')

    def test_digest_sends_one_message_then_a_new_quiet_window_starts(self):
        id = self.create(assignee='ag_work')
        for text in ('a', 'b', 'c'):
            self.mutate(id, 'comment', {'body': text}, AUTHOR)
        sent, logs = [], []
        digest = Digest(self.store, lambda: 'https://issues.example/', clock=lambda: self.now, runner=lambda args: sent.append(args) or (0, ''), log=logs.append)
        self.now += WINDOW - 1
        self.assertEqual(digest.flush(), 0, 'not before the window ends')
        self.now += 2
        self.assertEqual(digest.flush(), 1)
        self.assertEqual(digest.flush(), 0, 'already delivered')
        (args,) = sent
        self.assertEqual(args[:4], ['message', 'send', '--to', 'ag_work'])
        body = args[args.index('--body') + 1]
        self.assertIn(f'Issue #{id} · 3 more updates', body)
        self.assertEqual(body.count('New comment · by author'), 1, 'identical summaries are de-duplicated')
        self.assertNotIn('Secret', body)
        self.assertTrue(args[args.index('--idempotency-key') + 1].startswith(f'issues-digest-ag_work-{id}-'))
        result = self.mutate(id, 'comment', {'body': 'd'}, AUTHOR)
        self.assertEqual(result['notification']['status'], 'coalesced', 'the digest opened a fresh quiet window')

    def test_event_after_a_quiet_window_is_immediate_again(self):
        id = self.create(assignee='ag_work')
        self.now += WINDOW + 1
        result = self.mutate(id, 'comment', {'body': 'later'}, AUTHOR)
        self.assertEqual(result['notification']['status'], 'sent')
        self.assertEqual(len(self.auth.requests), 2)

    def test_windows_are_per_recipient_and_per_issue(self):
        one = self.create(assignee='ag_work')
        two = self.create(assignee='ag_work')
        self.assertEqual(len(self.auth.requests), 2, 'another issue has its own window')
        self.mutate(one, 'comment', {'body': 'x @author'}, LEAD)       # worker coalesced, author immediate
        self.assertEqual(self.last()['recipients'], ['ag_author'])
        self.assertEqual(self.store.due_digests(self.now + WINDOW + 1)[0]['recipient'], 'ag_work')

    def test_failed_immediate_send_does_not_open_a_window(self):
        self.auth.fail = 'core_messages_disabled'
        id = self.create(assignee='ag_work')
        result = self.mutate(id, 'comment', {'body': 'x'}, AUTHOR)
        self.assertEqual(result['notification']['status'], 'disabled')
        self.auth.fail = None
        result = self.mutate(id, 'comment', {'body': 'y'}, AUTHOR)
        self.assertEqual(result['notification']['status'], 'sent', 'the next event retries immediately')
        self.auth.fail = 'treer_unavailable'
        result = self.mutate(id, 'comment', {'body': 'z'}, WORKER)     # creator? no: assignee actor excluded -> nobody
        self.assertEqual(result['notification']['status'], 'no_recipients')

    def test_digest_failure_is_retried_three_times_then_dropped(self):
        id = self.create(assignee='ag_work')
        self.mutate(id, 'comment', {'body': 'a'}, AUTHOR)
        logs = []
        digest = Digest(self.store, lambda: '', clock=lambda: self.now, runner=lambda args: (1, ''), log=logs.append)
        self.now += WINDOW + 1
        for _ in range(3):
            self.assertEqual(digest.flush(), 0)
        self.assertEqual(len(logs), 3)
        self.assertEqual(self.store.due_digests(self.now), [], 'dropped after the third failure')
        self.assertTrue(all('Secret' not in line for line in logs))

    def test_digest_pass_survives_unexpected_errors_and_old_windows_are_pruned(self):
        id = self.create(assignee='ag_work')
        self.mutate(id, 'comment', {'body': 'a'}, AUTHOR)
        logs = []
        digest = Digest(self.store, lambda: '', clock=lambda: self.now, runner=lambda args: 1 / 0, log=logs.append)
        self.now += WINDOW + 1
        digest.guarded()
        self.assertEqual(logs, ['Issues digest pass failed: ZeroDivisionError'])
        ok = Digest(self.store, lambda: '', clock=lambda: self.now, runner=lambda args: (0, ''), log=logs.append)
        ok.flush()
        self.now += WINDOW + 1
        ok.flush()
        with self.store.connect() as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM notify_windows').fetchone()[0], 0)

    def test_digest_body_shape(self):
        body = digest_body(7, [f'S{i}' for i in range(6)], 'https://x/')
        self.assertIn('6 more updates', body); self.assertIn('+2 more', body); self.assertTrue(body.endswith('https://x/#issue=7'))
        self.assertEqual(digest_body(7, ['a'], ''), 'Issue #7 · 1 more update in the last 5 minutes: a')


class SwitchTest(Base):
    def test_notify_off_sends_nothing_and_records_nothing(self):
        for value in ('0', 'off', 'False', ' no '):
            self.assertFalse(enabled({'ISSUES_NOTIFY': value}))
        self.assertTrue(enabled({})); self.assertTrue(enabled({'ISSUES_NOTIFY': 'on'}))
        os.environ['ISSUES_NOTIFY'] = 'off'
        id = self.create(assignee='ag_work')
        result = self.mutate(id, 'comment', {'body': 'hi @author'}, LEAD)
        self.assertEqual(result['notification']['status'], 'off')
        self.assertEqual(self.auth.requests, [])
        self.assertEqual(self.store.due_digests(self.now + 10 * WINDOW), [])
        os.environ.pop('ISSUES_NOTIFY')
        self.assertEqual(self.mutate(id, 'comment', {'body': 'again'}, LEAD)['notification']['status'], 'sent')


if __name__ == '__main__':
    unittest.main()
