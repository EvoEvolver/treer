import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from hermetic import hermetic_env

ROOT = Path(__file__).resolve().parents[1]
clean = hermetic_env(); os.environ.clear(); os.environ.update(clean)
sys.path.insert(0, str(ROOT))
from store import Store
import reminders
from reminders import Backoff, MAX_LISTED, POLL, Reminders, backoff, enabled, parse_duration, skip_labels

PEOPLE = [{'id': 'ag_a', 'kind': 'agent'}, {'id': 'ag_b', 'kind': 'agent'}, {'id': 'ag_c', 'kind': 'agent'},
          {'id': 'usr_h', 'kind': 'human'}]
ACTOR = {'id': 'usr_h', 'kind': 'human', 'role': 'admin', 'name': 'h'}
OTHER = {'id': 'ag_b', 'kind': 'agent', 'name': 'b'}
T0 = 1_800_000_000.0
MIN = 60.0
HOUR = 3600.0
FIRST = 10 * MIN
DEFAULT = Backoff(FIRST, 2.0, 24 * HOUR)


class FakeTreer:
    """Stands in for the CLI: records argv, answers `agent show` from a table, can fail sends."""
    def __init__(self, states=None):
        self.states = states or {}
        self.calls = []
        self.fail_send = set()

    def __call__(self, args):
        self.calls.append(list(args))
        if args[:2] == ['agent', 'show']:
            state = self.states.get(args[2])
            if state is None:
                return 1, ''
            if state == 'garbage':
                return 0, 'not json'
            return 0, json.dumps({'agent_id': args[2], 'status': state, 'busy': state != 'idle'})
        if args[:2] == ['message', 'send']:
            return (1, '') if args[args.index('--to') + 1] in self.fail_send else (0, '{}')
        raise AssertionError(args)

    def sends(self):
        return [c for c in self.calls if c[:2] == ['message', 'send']]

    def probes(self):
        return [c[2] for c in self.calls if c[:2] == ['agent', 'show']]

    def recipients(self):
        return [c[c.index('--to') + 1] for c in self.sends()]

    def body(self, index=-1):
        call = self.sends()[index]
        return call[call.index('--body') + 1]

    def key(self, index=-1):
        call = self.sends()[index]
        return call[call.index('--idempotency-key') + 1]


class ReminderBase(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-reminders-')
        self.now = T0
        self.path = Path(self.directory.name) / 'issues.sqlite3'
        self.store = Store(self.path, clock=lambda: self.now)
        self.logs = []

    def tearDown(self):
        self.directory.cleanup()

    def issue(self, **values):
        return self.store.mutate(None, 'create', dict(title='Secret <title>', **values), ACTOR, PEOPLE)[0]

    def act(self, issue_id, action, body, actor=ACTOR):
        return self.store.mutate(issue_id, action, body, actor, PEOPLE)

    def reminders(self, treer, schedule=DEFAULT, link=lambda: 'https://issues.example/', store=None, skip=None):
        return Reminders(store or self.store, link, clock=lambda: self.now, runner=treer, log=self.logs.append, schedule=schedule, skip=skip)

    def advance(self, seconds):
        self.now += seconds


class BackoffSequenceTest(ReminderBase):
    def test_first_reminder_after_ten_minutes_then_doubling_up_to_the_cap(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        self.assertEqual(r.tick(), {'sent': 0, 'skipped': 0, 'due': 0}, 'the poll that notices the issue only starts its clock')
        self.advance(FIRST - 1)
        self.assertEqual(r.tick()['sent'], 0, 'not before ten minutes')
        gaps, sent_at = [], []
        self.advance(1)
        for _ in range(12):
            self.assertEqual(r.tick()['sent'], 1)
            sent_at.append(self.now)
            interval = self.store.reminder_states()[1]['interval']
            gaps.append(interval)
            self.advance(interval - 1)
            self.assertEqual(r.tick()['sent'], 0, 'never a second before the interval is over')
            self.advance(1)
        expected = [20 * MIN, 40 * MIN, 80 * MIN, 160 * MIN, 320 * MIN, 640 * MIN, 1280 * MIN, 24 * HOUR, 24 * HOUR, 24 * HOUR, 24 * HOUR, 24 * HOUR]
        self.assertEqual(gaps, expected, '10m, 20m, 40m, 1h20m, 2h40m, 5h20m, 10h40m, 21h20m, then capped at 24h')
        self.assertEqual([round((b - a) / MIN) for a, b in zip(sent_at, sent_at[1:])][:8], [20, 40, 80, 160, 320, 640, 1280, 1440])
        self.assertEqual(len(treer.sends()), 12)

    def test_configurable_factor_first_and_max(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer, Backoff(2 * MIN, 1.5, 5 * MIN))
        r.tick()
        intervals = []
        for _ in range(4):
            self.advance(self.store.reminder_states()[1]['next_due'] - self.now)
            self.assertEqual(r.tick()['sent'], 1)
            intervals.append(self.store.reminder_states()[1]['interval'])
        self.assertEqual(intervals, [180, 270, 300, 300], '2m, then x1.5 each time, capped at 5m')

    def test_a_factor_of_one_keeps_a_constant_interval(self):
        self.issue(assignee='ag_a')
        r = self.reminders(FakeTreer({'ag_a': 'idle'}), Backoff(FIRST, 1.0, 24 * HOUR))
        r.tick()
        for _ in range(3):
            self.advance(FIRST)
            self.assertEqual(r.tick()['sent'], 1)
        self.assertEqual(self.store.reminder_states()[1]['interval'], FIRST)


class StateRulesTest(ReminderBase):
    def test_busy_assignees_get_nothing_and_the_interval_does_not_grow(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'working'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        for _ in range(5):
            self.assertEqual(r.tick(), {'sent': 0, 'skipped': 1, 'due': 1})
            self.advance(POLL)
        self.assertEqual(self.store.reminder_states()[1]['interval'], FIRST, 'a busy poll never grows the interval')
        treer.states['ag_a'] = 'idle'
        self.assertEqual(r.tick()['sent'], 1, 'the first idle poll after the due time sends')
        self.assertEqual(self.store.reminder_states()[1]['interval'], 20 * MIN)
        for state in ('garbage', None):                   # unknown or unreachable counts as not idle
            treer.states['ag_a'] = state
            self.advance(20 * MIN)
            self.assertEqual(r.tick()['sent'], 0)

    def test_activity_resets_the_sequence_and_a_new_assignee_starts_from_scratch(self):
        a = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'})
        r = self.reminders(treer)
        r.tick()
        for _ in range(3):
            self.advance(self.store.reminder_states()[a]['next_due'] - self.now)
            r.tick()
        self.assertEqual(self.store.reminder_states()[a]['count'], 3)
        grown = self.store.reminder_states()[a]['interval']
        self.assertEqual(grown, 80 * MIN)
        self.act(a, 'comment', {'body': 'any news?'}, OTHER)                 # real activity
        r.tick()
        state = self.store.reminder_states()[a]
        self.assertEqual((state['count'], state['interval'], state['next_due']), (0, FIRST, self.now + FIRST), 'comment: back to ten minutes')
        self.act(a, 'update', {'status': 'in_progress'})
        self.advance(5 * MIN)
        r.tick()
        self.assertEqual(self.store.reminder_states()[a]['next_due'], self.now - 5 * MIN + FIRST, 'a status change restarts the clock')
        self.advance(FIRST)
        r.tick()
        self.assertEqual(treer.recipients()[-1], 'ag_a')
        self.act(a, 'update', {'assignee': 'ag_b'})
        r.tick()
        state = self.store.reminder_states()[a]
        self.assertEqual((state['assignee'], state['count'], state['interval']), ('ag_b', 0, FIRST), 'the new assignee starts from scratch')
        self.assertEqual(len(self.store.reminder_states()), 1, 'the old assignee has no state left')
        sends_before = len(treer.sends())
        self.advance(FIRST - 1)
        r.tick()
        self.assertEqual(len(treer.sends()), sends_before)
        self.advance(1)
        r.tick()
        self.assertEqual(treer.recipients()[-1], 'ag_b')

    def test_issues_that_stop_needing_a_reminder_lose_their_state_and_restart_when_they_return(self):
        a = self.issue(assignee='ag_a', labels=['x'])
        self.issue(assignee='ag_a', labels=['epic'])          # skipped
        self.issue(assignee='usr_h')                          # a human is never reminded
        self.issue()                                          # unassigned
        self.issue(assignee='ag_a', status='done')
        self.issue(assignee='ag_a', status='closed')
        r = self.reminders(FakeTreer({'ag_a': 'idle'}))
        r.tick()
        self.assertEqual(list(self.store.reminder_states()), [a], 'only the one issue that needs reminding has a schedule')
        self.act(a, 'update', {'status': 'done'})
        r.tick()
        self.assertEqual(self.store.reminder_states(), {})
        self.advance(HOUR)
        self.act(a, 'update', {'status': 'open'})
        r.tick()
        self.assertEqual(self.store.reminder_states()[a]['next_due'], self.now + FIRST, 'reopened: a fresh first interval')
        self.act(a, 'update', {'labels': ['EPIC']})
        r.tick()
        self.assertEqual(self.store.reminder_states(), {}, 'labels are matched case-insensitively and applied at once')

    def test_a_fresh_deployment_reminds_nobody_at_once(self):
        for i in range(30):
            self.issue(assignee=['ag_a', 'ag_b', 'ag_c'][i % 3])
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle', 'ag_c': 'idle'})
        r = self.reminders(treer)
        self.assertEqual(r.tick(), {'sent': 0, 'skipped': 0, 'due': 0})
        self.assertEqual(treer.calls, [], 'no probe, no message')
        self.assertEqual({s['next_due'] for s in self.store.reminder_states().values()}, {self.now + FIRST})

    def test_the_schedule_survives_restarts(self):
        a = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        self.reminders(treer).tick()
        self.advance(FIRST)
        self.assertEqual(self.reminders(treer, store=Store(self.path, clock=lambda: self.now)).tick()['sent'], 1)
        reopened = Store(self.path, clock=lambda: self.now)
        self.advance(5 * MIN)
        self.assertEqual(self.reminders(treer, store=reopened).tick()['sent'], 0, 'still inside the 20 minute interval after a restart')
        self.assertEqual(reopened.reminder_states()[a]['count'], 1)
        self.advance(15 * MIN)
        self.assertEqual(self.reminders(treer, store=reopened).tick()['sent'], 1)


class AggregationAndDeliveryTest(ReminderBase):
    def test_everything_due_for_one_agent_is_one_message_with_independent_state(self):
        a1 = self.issue(assignee='ag_a')
        a2 = self.issue(assignee='ag_a', status='blocked')
        b1 = self.issue(assignee='ag_b')
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        self.assertEqual(r.tick(), {'sent': 2, 'skipped': 0, 'due': 3})
        self.assertEqual(sorted(treer.recipients()), ['ag_a', 'ag_b'])
        body = treer.body(0) if treer.recipients()[0] == 'ag_a' else treer.body(1)
        self.assertIn(f'#{a1}, #{a2}', body)
        self.assertIn('2 unresolved issues', body)
        self.assertNotIn('Secret', body, 'titles and descriptions never leave the App')
        self.assertIn('https://issues.example/', body)
        self.act(a2, 'comment', {'body': 'ping'}, OTHER)                       # only a2 resets, counted from this moment
        self.advance(10 * MIN)
        r.tick()                                                               # a2 is due again (10m after its activity), a1 is not (20m)
        states = self.store.reminder_states()
        self.assertEqual((states[a1]['count'], states[a1]['interval']), (1, 20 * MIN), 'a1 keeps its own sequence')
        self.assertEqual((states[a2]['count'], states[a2]['interval']), (1, 20 * MIN), 'a2 restarted: first reminder after the reset, then doubled')
        self.assertEqual(treer.body(-1).count('#'), 1, 'only a2 was due, so only a2 is named')
        self.assertIn(f'#{a2}', treer.body(-1))
        self.advance(10 * MIN)
        r.tick()                                                               # a1 due (20m after its reminder), a2 not yet (20m)
        to_a = [c[c.index('--body') + 1] for c in treer.sends() if c[c.index('--to') + 1] == 'ag_a'][-1]
        self.assertEqual(to_a.count('#'), 1)
        self.assertIn(f'#{a1}', to_a)
        self.assertEqual(self.store.reminder_states()[b1]['count'], 2)

    def test_long_lists_are_capped_and_all_listed_issues_advance(self):
        ids = [self.issue(assignee='ag_a') for _ in range(MAX_LISTED + 3)]
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        r.tick()
        self.assertIn(f'{len(ids)} unresolved issues', treer.body())
        self.assertIn('(+3 more)', treer.body())
        self.assertNotIn(f'#{ids[-1]},', treer.body())
        self.assertEqual({s['count'] for s in self.store.reminder_states().values()}, {1})

    def test_failed_sends_are_not_recorded_and_retried_with_the_same_key(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        treer.fail_send.add('ag_a')
        self.assertEqual(r.tick(), {'sent': 0, 'skipped': 1, 'due': 1})
        self.assertEqual(self.store.reminder_states()[1]['count'], 0, 'a failure does not advance the backoff')
        self.assertEqual(len(self.logs), 1)
        self.assertNotIn('Secret', self.logs[0])
        treer.fail_send.clear()
        self.advance(POLL)
        self.assertEqual(r.tick()['sent'], 1)
        self.assertEqual(treer.key(0), treer.key(1), 'the retry reuses the idempotency key, so a delivered-but-unrecorded send is deduplicated by Core')

    def test_the_idempotency_key_covers_agent_issue_set_and_backoff_step(self):
        a = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        keys = []
        for _ in range(3):
            self.advance(self.store.reminder_states()[a]['next_due'] - self.now)
            r.tick()
            keys.append(treer.key())
        self.assertEqual(len(set(keys)), 3, 'every step has its own key')
        self.assertTrue(all(k.startswith('issues-reminder-ag_a-') for k in keys))
        rows = [{'issue_id': 1, 'version': 4, 'count': 2}]
        self.assertNotEqual(Reminders.key('ag_a', rows), Reminders.key('ag_a', rows + [{'issue_id': 2, 'version': 1, 'count': 0}]), 'a different issue set is a different key')
        self.assertNotEqual(Reminders.key('ag_a', rows), Reminders.key('ag_b', rows), 'a different agent is a different key')
        self.assertNotEqual(Reminders.key('ag_a', rows), Reminders.key('ag_a', [{'issue_id': 1, 'version': 4, 'count': 3}]), 'a different step is a different key')
        self.assertEqual(Reminders.key('ag_a', rows), Reminders.key('ag_a', [{'issue_id': 1, 'version': 4, 'count': 2, 'assignee': 'ag_a', 'interval': 9}]), 'only issue, version and step matter')
        self.assertNotEqual(Reminders.key('ag_a', rows), Reminders.key('ag_a', [{'issue_id': 1, 'version': 5, 'count': 2}]), 'a new version (after activity) is a different key')

    def test_only_agents_with_due_issues_are_probed(self):
        self.issue(assignee='ag_a')
        self.issue(assignee='ag_b')
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(5 * MIN)
        r.tick()
        self.assertEqual(treer.probes(), [], 'nothing due, so no CLI call at all')
        self.advance(5 * MIN)
        r.tick()
        self.assertEqual(sorted(treer.probes()), ['ag_a', 'ag_b'])

    def test_link_is_resolved_once_and_only_when_sending(self):
        self.issue(assignee='ag_a'); self.issue(assignee='ag_b')
        calls = []
        r = self.reminders(FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'}), link=lambda: calls.append(1) or 'http://x/')
        r.tick()
        self.assertEqual(calls, [])
        self.advance(FIRST)
        r.tick()
        self.assertEqual(len(calls), 1)

    def test_skip_labels_default_and_configuration(self):
        self.assertEqual(skip_labels({}), ('epic',))
        self.assertEqual(skip_labels({'ISSUES_REMINDER_SKIP_LABELS': ' epic, wip ,,epic'}), ('epic', 'wip'))
        self.assertEqual(skip_labels({'ISSUES_REMINDER_SKIP_LABELS': ''}), ())
        self.issue(assignee='ag_a', labels=['wip'])
        r = self.reminders(FakeTreer({'ag_a': 'idle'}), skip=('wip',))
        r.tick()
        self.assertEqual(self.store.reminder_states(), {})
        r = self.reminders(FakeTreer({'ag_a': 'idle'}), skip=())
        r.tick()
        self.assertEqual(len(self.store.reminder_states()), 1)


class SchedulerTest(ReminderBase):
    def drive(self, r, until):
        """Run the real loop with a fake clock; each wait advances the clock, waking slightly early on purpose."""
        waits = []

        def wait(seconds):
            waits.append(seconds)
            self.now += seconds - 0.2
            if self.now > until:
                r.close()
                return True
            return False
        r.loop(wait)
        return waits

    def test_polls_at_least_every_minute_and_reminds_at_the_due_time(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        waits = self.drive(r, T0 + 35 * MIN)
        self.assertLessEqual(max(waits), POLL, 'the poll granularity is a minute, not an hour')
        self.assertEqual(len(treer.sends()), 2, 'reminders at ~10m and ~30m')

    def test_a_busy_agent_with_an_overdue_issue_is_probed_at_most_once_a_minute(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'working'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)                                    # due, and the Agent stays busy for the next ten minutes
        waits = self.drive(r, T0 + FIRST + 10 * MIN)
        self.assertGreaterEqual(min(waits), POLL - 1, 'an overdue row never makes the scheduler wake every second')
        self.assertLessEqual(len(treer.probes()), 12, 'about one probe per minute, not one per second')
        self.assertEqual(self.store.reminder_states()[1]['count'], 0)

    def test_other_wakeups_do_not_re_probe_a_busy_agent_before_the_minute_is_up(self):
        busy = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'working', 'ag_b': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        r.tick()
        self.assertEqual(treer.probes().count('ag_a'), 1)
        self.advance(5)
        r.tick()                                               # a wake-up for some other reason
        self.assertEqual(treer.probes().count('ag_a'), 1, 'no second probe inside the same minute')
        self.advance(POLL)
        treer.states['ag_a'] = 'idle'
        self.assertEqual(r.tick()['sent'], 1, 'it is looked at again once the minute has passed')

    def test_a_reset_issue_never_reuses_the_key_of_its_earlier_first_reminder(self):
        a = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        r.tick()
        first_key = treer.key()
        self.act(a, 'comment', {'body': 'activity'}, OTHER)    # resets the sequence to step 0 again
        r.tick()
        self.advance(FIRST)
        r.tick()
        self.assertEqual(len(treer.sends()), 2)
        self.assertNotEqual(treer.key(), first_key, 'the same issue at step 0 again must not collide with its earlier reminder')

    def test_a_crash_between_send_and_record_retries_with_the_same_key(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST)
        real = self.store.reminder_advance
        self.store.reminder_advance = lambda *a, **k: (_ for _ in ()).throw(RuntimeError('crash before recording'))
        with self.assertRaises(RuntimeError):
            r.tick()
        self.store.reminder_advance = real
        self.advance(POLL)
        r2 = self.reminders(treer)                             # a restarted process
        self.assertEqual(r2.tick()['sent'], 1)
        self.assertEqual(treer.key(0), treer.key(1), 'the unrecorded send is retried under the same key')

    def test_an_idle_system_still_polls_and_a_failing_poll_does_not_kill_the_loop(self):
        r = self.reminders(lambda args: 1 / 0)
        self.issue(assignee='ag_a')
        self.advance(FIRST)
        r.store.reminder_sync(r.store.reminder_candidates(), self.now - FIRST, FIRST)
        waits = self.drive(r, T0 + 3 * POLL + FIRST)
        self.assertGreaterEqual(len(waits), 3)
        self.assertTrue(self.logs and all(line == 'Issues reminder poll failed: ZeroDivisionError' for line in self.logs))

    def test_early_wakeups_never_send_before_the_due_time(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        r = self.reminders(treer)
        r.tick()
        self.advance(FIRST - 0.5)
        self.assertEqual(r.tick()['sent'], 0)


class ConfigurationTest(unittest.TestCase):
    def test_durations(self):
        for text, seconds in (('10m', 600), ('90s', 90), ('1.5h', 5400), ('24h', 86400), (' 2d ', 172800)):
            self.assertEqual(parse_duration(text, 'x'), seconds)
        for bad in ('', '10', 'm', '-5m', '1x', '1e3s', 'ten minutes', None, '10 m s'):
            with self.assertRaises(ValueError, msg=repr(bad)):
                parse_duration(bad, 'x')

    def test_defaults_and_overrides(self):
        self.assertEqual(backoff({}), Backoff(600.0, 2.0, 86400.0))
        self.assertEqual(backoff({'ISSUES_REMINDER_FIRST': '5m', 'ISSUES_REMINDER_FACTOR': '1.5', 'ISSUES_REMINDER_MAX': '12h'}), Backoff(300.0, 1.5, 43200.0))

    def test_invalid_values_are_errors_not_silent_defaults(self):
        for key, value in (('ISSUES_REMINDER_FIRST', '30s'), ('ISSUES_REMINDER_FIRST', '2d'), ('ISSUES_REMINDER_FIRST', '0m'), ('ISSUES_REMINDER_FIRST', 'soon'),
                           ('ISSUES_REMINDER_FACTOR', '0.5'), ('ISSUES_REMINDER_FACTOR', '11'), ('ISSUES_REMINDER_FACTOR', 'nan'), ('ISSUES_REMINDER_FACTOR', 'inf'),
                           ('ISSUES_REMINDER_FACTOR', 'abc'), ('ISSUES_REMINDER_FACTOR', ''), ('ISSUES_REMINDER_MAX', '5m'), ('ISSUES_REMINDER_MAX', '31d'),
                           ('ISSUES_REMINDER_MAX', '0s'), ('ISSUES_REMINDER_MAX', 'x')):
            with self.assertRaises(ValueError, msg=(key, value)):
                backoff({key: value})
        with self.assertRaises(ValueError):
            backoff({'ISSUES_REMINDER_FIRST': '1h', 'ISSUES_REMINDER_MAX': '30m'})      # MAX below FIRST

    def test_the_switch(self):
        for value in ('0', 'off', 'False', ' no '):
            self.assertFalse(enabled({'ISSUES_REMINDERS': value}))
        for environ in ({}, {'ISSUES_REMINDERS': 'on'}, {'ISSUES_REMINDERS': '1'}):
            self.assertTrue(enabled(environ))

    def test_the_server_refuses_to_start_with_invalid_settings_before_any_side_effect(self):
        import subprocess
        env = hermetic_env({'ISSUES_PROXY_URL': 'http://127.0.0.1:9/', 'ISSUES_LISTEN': '127.0.0.1:0', 'ISSUES_REMINDER_FACTOR': '99'})
        with tempfile.TemporaryDirectory() as directory:
            env['ISSUES_DB'] = str(Path(directory) / 'x.sqlite3')
            result = subprocess.run([sys.executable, str(ROOT / 'issues.py')], env=env, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('ISSUES_REMINDER_FACTOR', result.stderr)
            self.assertNotIn('listening', result.stdout, 'it never started serving')


class RealRunnerTest(ReminderBase):
    def test_real_subprocess_runner_uses_the_cli_contract_and_logs_no_output(self):
        log = Path(self.directory.name) / 'argv.jsonl'
        script = Path(self.directory.name) / 'treer'
        script.write_text(f'''#!{sys.executable}
import json, sys
open({str(log)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1:3] == ['agent', 'show']:
    print(json.dumps({{'agent_id': sys.argv[3], 'status': 'idle', 'busy': False}}))
else:
    print('SECRET-OUTPUT'); sys.stderr.write('SECRET-ERROR')
''')
        script.chmod(script.stat().st_mode | stat.S_IXUSR)
        self.issue(assignee='ag_a')
        r = Reminders(self.store, lambda: 'http://issues.treer/', str(script), clock=lambda: self.now, log=self.logs.append, schedule=DEFAULT)
        r.tick()
        self.advance(FIRST)
        self.assertEqual(r.tick()['sent'], 1)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(calls[0], ['agent', 'show', 'ag_a'])
        self.assertEqual(calls[1][:4], ['message', 'send', '--to', 'ag_a'])
        self.assertEqual(self.logs, [])
        missing = Reminders(self.store, lambda: '', str(script) + '-missing', clock=lambda: self.now + 2 * HOUR, log=self.logs.append, schedule=DEFAULT)
        self.assertEqual(missing.tick()['sent'], 0)


if __name__ == '__main__':
    unittest.main()
