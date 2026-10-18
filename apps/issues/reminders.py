"""Reminders for unresolved issues assigned to Agents, with exponential backoff per (assignee, issue).

An issue that needs a reminder (not done/closed, assigned to an Agent, not carrying a skip label such as `epic`) is first
reminded FIRST after it entered that state (10 minutes by default); every later interval is the previous one times FACTOR
(2 by default), capped at MAX (24 hours): 10m, 20m, 40m, 1h20m, 2h40m, 5h20m, 10h40m, 21h20m, then 24h. Any real activity on
the issue (a comment, status or assignee change, an edit, a lease) starts the sequence over, a new assignee starts from
scratch and the old state is dropped. The reminder is only sent while the assignee is idle: when it is busy at the due time
nothing is sent and the interval does not grow, the next poll (a minute later) looks again. Everything due for the same
Agent at the same moment goes into ONE Message; each issue keeps its own backoff. State lives in SQLite, so restarts keep the
schedule, and issues unresolved at deployment time simply start their first interval then.

Sending: `treer agent show` to check idleness, `treer message send` under the App's own runtime identity. No credential is
read, logged or put on a command line. A failed send is not recorded (the same idempotency key is used on the retry). The
key covers the Agent, the issue set and each issue's backoff step, so a crash between send and record cannot duplicate a
reminder and different steps never collide. Message bodies carry only issue IDs and a fixed CLI reference, never titles or descriptions.
"""
import collections
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import threading
import time
import urllib.parse

POLL = 60.0             # the scheduler wakes at least this often (or at the nearest due time)
MAX_LISTED = 10         # issue IDs named in one Message; the rest are summarised as "+N more"
TIMEOUT = 15
DURATION = re.compile(r'^\s*(\d+(?:\.\d+)?)\s*(s|m|h|d)\s*$')
UNITS = {'s': 1, 'm': 60, 'h': 3600, 'd': 86400}

Backoff = collections.namedtuple('Backoff', 'first factor maximum')


def parse_duration(text, name):
    match = DURATION.match(text or '')
    if not match:
        raise ValueError(f'{name} must be a duration such as 10m, 90s, 1.5h or 24h, not {text!r}')
    return float(match.group(1)) * UNITS[match.group(2)]


def backoff(environ=None):
    """ISSUES_REMINDER_FIRST (default 10m, 1m-24h), ISSUES_REMINDER_FACTOR (default 2, 1-10) and ISSUES_REMINDER_MAX (default 24h,
    at least FIRST, at most 30d). Invalid values are an error at start-up, never silently replaced by a default."""
    env = os.environ if environ is None else environ
    first = parse_duration(env.get('ISSUES_REMINDER_FIRST', '10m'), 'ISSUES_REMINDER_FIRST')
    maximum = parse_duration(env.get('ISSUES_REMINDER_MAX', '24h'), 'ISSUES_REMINDER_MAX')
    try:
        factor = float(env.get('ISSUES_REMINDER_FACTOR', '2'))
    except ValueError:
        raise ValueError('ISSUES_REMINDER_FACTOR must be a number between 1 and 10') from None
    if not 60 <= first <= 86400:
        raise ValueError('ISSUES_REMINDER_FIRST must be between 1m and 24h')
    if not (math.isfinite(factor) and 1 <= factor <= 10):
        raise ValueError('ISSUES_REMINDER_FACTOR must be between 1 and 10')
    if not first <= maximum <= 30 * 86400:
        raise ValueError('ISSUES_REMINDER_MAX must be at least ISSUES_REMINDER_FIRST and at most 30d')
    return Backoff(first, factor, maximum)


def enabled(environ=None):
    """ISSUES_REMINDERS=0|off|false|no disables the poller; anything else (default) enables it."""
    value = (os.environ if environ is None else environ).get('ISSUES_REMINDERS', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


def skip_labels(environ=None):
    """ISSUES_REMINDER_SKIP_LABELS: comma-separated labels whose issues never trigger reminders. Default `epic`; matching is
    case-insensitive. Set it to an empty value to remind about everything."""
    raw = (os.environ if environ is None else environ).get('ISSUES_REMINDER_SKIP_LABELS')
    if raw is None:
        return ('epic',)
    return tuple(dict.fromkeys(label.strip() for label in raw.split(',') if label.strip()))


def run_cli(treer_bin, args):
    """Run the Treer CLI; returns (exit status, stdout). stdout/stderr are never logged."""
    try:
        result = subprocess.run([treer_bin, *args], capture_output=True, text=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return 127, ''
    return result.returncode, result.stdout


def safe_link(base_url):
    """The App link for a Message; anything that is not a plain http(s) URL is dropped, not sent."""
    try:
        value = str(base_url() or '')
    except Exception:
        return ''
    parts = urllib.parse.urlsplit(value)
    if parts.scheme not in ('http', 'https') or not parts.netloc or parts.username or parts.password or any(c.isspace() for c in value):
        return ''
    return value


class Reminders:
    def __init__(self, store, treer_bin='treer', clock=time.time, runner=None, log=None, skip=None, schedule=None):
        self.store, self.treer_bin, self.clock = store, treer_bin, clock
        self.skip = skip
        self.schedule = schedule or backoff()
        self.runner = runner or self.run_treer
        self.log = log or (lambda message: print(message, file=sys.stderr, flush=True))
        self.retry_after = {}      # agent -> earliest next probe: a busy or unreachable Agent is looked at at most once per POLL
        self.stop = threading.Event()
        self.thread = None

    def run_treer(self, args):
        return run_cli(self.treer_bin, args)

    def idle(self, agent):
        status, output = self.runner(['agent', 'show', agent])
        if status != 0:
            return False
        try:
            info = json.loads(output)
        except ValueError:
            return False
        return isinstance(info, dict) and info.get('agent_id') == agent and info.get('status') == 'idle' and info.get('busy') is not True

    REF = 'issues list --assignee me --unresolved'   # reminders only go to Agents: a fixed command, no URL, no tree path

    def body(self, issue_ids):
        shown = ', '.join(f'#{i}' for i in issue_ids[:MAX_LISTED])
        more = f' (+{len(issue_ids) - MAX_LISTED} more)' if len(issue_ids) > MAX_LISTED else ''
        noun = 'issue' if len(issue_ids) == 1 else 'issues'
        return f'Issues reminder: you are idle and have {len(issue_ids)} unresolved {noun} assigned to you: {shown}{more}.\n{self.REF}'

    @staticmethod
    def key(agent, rows):
        # The version is part of the key: after a reset the sequence starts at step 0 again, and a repeated key could be
        # silently de-duplicated by Core against the issue's earlier first reminder.
        steps = json.dumps([[row['issue_id'], row['version'], row['count']] for row in rows], separators=(',', ':'))
        return f'issues-reminder-{agent}-' + hashlib.sha256(steps.encode()).hexdigest()[:20]

    def tick(self, now=None):
        """One poll: align the schedule with the issues, then remind every idle Agent that has issues due."""
        now = self.clock() if now is None else now
        self.store.reminder_sync(self.store.reminder_candidates(skip_labels=skip_labels() if self.skip is None else self.skip), now, self.schedule.first)
        due = self.store.reminder_due(now)
        by_agent = {}
        for row in due:
            by_agent.setdefault(row['assignee'], []).append(row)
        sent = skipped = 0
        for agent, rows in by_agent.items():
            if self.retry_after.get(agent, 0) > now:
                skipped += 1            # looked at a moment ago and it was busy: do not probe again before the next poll
                continue
            if not self.idle(agent):
                self.retry_after[agent] = now + POLL
                skipped += 1            # busy, unknown or unreachable: no message and no growth; look again next poll
                continue
            status, _ = self.runner(['message', 'send', '--to', agent, '--body', self.body([r['issue_id'] for r in rows]),
                                     '--idempotency-key', self.key(agent, rows)])
            if status != 0:
                self.retry_after[agent] = now + POLL
                skipped += 1
                self.log(f'Issues reminder not delivered to {agent} (treer exit {status})')
                continue
            self.retry_after.pop(agent, None)
            self.store.reminder_advance([r['issue_id'] for r in rows], now, self.schedule.factor, self.schedule.maximum)
            sent += 1
        return {'sent': sent, 'skipped': skipped, 'due': len(due)}

    def loop(self, wait=None):
        wait = wait or self.stop.wait
        while not self.stop.is_set():
            self.guarded()
            try:
                nearest = self.store.reminder_next_due(self.clock())
            except Exception:
                nearest = None
            delay = POLL if nearest is None else min(POLL, max(1.0, nearest - self.clock()))
            if wait(delay):
                return

    def guarded(self):
        try:
            self.tick()
        except Exception as exc:  # a failed poll must never kill the scheduler
            self.log(f'Issues reminder poll failed: {type(exc).__name__}')

    def start(self):
        self.thread = threading.Thread(target=self.loop, name='issues-reminders', daemon=True)
        self.thread.start()
        return self.thread

    def close(self):
        self.stop.set()
