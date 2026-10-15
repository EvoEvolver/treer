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
from reminders import Reminders, CATCH_UP, HOUR, MAX_LISTED, enabled, next_boundary, slot_of

PEOPLE = [{'id': 'ag_a', 'kind': 'agent'}, {'id': 'ag_b', 'kind': 'agent'}, {'id': 'ag_c', 'kind': 'agent'},
          {'id': 'usr_h', 'kind': 'human'}]
ACTOR = {'id': 'usr_h', 'kind': 'human', 'role': 'admin'}
T0 = 1_800_000_000 // HOUR * HOUR   # an exact top of the hour


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


class ReminderTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='issues-reminders-')
        self.now = T0 + 5
        self.store = Store(Path(self.directory.name) / 'issues.sqlite3', clock=lambda: self.now)
        self.logs = []

    def tearDown(self):
        self.directory.cleanup()

    def issue(self, **values):
        return self.store.mutate(None, 'create', dict(title='Secret <title>', **values), ACTOR, PEOPLE)[0]

    def reminders(self, treer, link=lambda: 'https://issues.example/'):
        return Reminders(self.store, link, clock=lambda: self.now, runner=treer, log=self.logs.append)

    def test_reminds_only_idle_agents_with_unresolved_issues(self):
        a1 = self.issue(assignee='ag_a')
        a2 = self.issue(assignee='ag_a', status='blocked')
        self.issue(assignee='ag_a', status='done')           # resolved: never counted
        self.issue(assignee='ag_a', status='closed')
        self.issue(assignee='ag_b')                          # assignee busy
        self.issue(assignee='ag_c')                          # probe fails
        self.issue(assignee='usr_h')                         # a human is never probed or messaged
        self.issue()                                         # unassigned
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'working'})
        result = self.reminders(treer).tick()
        self.assertEqual((result['sent'], result['skipped']), (1, 2))
        probed = sorted(c[2] for c in treer.calls if c[:2] == ['agent', 'show'])
        self.assertEqual(probed, ['ag_a', 'ag_b', 'ag_c'])
        (send,) = treer.sends()
        self.assertEqual(send[send.index('--to') + 1], 'ag_a')
        body = send[send.index('--body') + 1]
        self.assertIn(f'#{a1}, #{a2}', body)
        self.assertIn('2 unresolved issues', body)
        self.assertIn('https://issues.example/', body)
        self.assertNotIn('Secret', body, 'titles and descriptions never leave the App')
        self.assertEqual(send[send.index('--idempotency-key') + 1], f'issues-reminder-ag_a-{slot_of(self.now)}')

    def test_exactly_one_message_per_agent_per_hour_even_across_restarts(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        self.reminders(treer).tick()
        self.assertEqual(len(treer.sends()), 1)
        self.now += 600
        self.reminders(treer).tick()                         # a "restarted" instance, same database
        self.reminders(treer).tick()
        self.assertEqual(len(treer.sends()), 1)
        self.now = T0 + HOUR + 1                             # next hour: reminded again
        self.reminders(treer).tick()
        self.assertEqual(len(treer.sends()), 2)
        self.assertNotEqual(treer.sends()[0][-1], treer.sends()[1][-1], 'a new hour uses a new idempotency key')

    def test_failed_send_is_not_recorded_and_retried_in_the_same_hour(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle'})
        treer.fail_send.add('ag_a')
        self.assertEqual(self.reminders(treer).tick()['sent'], 0)
        self.assertEqual(len(self.logs), 1)
        self.assertNotIn('Secret', self.logs[0])
        treer.fail_send.clear()
        self.assertEqual(self.reminders(treer).tick()['sent'], 1)
        self.assertEqual(self.reminders(treer).tick()['sent'], 0)

    def test_resolving_or_reassigning_stops_reminders(self):
        issue = self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'})
        self.store.mutate(issue, 'update', {'status': 'done'}, ACTOR, PEOPLE)
        self.assertEqual(self.reminders(treer).tick()['sent'], 0)
        self.store.mutate(issue, 'update', {'status': 'open', 'assignee': 'ag_b'}, ACTOR, PEOPLE)
        self.assertEqual(self.reminders(treer).tick()['sent'], 1)
        self.assertEqual(treer.sends()[0][treer.sends()[0].index('--to') + 1], 'ag_b')

    def test_long_lists_are_capped(self):
        ids = [self.issue(assignee='ag_a') for _ in range(MAX_LISTED + 3)]
        treer = FakeTreer({'ag_a': 'idle'})
        self.reminders(treer).tick()
        body = treer.sends()[0][treer.sends()[0].index('--body') + 1]
        self.assertIn(f'{len(ids)} unresolved issues', body)
        self.assertIn('(+3 more)', body)
        self.assertNotIn(f'#{ids[-1]},', body)

    def test_malformed_probe_output_counts_as_not_idle(self):
        self.issue(assignee='ag_a')
        treer = FakeTreer({'ag_a': 'garbage'})
        self.assertEqual(self.reminders(treer).tick()['sent'], 0)
        mismatched = lambda args: (0, json.dumps({'agent_id': 'ag_other', 'status': 'idle'}))
        self.assertEqual(self.reminders(mismatched).tick()['sent'], 0)

    def test_link_is_resolved_once_and_only_when_sending(self):
        self.issue(assignee='ag_a'); self.issue(assignee='ag_b')
        calls = []
        treer = FakeTreer({'ag_a': 'idle', 'ag_b': 'idle'})
        self.reminders(treer, link=lambda: calls.append(1) or 'http://x/').tick()
        self.assertEqual(len(calls), 1)
        calls.clear()
        self.reminders(FakeTreer({}), link=lambda: calls.append(1) or 'http://x/').tick()
        self.assertEqual(calls, [])

    def test_hour_arithmetic_and_switch(self):
        self.assertEqual(next_boundary(T0), T0 + HOUR)
        self.assertEqual(next_boundary(T0 + HOUR - 0.5), T0 + HOUR)
        for value in ('0', 'off', 'False', ' no '):
            self.assertFalse(enabled({'ISSUES_REMINDERS': value}))
        for environ in ({}, {'ISSUES_REMINDERS': 'on'}, {'ISSUES_REMINDERS': '1'}):
            self.assertTrue(enabled(environ))

    def run_loop(self, start, until):
        """Drive the real scheduler loop with a fake clock/wait; returns the slots that ran."""
        self.now = start
        ran = []
        scheduler = self.reminders(FakeTreer())
        scheduler.tick = lambda now=None: ran.append(slot_of(self.now))

        def wait(seconds):
            self.now += seconds - 0.4          # wake slightly early: must not fire before the hour
            if self.now > until:
                scheduler.close()
                return True
            return False
        scheduler.loop(wait)
        return ran

    def test_scheduler_runs_on_the_hour_and_never_early(self):
        ran = self.run_loop(T0 + 1800, T0 + 3 * HOUR + 1)
        self.assertEqual(ran, [slot_of(T0) + 1, slot_of(T0) + 2, slot_of(T0) + 3])

    def test_restart_shortly_after_the_hour_catches_up_but_late_restart_does_not(self):
        self.assertEqual(self.run_loop(T0 + CATCH_UP - 1, T0 + HOUR / 2), [slot_of(T0)])
        self.assertEqual(self.run_loop(T0 + CATCH_UP + 1, T0 + HOUR / 2), [])
        self.assertEqual(self.run_loop(T0 + CATCH_UP + 1, T0 + HOUR + 1), [slot_of(T0) + 1])

    def test_a_failing_pass_does_not_kill_the_scheduler(self):
        scheduler = self.reminders(lambda args: 1 / 0)
        self.issue(assignee='ag_a')
        scheduler.guarded()
        self.assertEqual(self.logs, ['Issues reminder pass failed: ZeroDivisionError'])

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
        scheduler = Reminders(self.store, lambda: 'http://issues.treer/', str(script), clock=lambda: self.now, log=self.logs.append)
        self.assertEqual(scheduler.tick()['sent'], 1)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(calls[0], ['agent', 'show', 'ag_a'])
        self.assertEqual(calls[1][:4], ['message', 'send', '--to', 'ag_a'])
        self.assertEqual(self.logs, [])
        missing = Reminders(self.store, lambda: '', str(script) + '-missing', clock=lambda: T0 + HOUR + 1, log=self.logs.append)
        self.assertEqual(missing.tick()['sent'], 0)


if __name__ == '__main__':
    unittest.main()
