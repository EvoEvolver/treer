"""Transactional issue state. All ownership checks run under BEGIN IMMEDIATE."""
import json
import os
import sqlite3
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from auth import Error
from notify import WINDOW, notice_for, recipients_for, resolve_mentions, unresolved_mentions

STATUSES = ('open', 'in_progress', 'blocked', 'done', 'closed')
PRIORITIES = ('low', 'normal', 'high', 'urgent')


def stamp(now=None):
    return datetime.fromtimestamp(time.time() if now is None else now, timezone.utc).isoformat().replace('+00:00', 'Z')


def fail(code, message, status=400):
    raise Error(status, code, message)


def fields(body, allowed):
    if not isinstance(body, dict) or set(body) - set(allowed):
        fail('invalid_fields', 'Unknown fields or non-object JSON')


COMMENT_TITLE_BYTES = 200


def text(value, name, maximum, empty=False):
    if not isinstance(value, str) or len(value.encode()) > maximum or (not empty and not value.strip()):
        fail('invalid_field', f'{name} must be text, at most {maximum} bytes')
    return value


def ident(value):
    if type(value) is not int or value < 1:
        fail('invalid_id', 'Issue IDs must be positive integers')
    return value


class Store:
    def __init__(self, path, clock=time.time):
        self.path, self.clock = str(path), clock
        os.umask(0o077)
        Path(path).parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        Path(path).parent.chmod(0o700)
        with self.connect() as db:
            db.executescript('''
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS issues(id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS activity(id INTEGER PRIMARY KEY AUTOINCREMENT,
                    issue_id INTEGER NOT NULL REFERENCES issues(id), data TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS activity_issue ON activity(issue_id,id);
                CREATE TABLE IF NOT EXISTS oauth(state TEXT PRIMARY KEY, verifier TEXT NOT NULL, expires REAL NOT NULL, browser TEXT);
                CREATE TABLE IF NOT EXISTS reminders(slot INTEGER NOT NULL, assignee TEXT NOT NULL, issue_ids TEXT NOT NULL,
                    sent_at REAL NOT NULL, PRIMARY KEY(slot, assignee));
                CREATE TABLE IF NOT EXISTS notify_windows(recipient TEXT NOT NULL, issue_id INTEGER NOT NULL, actor TEXT NOT NULL,
                    window_start REAL NOT NULL, pending TEXT NOT NULL DEFAULT '[]', attempts INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(recipient, issue_id, actor));
                CREATE TABLE IF NOT EXISTS notify_proven(recipient TEXT NOT NULL, actor TEXT NOT NULL, hour INTEGER NOT NULL,
                    PRIMARY KEY(recipient, actor, hour));
                CREATE TABLE IF NOT EXISTS notify_budget(recipient TEXT NOT NULL, hour INTEGER NOT NULL, immediate INTEGER NOT NULL,
                    PRIMARY KEY(recipient, hour));
                CREATE TABLE IF NOT EXISTS reminder_backoff(issue_id INTEGER PRIMARY KEY, assignee TEXT NOT NULL, version INTEGER NOT NULL,
                    interval REAL NOT NULL, next_due REAL NOT NULL, count INTEGER NOT NULL DEFAULT 0);
                CREATE INDEX IF NOT EXISTS reminder_backoff_due ON reminder_backoff(next_due);
                CREATE TABLE IF NOT EXISTS sessions(secret TEXT PRIMARY KEY, token TEXT NOT NULL, expires REAL NOT NULL);
            ''')
        with self.connect() as db:
            columns = {row[1] for row in db.execute('PRAGMA table_info(oauth)')}
            if 'browser' not in columns:
                db.execute('ALTER TABLE oauth ADD COLUMN browser TEXT')
                db.execute('DELETE FROM oauth')
        os.chmod(self.path, 0o600)

    @contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=10)
        db.row_factory = sqlite3.Row
        db.execute('PRAGMA foreign_keys=ON')
        try:
            with db:
                yield db
        finally:
            db.close()

    def all(self, db):
        return {row['id']: dict(json.loads(row['data']), id=row['id'])
                for row in db.execute('SELECT id,data FROM issues')}

    def get(self, items, issue_id):
        if issue_id not in items:
            fail('issue_not_found', 'Issue not found', 404)
        return items[issue_id]

    def active_claim(self, issue):
        claim = issue.get('claim')
        if claim and datetime.fromisoformat(claim['expires_at'].replace('Z', '+00:00')).timestamp() > self.clock():
            return claim
        return None

    def view(self, issue, items):
        result = dict(issue, claim=self.active_claim(issue))
        descendants = []
        pending = [issue['id']]
        while pending:
            parent = pending.pop()
            children = [i for i in items.values() if i['parent_id'] == parent]
            descendants.extend(children)
            pending.extend(i['id'] for i in children)
        result['progress'] = {'total': len(descendants),
            'completed': sum(i['status'] in ('done', 'closed') for i in descendants),
            'by_status': {s: sum(i['status'] == s for i in descendants) for s in STATUSES}}
        result['unresolved_dependencies'] = [i for i in issue['blocked_by'] if items[i]['status'] not in ('done', 'closed')]
        return result

    def list(self, filters):
        allowed = ('status', 'assignee', 'label', 'parent_id', 'unresolved', 'limit', 'offset')
        fields(filters, allowed)
        try:
            limit, offset = int(filters.get('limit', 50)), int(filters.get('offset', 0))
        except (ValueError, TypeError):
            fail('invalid_pagination', 'Invalid pagination')
        if not 1 <= limit <= 100 or offset < 0:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        with self.connect() as db:
            items = self.all(db)
        result = sorted(items.values(), key=lambda i: i['id'], reverse=True)
        for key in ('status', 'assignee', 'label', 'parent_id', 'unresolved'):
            if key not in filters:
                continue
            value = filters[key]
            if key == 'unresolved':
                if value not in ('1', 'true', '0', 'false'):
                    fail('invalid_field', 'unresolved must be 1 or 0')
                if value in ('1', 'true'):
                    result = [i for i in result if i['status'] not in ('done', 'closed')]
            elif key == 'label':
                result = [i for i in result if value in i['labels']]
            elif key == 'parent_id':
                try:
                    parent = int(value) if value not in ('', 'null') else None
                except (ValueError, TypeError):
                    fail('invalid_id', 'Invalid parent ID')
                result = [i for i in result if i['parent_id'] == parent]
            else:
                result = [i for i in result if i[key] == value]
        return {'issues': [self.view(i, items) for i in result[offset:offset+limit]],
                'total': len(result), 'next_offset': offset+limit if offset+limit < len(result) else None}

    @staticmethod
    def titles_only(event):
        """A comment without its text: `title` (when the author wrote one) and `body_bytes` say what is there; the text
        is read on demand with `event()`. Other activity (changes, summaries) is small and unchanged."""
        if event.get('kind') != 'comment' or 'body' not in event:
            return event
        short = {k: v for k, v in event.items() if k != 'body'}
        short['body_bytes'] = len(event['body'].encode())
        return short

    def event(self, issue_id, event_id):
        with self.connect() as db:
            self.get(self.all(db), issue_id)
            row = db.execute('SELECT id,data FROM activity WHERE id=? AND issue_id=?', (event_id, issue_id)).fetchone()
        if row is None:
            fail('event_not_found', 'No such activity on this issue', 404)
        return dict(json.loads(row['data']), id=row['id'])

    def show(self, issue_id, offset=0, limit=50, titles=False):
        if type(offset) is not int or offset < 0 or not 1 <= limit <= 100:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        with self.connect() as db:
            items = self.all(db)
            issue = self.view(self.get(items, issue_id), items)
            rows = db.execute('SELECT id,data FROM activity WHERE issue_id=? ORDER BY id DESC LIMIT ? OFFSET ?',
                              (issue_id, limit+1, offset)).fetchall()
        return {'issue': issue, 'children': [self.view(i, items) for i in items.values() if i['parent_id'] == issue_id][:100],
                'children_total': sum(i['parent_id'] == issue_id for i in items.values()),
                'activity': [(self.titles_only if titles else (lambda e: e))(dict(json.loads(r['data']), id=r['id'])) for r in rows[:limit]],
                'next_activity_offset': offset+limit if len(rows)>limit else None}

    def children(self, issue_id, offset=0, limit=50):
        if offset < 0 or not 1 <= limit <= 100:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        with self.connect() as db:
            items = self.all(db)
        self.get(items, issue_id)
        children = [i for i in items.values() if i['parent_id'] == issue_id]
        return {'issues': [self.view(i, items) for i in children[offset:offset+limit]],
                'total': len(children), 'next_offset': offset+limit if offset+limit<len(children) else None}

    def editable(self, issue, actor):
        claim = self.active_claim(issue)
        return actor['id'] in (issue['creator'], issue['assignee'], claim['holder'] if claim else None) or (actor['kind'] == 'human' and actor.get('role') in ('owner', 'admin'))

    def validate(self, issue, items, directory):
        text(issue['title'], 'title', 300)
        text(issue['description'], 'description', 32768, True)
        if issue['status'] not in STATUSES or issue['priority'] not in PRIORITIES:
            fail('invalid_field', 'Invalid status or priority')
        labels = issue['labels']
        if not isinstance(labels, list) or len(labels) > 20:
            fail('invalid_field', 'labels must be a list of at most 20 strings')
        for label in labels:
            text(label, 'label', 64)
        if len(set(labels)) != len(labels):
            fail('invalid_field', 'Labels must be unique')
        if issue['assignee'] is not None and (not isinstance(issue['assignee'], str) or issue['assignee'] not in {p['id'] for p in directory}):
            fail('assignee_unavailable', 'Use a stable ID from the Treer directory')
        parent = issue['parent_id']
        if parent is not None:
            ident(parent)
            seen = {issue['id']}
            while parent is not None:
                if parent in seen:
                    fail('hierarchy_cycle', 'Parent hierarchy would cycle')
                seen.add(parent)
                parent = self.get(items, parent)['parent_id']
        dependencies = issue['blocked_by']
        if not isinstance(dependencies, list) or len(dependencies)>50:
            fail('invalid_field', 'blocked_by must be a list of at most 50 IDs')
        for dependency in dependencies:
            ident(dependency)
            self.get(items, dependency)
        if len(set(dependencies)) != len(dependencies):
            fail('invalid_field', 'Dependencies must be unique')
        pending, seen = list(dependencies), set()
        while pending:
            dependency = pending.pop()
            if dependency == issue['id']:
                fail('dependency_cycle', 'Dependencies would cycle')
            if dependency not in seen:
                seen.add(dependency)
                pending.extend(items[dependency]['blocked_by'])

    def mutate(self, issue_id, action, body, actor, directory, paths=None):
        allowed = ('title', 'description', 'status', 'priority', 'labels', 'assignee', 'parent_id', 'blocked_by')
        fields(body, allowed if action == 'create' else (*allowed, 'version') if action == 'update'
               else ('body', 'title') if action == 'comment' else ('action', 'seconds'))
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            now = self.clock()
            items = self.all(db)
            if action == 'create':
                issue = dict(title='', description='', status='open', priority='normal', labels=[],
                             assignee=None, parent_id=None, blocked_by=[], creator=actor['id'],
                             created_at=stamp(now), updated_at=stamp(now), version=0, claim=None, **{'id': -1})
                issue.update(body)
                self.validate(issue, items, directory)
                issue_id = db.execute('INSERT INTO issues(data) VALUES(?)', ('{}',)).lastrowid
                issue['id'] = issue_id
                old_assignee = None
                before = {}
                summary = 'Issue created'
            else:
                issue = self.get(items, issue_id)
                old_assignee = issue['assignee']
                before = dict(issue)
                claim = self.active_claim(issue)
                if action != 'comment' and claim and claim['holder'] != actor['id']:
                    fail('claim_owned', 'Another principal holds the active lease', 409)
                if action == 'update':
                    if not self.editable(issue, actor):
                        fail('forbidden', 'Only creator, assignee or human admin may edit', 403)
                    if 'version' in body and (type(body['version']) is not int or body['version'] != issue['version']):
                        fail('version_conflict', 'Reload the issue before editing', 409)
                    issue.update({k: v for k, v in body.items() if k != 'version'})
                    self.validate(issue, items, directory)
                    summary = 'Issue updated: ' + ', '.join(k for k in body if k != 'version')
                elif action == 'comment':
                    text(body.get('body'), 'comment', 8192)
                    if body.get('title') is not None:
                        title = text(body['title'], 'comment title', COMMENT_TITLE_BYTES)
                        if title != title.strip() or '\n' in title or '\r' in title:
                            fail('invalid_field', 'comment title must be one trimmed line')
                    summary = 'New comment'
                elif action == 'claim':
                    operation = body.get('action', 'acquire')
                    seconds = body.get('seconds', 900)
                    if operation not in ('acquire', 'renew', 'release') or type(seconds) is not int or not 1 <= seconds <= 86400:
                        fail('invalid_lease', 'action acquire/renew/release; seconds 1..86400')
                    if operation in ('renew', 'release') and (not claim or claim['holder'] != actor['id']):
                        fail('claim_missing', 'No active lease owned by caller', 409)
                    if operation != 'release':
                        if issue['status'] in ('done', 'closed') or issue['assignee'] not in (None, actor['id']):
                            fail('not_claimable', 'Issue must be active and unassigned or assigned to caller', 409)
                        if any(items[d]['status'] not in ('done', 'closed') for d in issue['blocked_by']):
                            fail('dependencies_unresolved', 'Resolve blocking issues before claiming', 409)
                        issue['claim'] = {'holder': actor['id'], 'expires_at': stamp(now+seconds)}
                    else:
                        issue['claim'] = None
                    summary = 'Lease ' + operation
                else:
                    fail('route_not_found', 'Unknown action', 404)
            changed = {k for k, v in body.items() if k not in ('version', 'body') and before.get(k) != v} if action == 'update' else set()
            if action == 'comment':
                mention_text, earlier = body['body'], ''
            elif action in ('create', 'update'):
                mention_text = issue['title'] + '\n' + issue['description']
                earlier = before.get('title', '') + '\n' + before.get('description', '')
            else:
                mention_text = earlier = ''
            mentions = sorted(resolve_mentions(mention_text, directory, paths) - resolve_mentions(earlier, directory, paths) - {actor['id']})
            if action in ('create', 'update') and issue['status'] in ('in_progress', 'done'):
                if any(items[d]['status'] not in ('done', 'closed') for d in issue['blocked_by']):
                    fail('dependencies_unresolved', 'Resolve blocking issues before starting or completing', 409)
            if issue['status'] in ('done', 'closed'):
                issue['claim'] = None
            issue['version'] += 1
            issue['updated_at'] = stamp(now)
            event = {'actor': actor['id'], 'kind': action, 'at': stamp(now), 'summary': summary,
                     'changes': {k: v for k, v in body.items() if k != 'body'} if action in ('create', 'update') else {},
                     'notification': {'status': 'pending' if action in ('create', 'update', 'comment') else 'not_requested'}}
            if action == 'comment':
                event['body'] = body['body']
                if body.get('title') is not None:
                    event['title'] = body['title']
            if mentions:
                event['mentions'] = mentions
            db.execute('UPDATE issues SET data=? WHERE id=?', (json.dumps(issue), issue_id))
            event_id = db.execute('INSERT INTO activity(issue_id,data) VALUES(?,?)', (issue_id, json.dumps(event))).lastrowid
            names = {p['id']: p.get('name', p['id']) for p in directory}
            notice = notice_for(action, actor, before, issue, changed, mentions, names)
            recipients = recipients_for(action, issue, old_assignee, changed, mentions, actor['id']) if notice else []
            missed = unresolved_mentions(mention_text, directory, paths) if mention_text != earlier else []
        return issue_id, event_id, event, recipients, notice, missed

    def _overflow(self, db, recipient, text, now):
        row = db.execute("SELECT pending FROM notify_windows WHERE recipient=? AND issue_id=0 AND actor=''", (recipient,)).fetchone()
        pending = json.loads(row['pending']) if row else []
        if row and pending:
            db.execute("UPDATE notify_windows SET pending=? WHERE recipient=? AND issue_id=0 AND actor=''", (json.dumps((pending + [text])[-50:]), recipient))
        else:   # no pending updates: this one starts a fresh window
            db.execute("INSERT OR REPLACE INTO notify_windows(recipient,issue_id,actor,window_start,pending,attempts) VALUES(?,0,'',?,?,0)",
                       (recipient, now, json.dumps([text])))

    def claim_notifications(self, issue_id, recipients, summary, now, actor_id, limit=20):
        """Split recipients into (send now, coalesced, throttled). Windows are per (recipient, issue, ACTOR): an event is only
        ever merged into a window opened by a send that the same actor's token already got Core to accept, so a
        digest (sent under the App identity) can never carry an actor's event to someone their Policy denies.
        Each recipient also has an hourly budget of immediate Messages; the excess goes to one per-recipient
        digest, which bounds the total regardless of how many issues are touched. The overflow digest is also sent
        under the App identity, so only an actor whom Core already let message that recipient THIS HOUR
        (notify_proven, written after a successful send) may overflow into it; anyone else is throttled (dropped)."""
        send, merged, throttled = [], [], []
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            for recipient in recipients:
                row = db.execute('SELECT window_start,pending FROM notify_windows WHERE recipient=? AND issue_id=? AND actor=?',
                                 (recipient, issue_id, actor_id)).fetchone()
                pending = json.loads(row['pending']) if row else []
                if row and (now < row['window_start'] + WINDOW or pending):
                    db.execute('UPDATE notify_windows SET pending=? WHERE recipient=? AND issue_id=? AND actor=?',
                               (json.dumps((pending + [summary])[-50:]), recipient, issue_id, actor_id))
                    merged.append(recipient)
                    continue
                hour = int(now // 3600)
                used = db.execute('SELECT immediate FROM notify_budget WHERE recipient=? AND hour=?', (recipient, hour)).fetchone()
                if used and used['immediate'] >= limit:
                    if db.execute('SELECT 1 FROM notify_proven WHERE recipient=? AND actor=? AND hour=?', (recipient, actor_id, hour)).fetchone():
                        self._overflow(db, recipient, f'#{issue_id} {summary}', now)
                        merged.append(recipient)
                    else:
                        throttled.append(recipient)
                    continue
                db.execute('INSERT INTO notify_budget(recipient,hour,immediate) VALUES(?,?,1) ON CONFLICT(recipient,hour) DO UPDATE SET immediate=immediate+1',
                           (recipient, hour))
                db.execute('INSERT OR REPLACE INTO notify_windows(recipient,issue_id,actor,window_start,pending,attempts) VALUES(?,?,?,?,?,0)',
                           (recipient, issue_id, actor_id, now, '[]'))
                send.append(recipient)
        return send, merged, throttled

    def prove_notifications(self, recipients, actor_id, now):
        """Core accepted this actor's Message to these recipients: they may use the overflow digest this hour."""
        with self.connect() as db:
            for recipient in recipients:
                db.execute('INSERT OR IGNORE INTO notify_proven(recipient,actor,hour) VALUES(?,?,?)', (recipient, actor_id, int(now // 3600)))

    def release_notifications(self, issue_id, recipients, actor_id, now):
        """A failed immediate send proved nothing: drop the claim INCLUDING anything merged into it meanwhile (it was
        never authorised by Core) and give the budget back, so the next event simply retries."""
        with self.connect() as db:
            for recipient in recipients:
                db.execute('DELETE FROM notify_windows WHERE recipient=? AND issue_id=? AND actor=?', (recipient, issue_id, actor_id))
                db.execute('UPDATE notify_budget SET immediate=MAX(0,immediate-1) WHERE recipient=? AND hour=?', (recipient, int(now // 3600)))
                db.execute('DELETE FROM notify_proven WHERE recipient=? AND actor=? AND hour=?', (recipient, actor_id, int(now // 3600)))

    def due_digests(self, now):
        with self.connect() as db:
            rows = db.execute("SELECT recipient,issue_id,actor,window_start,pending FROM notify_windows WHERE pending!='[]' AND window_start+?<=?",
                              (WINDOW, now)).fetchall()
        return [{'recipient': r['recipient'], 'issue_id': r['issue_id'], 'actor': r['actor'], 'start': r['window_start'],
                 'pending': json.loads(r['pending'])} for r in rows]

    def digest_done(self, recipient, issue_id, actor, now):
        with self.connect() as db:
            db.execute("UPDATE notify_windows SET window_start=?, pending='[]', attempts=0 WHERE recipient=? AND issue_id=? AND actor=?",
                       (now, recipient, issue_id, actor))

    def digest_failed(self, recipient, issue_id, actor, limit):
        with self.connect() as db:
            db.execute('UPDATE notify_windows SET attempts=attempts+1 WHERE recipient=? AND issue_id=? AND actor=?', (recipient, issue_id, actor))
            db.execute("UPDATE notify_windows SET pending='[]' WHERE recipient=? AND issue_id=? AND actor=? AND attempts>=?", (recipient, issue_id, actor, limit))

    def prune_windows(self, now):
        with self.connect() as db:
            db.execute("DELETE FROM notify_windows WHERE pending='[]' AND window_start+?<=?", (WINDOW, now))
            db.execute('DELETE FROM notify_budget WHERE hour < ?', (int(now // 3600) - 2,))
            db.execute('DELETE FROM notify_proven WHERE hour < ?', (int(now // 3600) - 2,))

    def unresolved_by_assignee(self, prefix='ag_', skip_labels=()):
        """Unresolved (not done/closed) issue IDs per assignee whose stable ID starts with prefix. Issues carrying
        any of skip_labels (e.g. long-lived epics) are not counted."""
        skip = {label.lower() for label in skip_labels}
        with self.connect() as db:
            items = self.all(db)
        result = {}
        for issue in sorted(items.values(), key=lambda i: i['id']):
            assignee = issue['assignee']
            if assignee and assignee.startswith(prefix) and issue['status'] not in ('done', 'closed') and not skip & {label.lower() for label in issue['labels']}:
                result.setdefault(assignee, []).append(issue['id'])
        return result

    def reminder_candidates(self, prefix='ag_', skip_labels=()):
        """(issue ID, assignee, version, updated-at epoch) of every issue that needs reminding: unresolved, assigned to an Agent, not skipped.
        The version changes on every real write (edit, comment, status/assignee change, lease), which resets the backoff."""
        skip = {label.lower() for label in skip_labels}
        with self.connect() as db:
            items = self.all(db)
        return [(i['id'], i['assignee'], i['version'], datetime.fromisoformat(i['updated_at'].replace('Z', '+00:00')).timestamp())
                for i in sorted(items.values(), key=lambda i: i['id'])
                if i['assignee'] and i['assignee'].startswith(prefix) and i['status'] not in ('done', 'closed')
                and not skip & {label.lower() for label in i['labels']}]

    def reminder_sync(self, candidates, now, first):
        """Aligns the persisted backoff with the issues: issues that stopped needing a reminder lose their state; a new one
        starts at `first` from now (so a fresh deployment does not remind anyone at once); a changed assignee or any new
        activity (version) starts over. Idempotent."""
        wanted = {issue_id: (assignee, version, updated) for issue_id, assignee, version, updated in candidates}
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            current = {row['issue_id']: row for row in db.execute('SELECT * FROM reminder_backoff')}
            for issue_id in set(current) - set(wanted):
                db.execute('DELETE FROM reminder_backoff WHERE issue_id=?', (issue_id,))
            for issue_id, (assignee, version, updated) in wanted.items():
                row = current.get(issue_id)
                if row is None:
                    start = now                       # newly tracked (also: just deployed): the first interval starts now
                elif row['assignee'] != assignee or row['version'] != version:
                    start = min(now, max(updated, now - first))   # activity: counted from when it happened (never in the future)
                else:
                    continue
                db.execute('INSERT OR REPLACE INTO reminder_backoff(issue_id,assignee,version,interval,next_due,count) VALUES(?,?,?,?,?,0)',
                           (issue_id, assignee, version, first, max(now, start + first)))

    def reminder_due(self, now):
        with self.connect() as db:
            rows = db.execute('SELECT issue_id,assignee,version,interval,count FROM reminder_backoff WHERE next_due<=? ORDER BY assignee,issue_id', (now,)).fetchall()
        return [dict(row) for row in rows]

    def reminder_advance(self, issue_ids, now, factor, maximum):
        """After a delivered reminder: the next interval is the last one times `factor`, capped at `maximum`."""
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            for issue_id in issue_ids:
                row = db.execute('SELECT interval FROM reminder_backoff WHERE issue_id=?', (issue_id,)).fetchone()
                if row is None:
                    continue
                interval = min(row['interval'] * factor, maximum)
                db.execute('UPDATE reminder_backoff SET interval=?, next_due=?, count=count+1 WHERE issue_id=?', (interval, now + interval, issue_id))

    def reminder_next_due(self, after):
        """The nearest FUTURE due time. Rows that are already due (but were not sent because the Agent was busy) must not count,
        or the scheduler would wake every second for them."""
        with self.connect() as db:
            return db.execute('SELECT MIN(next_due) FROM reminder_backoff WHERE next_due>?', (after,)).fetchone()[0]

    def reminder_states(self):
        with self.connect() as db:
            return {row['issue_id']: dict(row) for row in db.execute('SELECT * FROM reminder_backoff')}

    def reminded(self, slot, assignee):
        with self.connect() as db:
            return db.execute('SELECT 1 FROM reminders WHERE slot=? AND assignee=?', (slot, assignee)).fetchone() is not None

    def record_reminder(self, slot, assignee, issue_ids):
        with self.connect() as db:
            db.execute('INSERT OR IGNORE INTO reminders(slot,assignee,issue_ids,sent_at) VALUES(?,?,?,?)',
                       (slot, assignee, json.dumps(issue_ids), self.clock()))
            # Bounded history: only recent slots matter for de-duplication.
            db.execute('DELETE FROM reminders WHERE slot < ?', (slot - 24 * 14,))

    def notification(self, event_id, result):
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT data FROM activity WHERE id=?', (event_id,)).fetchone()
            event = json.loads(row['data'])
            event['notification'] = result
            db.execute('UPDATE activity SET data=? WHERE id=?', (json.dumps(event), event_id))
