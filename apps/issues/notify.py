"""Issue-change notifications: @mentions, recipient rules and the coalescing digest.

The first event for a (recipient, issue) in a five minute window is sent immediately by `Application.mutation`
with the actor's own token (Core applies the actor's Message Policy). Later events in the window are only
recorded in SQLite; when the window ends this module sends ONE digest through `treer message send` under the
App's identity and opens a new quiet window. Digest keys are stable per (recipient, issue, window start), a failed
digest is retried at most three times, and Messages carry only issue IDs, short fixed summaries and a reference
(a CLI command for Agents, a link for people) - plus, for a comment that has one, its sanitized title (`clean_title`);
never issue titles, descriptions or comment bodies. No credential is read, logged or placed on a command line.
"""
import json
import os
import re
import unicodedata
import sys
import threading
import time

from reminders import run_cli, safe_link

WINDOW = 300
FLUSH_EVERY = 30
MAX_ATTEMPTS = 3
MAX_MENTIONS = 10
DEFAULT_HOURLY = 20
MENTION = re.compile(r'(?<![\w@/.\-])@(/[\w./\-]+|\w[\w.\-]*)')
SAFE_PATH = re.compile(r'[A-Za-z0-9 ._/\-]{1,120}')


def enabled(environ=None):
    """ISSUES_NOTIFY=0|off|false|no|disabled turns every issue notification off (default on)."""
    value = (os.environ if environ is None else environ).get('ISSUES_NOTIFY', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


TITLE_LIMIT, DIGEST_TITLE_LIMIT, TITLE_BYTES = 120, 60, 400
TITLE_MARK = ', title (untrusted): "'
_WHITESPACE_CONTROLS = '\t\n\r\x0b\x0c\x85\u2028\u2029'
_QUOTES = dict.fromkeys('"`\u201c\u201d\u201e\u201f\u00ab\u00bb\u2039\u203a\u300c\u300d\u300e\u300f\u301d\u301e\u301f\uff02\u2018\u2019\u201a\u201b\\', "'")
_BRACKETS = str.maketrans('<[{>]}', '((()))')


def clean_title(raw, limit=TITLE_LIMIT):
    """A comment title as it may appear inside a Message: author text is untrusted data headed for other agents' prompts.
    NFKC first (full-width quotes, at-signs and slashes lose their look-alike forms); then only printable letters, numbers,
    punctuation, symbols, ordinary spaces and at most three combining marks in a row survive (no controls, format or
    bidi characters, tag block, variation selectors, line/paragraph separators, private-use or unassigned code points);
    quote-like characters become apostrophes, brackets become parentheses, '@' becomes 'at' and '://' is broken so no
    mention, markdown link, HTML tag, code fence, template or URL can be formed; whitespace folds to single spaces;
    the result is capped by code points and by bytes. Returns '' when nothing usable is left."""
    if not isinstance(raw, str):
        return ''
    out, marks = [], 0
    for char in unicodedata.normalize('NFKC', raw):
        category, code = unicodedata.category(char), ord(char)
        if char in _WHITESPACE_CONTROLS or category == 'Zs':
            out.append(' ')
            marks = 0
        elif category in ('Cc', 'Cf', 'Zl', 'Zp', 'Co', 'Cs', 'Cn') or 0xFE00 <= code <= 0xFE0F or 0xE0100 <= code <= 0xE01EF:
            continue
        elif category[0] == 'M':
            marks += 1
            if marks <= 3:
                out.append(char)
        else:
            marks = 0
            out.append(_QUOTES.get(char, char))
    text = ''.join(out).translate(_BRACKETS).replace('@', ' at ').replace('://', ' ://')
    text = ' '.join(text.split())
    if len(text) > limit:
        text = text[:limit - 1].rstrip() + '\u2026'
    while len(text.encode()) > TITLE_BYTES:
        text = text[:-2].rstrip() + '\u2026'
    return text


def digest_form(notice, limit=DIGEST_TITLE_LIMIT):
    """The same notice with its quoted title cut to the digest length (a sanitized title contains no double quote)."""
    head, mark, rest = notice.partition(TITLE_MARK)
    if not mark:
        return notice
    title, quote, tail = rest.partition('"')
    if len(title) > limit:
        title = title[:limit - 1].rstrip() + '\u2026'
    return head + mark + title + quote + tail


SAFE_NAME = re.compile(r'[A-Za-z0-9 ._/\-]{1,24}')


def label(name, fallback):
    """A directory name as it may appear in a Message. Anything outside a small safe character set (control
    characters, bidi marks, markup, non-ASCII, over-long names) is replaced by the stable ID, which is itself
    a restricted token - names are attacker-influenced text and are never forwarded verbatim."""
    name = str(name)
    return ' '.join(name.split()) if SAFE_NAME.fullmatch(name) and name.strip() else str(fallback)


def hourly_limit(environ=None):
    """ISSUES_NOTIFY_HOURLY_LIMIT: immediate Messages per recipient per hour before updates are folded into one
    per-recipient digest (default 20, 1..1000)."""
    try:
        value = int((os.environ if environ is None else environ).get('ISSUES_NOTIFY_HOURLY_LIMIT', DEFAULT_HOURLY))
    except ValueError:
        return DEFAULT_HOURLY
    return value if 1 <= value <= 1000 else DEFAULT_HOURLY


def path_tokens(text):
    """Absolute tree paths (`@/team/agent`) written in text, as typed, de-duplicated case-insensitively, at most MAX_MENTIONS."""
    if not text or '@/' not in text:
        return []
    found, seen = [], set()
    for token in MENTION.findall(text):
        token = token.rstrip('.-')
        if token.startswith('/') and token.lower() not in seen and SAFE_PATH.fullmatch(token):
            seen.add(token.lower())
            found.append(token)
    return found[:MAX_MENTIONS]


def resolve_mentions(text, directory, paths=None):
    """Stable IDs of Agents @-mentioned in text. `@/tree/path` is looked up in `paths` ({lower-case path: ID}, resolved
    beforehand by Core with the author's own token) or the directory's own `path`; `@ag_id` is exact; `@name` must
    match exactly one Agent name (case-insensitive), ambiguous names are ignored. Only Agents visible in `directory`
    can be mentioned, so a path can never reach someone the author's Policy hides."""
    if not text or '@' not in text:
        return set()
    agents = [p for p in directory if p.get('kind') == 'agent']
    by_id = {p['id']: p for p in agents}
    found = set()
    for token in MENTION.findall(text):
        token = token.rstrip('.-')
        key = token.lower()
        if token.startswith('/'):
            hit = (paths or {}).get(key)
            hits = [by_id[hit]] if hit in by_id else [p for p in agents if str(p.get('path', '')).lower() == key]
        else:
            hits = [p for p in agents if p['id'].lower() == key] or [p for p in agents if str(p.get('name', '')).lower() == key]
        if len(hits) == 1:
            found.add(hits[0]['id'])
        if len(found) >= MAX_MENTIONS:
            break
    return found


def unresolved_mentions(text, directory, paths=None):
    """@tokens that notified nobody (unknown or ambiguous), so an author does not assume they were reached."""
    if not text or '@' not in text:
        return []
    missed = []
    for token in MENTION.findall(text):
        token = token.rstrip('.-')
        if not resolve_mentions('@' + token, directory, paths) and token not in missed:
            missed.append(token)
    return [t for t in missed if SAFE_PATH.fullmatch(t)][:5]


def candidates(token, directory):
    """Ways to write an ambiguous `@name` unambiguously: the tree path Core put in the author's own directory (it
    applies agent.tree.read), else the stable ID. No other source of paths is used: the App identity's view of the tree
    or a path the author can merely resolve to the Agent would disclose paths the author's Policy hides."""
    if token.startswith('/') or '/' in token:
        return []
    key = token.lower()
    named = [p for p in directory if p.get('kind') == 'agent' and str(p.get('name', '')).lower() == key]
    if len(named) < 2:
        return []
    out = []
    for person in named[:5]:
        path = person.get('path')
        out.append(path if isinstance(path, str) and SAFE_PATH.fullmatch(path) else person['id'])
    return out


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


def notice_for(action, actor, old, issue, changed, mentions, names, comment_title=None):
    """One short fixed sentence. Returns None when nothing changed (no notification)."""
    if action == 'create':
        core = 'Issue created'
    elif action == 'comment':
        core = 'New comment'
        title = clean_title(comment_title)
        if title:
            core += f'{TITLE_MARK}{title}"'
    else:
        parts = []
        if 'status' in changed:
            parts.append(f"Status {old['status']}→{issue['status']}")
        if 'assignee' in changed:
            parts.append(f"Assigned to {label(names.get(issue['assignee'], issue['assignee']), issue['assignee'])}" if issue['assignee'] else 'Unassigned')
        rest = sorted(set(changed) - {'status', 'assignee'})
        if rest:
            parts.append('Updated ' + ', '.join(rest))
        if not parts:
            return None
        core = '; '.join(parts)
    text = f"{core} · by {label(actor.get('name', actor['id']), actor['id'])}"
    if mentions:
        text += ' · mentions ' + ', '.join('@' + label(names.get(m, m), m) for m in mentions[:3]) + (f' +{len(mentions) - 3}' if len(mentions) > 3 else '')
    return text


def issue_ref(issue_id, agent):
    """How to open an issue from a Message. Agents get a fixed command with a validated integer (no title text, no
    URL, no tree path); people get the browser link, added by the caller when one is configured."""
    return f'issues show {int(issue_id)} --titles' if agent else ''


def digest_body(issue_id, summaries, link, agent=False):
    unique = list(dict.fromkeys(summaries))
    shown = '; '.join(unique[:4]) + (f'; +{len(unique) - 4} more' if len(unique) > 4 else '')
    noun = f'update{"s" if len(summaries) != 1 else ""}'
    if issue_id == 0:   # the per-recipient overflow digest: updates over the hourly limit, across issues
        text = f'Issues \u00b7 {len(summaries)} more {noun} (over the hourly limit) in the last 5 minutes: {shown}'
        tail = 'issues list --assignee me --unresolved' if agent else link
    else:
        text = f'Issue #{issue_id} \u00b7 {len(summaries)} more {noun} in the last 5 minutes: {shown}'
        tail = issue_ref(issue_id, True) if agent else (f'{link}#issue={issue_id}' if link else '')
    return text + (f'\n{tail}' if tail else '')


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
            agent = not row['recipient'].startswith('usr_')   # an unknown kind gets the Agent form (no link)
            status, _ = self.runner(['message', 'send', '--to', row['recipient'], '--body', digest_body(row['issue_id'], row['pending'], link, agent),
                                     '--idempotency-key', f"issues-digest-{row['recipient']}-{row['issue_id']}-{row['actor'] or 'any'}-{int(row['start'])}"])
            if status == 0:
                self.store.digest_done(row['recipient'], row['issue_id'], row['actor'], now)
                sent += 1
            else:
                self.log(f"Issues digest not delivered to {row['recipient']} for #{row['issue_id']} (treer exit {status})")
                self.store.digest_failed(row['recipient'], row['issue_id'], row['actor'], MAX_ATTEMPTS)
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
