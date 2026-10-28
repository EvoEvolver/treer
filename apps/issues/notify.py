"""Issue-change notifications: @mentions, recipients, the Message text and the Agent wake-up.

Every create/update/comment that concerns someone sends ONE Core Message per recipient kind with the ACTOR's own
token (Core applies the actor's Message Policy). A Message never wakes an idle Agent, so an Agent that was just
assigned an issue or @mentioned is also prompted with `treer agent prompt` under the App's own identity (its
Policy needs `agent.prompt`). The prompt is a short fixed pointer: issue number, reason, actor label, Message ID
and how to open it; never titles, descriptions or comment bodies. At most one wake per (Agent, issue) per
ISSUES_WAKE_MINUTES (default 10). Only Agents whose Message Core accepted are woken, so the App's grant cannot
reach anyone the actor may not message. Humans get the Message only.
"""
import json
import os
import re
import subprocess
import sys
import threading
import unicodedata
import urllib.parse

MAX_MENTIONS = 10
MENTION = re.compile(r'(?<![\w@/.\-])@(/[\w./\-]+|\w[\w.\-]*)')
SAFE_PATH = re.compile(r'[A-Za-z0-9 ._/\-]{1,120}')
TIMEOUT = 15


def enabled(environ=None):
    """ISSUES_NOTIFY=0|off|false|no|disabled turns every issue notification off (default on)."""
    value = (os.environ if environ is None else environ).get('ISSUES_NOTIFY', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


TITLE_LIMIT, TITLE_BYTES = 120, 400
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


SAFE_NAME = re.compile(r'[A-Za-z0-9 ._/\-]{1,24}')


def label(name, fallback):
    """A directory name as it may appear in a Message. Anything outside a small safe character set (control
    characters, bidi marks, markup, non-ASCII, over-long names) is replaced by the stable ID, which is itself
    a restricted token - names are attacker-influenced text and are never forwarded verbatim."""
    name = str(name)
    return ' '.join(name.split()) if SAFE_NAME.fullmatch(name) and name.strip() else str(fallback)


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


def wake_window(environ=None):
    """ISSUES_WAKE_MINUTES: at most one wake per (Agent, issue) in this many minutes (default 10, 0..1440; 0 = no dedupe)."""
    raw = (os.environ if environ is None else environ).get('ISSUES_WAKE_MINUTES', '10')
    try:
        minutes = float(raw)
    except ValueError:
        raise ValueError('ISSUES_WAKE_MINUTES must be a number of minutes between 0 and 1440') from None
    if not 0 <= minutes <= 1440:
        raise ValueError('ISSUES_WAKE_MINUTES must be a number of minutes between 0 and 1440')
    return minutes * 60


def wake_enabled(environ=None):
    """ISSUES_WAKE=0|off|false|no|disabled stops prompting Agents (Messages still go out)."""
    value = (os.environ if environ is None else environ).get('ISSUES_WAKE', 'on').strip().lower()
    return value not in ('0', 'off', 'false', 'no', 'disabled')


def safe_link(value):
    """The App link for a prompt; anything that is not a plain http(s) URL is dropped."""
    value = str(value or '')
    parts = urllib.parse.urlsplit(value)
    if parts.scheme not in ('http', 'https') or not parts.netloc or parts.username or parts.password or any(c.isspace() for c in value):
        return ''
    return value


def run_cli(treer_bin, args):
    """Run the Treer CLI: (exit status, error code or None). Output is never logged."""
    try:
        result = subprocess.run([treer_bin, *args], capture_output=True, text=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return 127, 'treer_unavailable'
    if result.returncode == 0:
        return 0, None
    for stream in (result.stderr, result.stdout):
        try:
            code = json.loads(stream.strip().splitlines()[-1])['error']['code']
            if isinstance(code, str) and re.fullmatch(r'[a-z0-9_.\-]{1,64}', code):
                return result.returncode, code
        except (ValueError, KeyError, TypeError, IndexError):
            continue
    return result.returncode, 'prompt_failed'


DENIED = ('policy_denied', 'forbidden', 'permission_denied', 'agent_prompt_denied')


def wake_text(issue_id, reason, actor_label, message_id, link):
    reasons = {'assigned': 'assigned to you', 'mentioned': 'you were mentioned'}
    text = f"Issues #{int(issue_id)}: {reasons[reason]} by {actor_label}."
    if message_id and re.fullmatch(r'[A-Za-z0-9_\-]{1,80}', message_id):
        text += f' Message {message_id}.'
    text += f' Read it: issues show {int(issue_id)} --titles'
    if link:
        text += f' (or {link}#issue={int(issue_id)})'
    return text


class Waker:
    """Prompts Agents about an issue event and records the outcome on the activity entry.
    Outcome per Agent: `prompted`, `deduped` (woken about this issue within the window), `denied` (Policy refused
    `agent.prompt`; the Message still stands) or `failed:<code>` (retried by the next event)."""
    def __init__(self, store, treer_bin='treer', window=600, link=lambda: '', runner=None, background=True, log=None):
        self.store, self.window, self.link, self.background = store, window, link, background
        self.runner = runner or (lambda args: run_cli(treer_bin, args))
        self.log = log or (lambda message: print(message, file=sys.stderr, flush=True))

    def wake(self, event_id, issue_id, targets, actor_label, message_id):
        """targets: {agent ID: 'assigned' | 'mentioned'}. Returns the initial outcome map ('pending' while running)."""
        if not targets:
            return {}
        if self.background:
            threading.Thread(target=self.run, args=(event_id, issue_id, dict(targets), actor_label, message_id),
                             name='issues-wake', daemon=True).start()
            return {t: 'pending' for t in targets}
        return self.run(event_id, issue_id, targets, actor_label, message_id)

    def run(self, event_id, issue_id, targets, actor_label, message_id):
        outcome = {}
        try:
            link = safe_link(self.link())
        except Exception:
            link = ''
        for target, reason in sorted(targets.items()):
            try:
                if not self.store.wake_claim(target, issue_id, self.store.clock(), self.window):
                    outcome[target] = 'deduped'
                    continue
                status, code = self.runner(['agent', 'prompt', target, wake_text(issue_id, reason, actor_label, message_id, link)])
                if status == 0:
                    outcome[target] = 'prompted'
                elif code in DENIED:
                    outcome[target] = 'denied'
                else:
                    self.store.wake_release(target, issue_id)
                    outcome[target] = f'failed:{code}'
                    self.log(f'Issues wake of {target} for #{issue_id} failed ({code})')
            except Exception as exc:          # a failed wake never breaks the request or the worker
                outcome[target] = 'failed:internal'
                self.log(f'Issues wake of {target} for #{issue_id} failed ({type(exc).__name__})')
        try:
            self.store.record_wake(event_id, outcome)
        except Exception as exc:
            self.log(f'Issues wake result not recorded ({type(exc).__name__})')
        return outcome
