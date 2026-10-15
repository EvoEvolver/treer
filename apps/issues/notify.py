"""Issue-change notifications: @mentions, recipient rules and the coalescing digest.

The first event for a (recipient, issue) in a five minute window is sent immediately by `Application.mutation`
with the actor's own token (Core applies the actor's Message Policy). Later events in the window are only
recorded in SQLite; when the window ends this module sends ONE digest through `treer message send` under the
App's identity and opens a new quiet window. Digest keys are stable per (recipient, issue, window start), a failed
digest is retried at most three times, and Messages carry only issue IDs, short fixed summaries and a link -
never titles, descriptions or comment bodies. No credential is read, logged or placed on a command line.
"""
import os
import re
import sys
import threading
import time

from reminders import run_cli, safe_link

WINDOW = 300
FLUSH_EVERY = 30
MAX_ATTEMPTS = 3
MAX_MENTIONS = 10
MENTION = re.compile(r'(?<![\w@/.\-])@(/[\w./\-]+|\w[\w.\-]*)')


def enabled(environ=None):
    """ISSUES_NOTIFY=0|off|false|no|disabled turns every issue notification off (default on)."""
    value = (os.environ if environ is None else environ).get('ISSUES_NOTIFY', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


def label(value, limit=40):
    """A directory name made safe for a one-line Message."""
    return ' '.join(str(value).split())[:limit]


def resolve_mentions(text, directory):
    """Stable IDs of Agents @-mentioned in text. `@name` must match exactly one Agent name; ambiguous names are
    ignored (use `@/tree/path` or `@ag_id`). Matching is case-insensitive."""
    if not text or '@' not in text:
        return set()
    agents = [p for p in directory if p.get('kind') == 'agent']
    found = set()
    for token in MENTION.findall(text):
        token = token.rstrip('.-')
        key = token.lower()
        if token.startswith('/'):
            hits = [p for p in agents if str(p.get('path', '')).lower() == key]
        else:
            hits = [p for p in agents if p['id'].lower() == key] or [p for p in agents if str(p.get('name', '')).lower() == key]
        if len(hits) == 1:
            found.add(hits[0]['id'])
        if len(found) >= MAX_MENTIONS:
            break
    return found


def recipients_for(action, issue, old_assignee, changed, mentions, actor_id):
    """Who hears about an event. The assignee hears about everything; the creator only about status/assignee
    changes; mentioned Agents about whatever mentioned them. The actor never hears about their own action."""
    people = {issue['assignee']}
    if action == 'update':
        if 'assignee' in changed:
            people.add(old_assignee)
        if 'status' in changed or 'assignee' in changed:
            people.add(issue['creator'])
    return sorted((people | set(mentions)) - {None, actor_id})


def notice_for(action, actor_name, old, issue, changed, mention_names, people_name):
    """One short fixed sentence. Returns None when nothing changed (no notification)."""
    if action == 'create':
        core = 'Issue created'
    elif action == 'comment':
        core = 'New comment'
    else:
        parts = []
        if 'status' in changed:
            parts.append(f"Status {old['status']}→{issue['status']}")
        if 'assignee' in changed:
            parts.append(f"Assigned to {people_name(issue['assignee'])}" if issue['assignee'] else 'Unassigned')
        rest = sorted(set(changed) - {'status', 'assignee'})
        if rest:
            parts.append('Updated ' + ', '.join(rest))
        if not parts:
            return None
        core = '; '.join(parts)
    text = f'{core} · by {label(actor_name)}'
    if mention_names:
        text += ' · mentions ' + ', '.join('@' + label(n) for n in mention_names[:3]) + (f' +{len(mention_names) - 3}' if len(mention_names) > 3 else '')
    return text


def digest_body(issue_id, summaries, link):
    unique = list(dict.fromkeys(summaries))
    shown = '; '.join(unique[:4]) + (f'; +{len(unique) - 4} more' if len(unique) > 4 else '')
    text = f'Issue #{issue_id} · {len(summaries)} more update{"s" if len(summaries) != 1 else ""} in the last 5 minutes: {shown}'
    return text + (f'\n{link}#issue={issue_id}' if link else '')


class Digest:
    """Sends one coalesced Message per (recipient, issue) when a window with pending updates ends."""
    def __init__(self, store, base_url, treer_bin='treer', clock=time.time, runner=None, log=None):
        self.store, self.base_url, self.treer_bin, self.clock = store, base_url, treer_bin, clock
        self.runner = runner or (lambda args: run_cli(self.treer_bin, args))
        self.log = log or (lambda message: print(message, file=sys.stderr, flush=True))
        self.stop = threading.Event()

    def flush(self):
        now = self.clock()
        sent = 0
        link = None
        for row in self.store.due_digests(now):
            link = link if link is not None else safe_link(self.base_url)
            status, _ = self.runner(['message', 'send', '--to', row['recipient'], '--body', digest_body(row['issue_id'], row['pending'], link),
                                     '--idempotency-key', f"issues-digest-{row['recipient']}-{row['issue_id']}-{int(row['start'])}"])
            if status == 0:
                self.store.digest_done(row['recipient'], row['issue_id'], now)
                sent += 1
            else:
                self.log(f"Issues digest not delivered to {row['recipient']} for #{row['issue_id']} (treer exit {status})")
                self.store.digest_failed(row['recipient'], row['issue_id'], MAX_ATTEMPTS)
        self.store.prune_windows(now)
        return sent

    def guarded(self):
        try:
            self.flush()
        except Exception as exc:  # a failed pass must never kill the worker
            self.log(f'Issues digest pass failed: {type(exc).__name__}')

    def loop(self, wait=None):
        wait = wait or self.stop.wait
        while not wait(FLUSH_EVERY):
            self.guarded()

    def start(self):
        thread = threading.Thread(target=self.loop, name='issues-digest', daemon=True)
        thread.start()
        return thread

    def close(self):
        self.stop.set()
