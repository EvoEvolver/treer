"""Hourly reminders for unresolved issues assigned to idle Agents.

Every full hour (epoch hour, i.e. the top of the hour in whole-hour time zones) the App looks for issues that
are not done/closed and assigned to an Agent, probes each such Agent with `treer agent show`, and sends one
Message per idle Agent through `treer message send` using the App's own runtime identity. No credential is
read, logged or passed on the command line: the CLI resolves identity from the inherited environment.

Anti-spam: one aggregated Message per Agent per hour; a (hour, Agent) record in SQLite survives restarts; the
Message idempotency key is stable per (hour, Agent), so a crash between send and record cannot duplicate it.
A busy, unknown or unreachable Agent is skipped until the next hour. Message bodies carry only issue IDs and
a link, never titles or descriptions.
"""
import json
import os
import subprocess
import sys
import threading
import time

HOUR = 3600
CATCH_UP = 300          # a restart within 5 minutes after the hour still runs that hour's pass
MAX_LISTED = 10         # issue IDs named in one Message; the rest are summarised as "+N more"
TIMEOUT = 15


def enabled(environ=None):
    """ISSUES_REMINDERS=0|off|false|no disables the poller; anything else (default) enables it."""
    value = (os.environ if environ is None else environ).get('ISSUES_REMINDERS', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


def slot_of(now):
    return int(now // HOUR)


def next_boundary(now):
    return (slot_of(now) + 1) * HOUR


class Reminders:
    def __init__(self, store, base_url, treer_bin='treer', clock=time.time, runner=None, log=None):
        self.store, self.base_url, self.treer_bin, self.clock = store, base_url, treer_bin, clock
        self.runner = runner or self.run_treer
        self.log = log or (lambda message: print(message, file=sys.stderr, flush=True))
        self.stop = threading.Event()
        self.thread = None

    def run_treer(self, args):
        """Run the Treer CLI; returns (exit status, stdout). stderr/stdout are never logged."""
        try:
            result = subprocess.run([self.treer_bin, *args], capture_output=True, text=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError):
            return 127, ''
        return result.returncode, result.stdout

    def idle(self, agent):
        status, output = self.runner(['agent', 'show', agent])
        if status != 0:
            return False
        try:
            info = json.loads(output)
        except ValueError:
            return False
        return isinstance(info, dict) and info.get('agent_id') == agent and info.get('status') == 'idle' and info.get('busy') is not True

    def body(self, issue_ids, link):
        shown = ', '.join(f'#{i}' for i in issue_ids[:MAX_LISTED])
        more = f' (+{len(issue_ids) - MAX_LISTED} more)' if len(issue_ids) > MAX_LISTED else ''
        noun = 'issue' if len(issue_ids) == 1 else 'issues'
        return (f'Issues reminder: you are idle and have {len(issue_ids)} unresolved {noun} assigned to you: {shown}{more}.\n'
                f'{link}')

    def tick(self, now=None):
        """One pass for the current hour. Safe to repeat: already reminded Agents are skipped."""
        slot = slot_of(self.clock() if now is None else now)
        sent = skipped = 0
        link = None
        for agent, issue_ids in self.store.unresolved_by_assignee().items():
            if self.store.reminded(slot, agent):
                continue
            if not self.idle(agent):
                skipped += 1
                continue
            link = link or self.base_url()   # resolved once per pass, and only when something is to be sent
            status, _ = self.runner(['message', 'send', '--to', agent, '--body', self.body(issue_ids, link),
                                     '--idempotency-key', f'issues-reminder-{agent}-{slot}'])
            if status != 0:
                skipped += 1
                self.log(f'Issues reminder not delivered to {agent} (treer exit {status})')
                continue
            self.store.record_reminder(slot, agent, issue_ids)
            sent += 1
        return {'slot': slot, 'sent': sent, 'skipped': skipped}

    def loop(self, wait=None):
        wait = wait or self.stop.wait
        now = self.clock()
        if now - slot_of(now) * HOUR < CATCH_UP:
            self.guarded()
        while not self.stop.is_set():
            target = next_boundary(self.clock())
            while self.clock() < target:   # a timer may wake early; never run a pass before the hour
                if wait(max(1.0, target - self.clock())):
                    return
            self.guarded()

    def guarded(self):
        try:
            self.tick()
        except Exception as exc:  # a failed pass must never kill the scheduler
            self.log(f'Issues reminder pass failed: {type(exc).__name__}')

    def start(self):
        self.thread = threading.Thread(target=self.loop, name='issues-reminders', daemon=True)
        self.thread.start()
        return self.thread

    def close(self):
        self.stop.set()
