"""Transactional issue state. Every write runs under BEGIN IMMEDIATE.

Any verified workspace principal (human or Agent) may change any issue; every change is an activity entry (who,
when, what), so history is the safeguard rather than ownership. Issues and comments are never deleted: hiding is a
restorable flag and is itself recorded in the activity.

Older databases may carry tables and issue fields of retired features (reminders, waiting_on, leases, notification
policies and digests, shipped signals, stall_* tables). They are left exactly as they are: nothing here reads,
rewrites or drops them, and the stored issue JSON keeps those fields untouched (they are only left out of API output).
"""
import json
import os
import sys
import sqlite3
import time
import unicodedata
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from auth import Error
import hierarchy
import refs
import search as fulltext
from notify import notice_for, recipients_for, resolve_mentions, unresolved_mentions

STATUSES = ('open', 'in_progress', 'blocked', 'done', 'closed', 'note')
RESOLVED = ('done', 'closed', 'note')      # nothing left to do: not unresolved, not a pending dependency
PRIORITIES = ('low', 'normal', 'high', 'urgent')
# Fields of retired features that older rows still carry. Kept in storage, never shown or acted on.
LEGACY_FIELDS = ('claim', 'waiting_on', 'waiting_by', 'waiting_at', 'rv', 'progress_at', 'progress_reason', 'progress_resets', 'resp_seen')
COMMENT_TITLE_BYTES = 200
EXCERPT_CHARS = 160


def stamp(now=None):
    return datetime.fromtimestamp(time.time() if now is None else now, timezone.utc).isoformat().replace('+00:00', 'Z')


def fail(code, message, status=400):
    raise Error(status, code, message)


def fields(body, allowed):
    if not isinstance(body, dict) or set(body) - set(allowed):
        fail('invalid_fields', 'Unknown fields or non-object JSON')


def text(value, name, maximum, empty=False):
    if not isinstance(value, str) or len(value.encode()) > maximum or (not empty and not value.strip()):
        fail('invalid_field', f'{name} must be text, at most {maximum} bytes')
    return value


def ident(value):
    if type(value) is not int or value < 1:
        fail('invalid_id', 'Issue IDs must be positive integers')
    return value


def flag(value, name):
    if value in (None, '', '0', 'false'):
        return False
    if value in ('1', 'true'):
        return True
    fail('invalid_field', f'{name} must be 1 or 0')


class Store:
    def __init__(self, path, clock=time.time):
        self.path, self.clock = str(path), clock
        self.link_hits = {}
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
                CREATE TABLE IF NOT EXISTS sessions(secret TEXT PRIMARY KEY, token TEXT NOT NULL, expires REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS wakes(target TEXT NOT NULL, issue_id INTEGER NOT NULL, at REAL NOT NULL,
                    PRIMARY KEY(target, issue_id));
            ''')
        with self.connect() as db:
            columns = {row[1] for row in db.execute('PRAGMA table_info(oauth)')}
            if 'browser' not in columns:
                db.execute('ALTER TABLE oauth ADD COLUMN browser TEXT')
                db.execute('DELETE FROM oauth')
        os.chmod(self.path, 0o600)
        self.search_ready = False
        try:
            with self.connect() as db:
                hierarchy.ensure(db)
                hierarchy.migrate(db, fulltext.stamp_epoch)
                fulltext.ensure(db)
                refs.ensure(db)
                refs.rebuild_if_needed(db, fulltext.stamp_epoch, fulltext.writers)
                self.search_ready = fulltext.rebuild_if_needed(db)
        except Error:
            print('Issues search index could not be built in time; search answers 503 until a later start finishes it', file=sys.stderr, flush=True)

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
        items = hierarchy.Items((row['id'], dict(json.loads(row['data']), id=row['id']))
                                for row in db.execute('SELECT id,data FROM issues'))
        return hierarchy.attach(db, items)

    def get(self, items, issue_id):
        if issue_id not in items:
            fail('issue_not_found', 'Issue not found', 404)
        return items[issue_id]

    def view(self, issue, items):
        result = {k: v for k, v in issue.items() if k not in LEGACY_FIELDS}
        result.setdefault('hidden', None)
        below, truncated = hierarchy.descendants(items, issue['id'])      # each descendant once, however many paths lead to it
        counted = [items[n] for n in below if items[n]['status'] != 'note' and not items[n].get('hidden')]   # notes are memory, not work
        result['progress'] = {'total': len(counted), 'truncated': truncated,
            'completed': sum(i['status'] in ('done', 'closed') for i in counted),
            'by_status': {s: sum(i['status'] == s for i in counted) for s in STATUSES}}
        result['unresolved_dependencies'] = [i for i in issue['blocked_by'] if items[i]['status'] not in RESOLVED]
        return result

    def list(self, filters):
        allowed = ('status', 'assignee', 'creator', 'label', 'parent_id', 'unresolved', 'hidden', 'limit', 'offset')
        fields(filters, allowed)
        try:
            limit, offset = int(filters.get('limit', 50)), int(filters.get('offset', 0))
        except (ValueError, TypeError):
            fail('invalid_pagination', 'Invalid pagination')
        if not 1 <= limit <= 100 or offset < 0:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        hidden = filters.get('hidden', '0')
        if hidden not in ('0', '1', 'only'):
            fail('invalid_field', 'hidden must be 0, 1 or only')
        with self.connect() as db:
            items = self.all(db)
        result = sorted(items.values(), key=lambda i: i['id'], reverse=True)
        if hidden != '1':
            result = [i for i in result if bool(i.get('hidden')) == (hidden == 'only')]
        for key in ('status', 'assignee', 'creator', 'label', 'parent_id', 'unresolved'):
            if key not in filters:
                continue
            value = filters[key]
            if key == 'unresolved':
                if flag(value, 'unresolved'):
                    result = [i for i in result if i['status'] not in RESOLVED]
            elif key == 'label':
                result = [i for i in result if value in i['labels']]
            elif key == 'parent_id':
                try:
                    parent = int(value) if value not in ('', 'null') else None
                except (ValueError, TypeError):
                    fail('invalid_id', 'Invalid parent ID')
                result = [i for i in result if (parent in i['parent_ids'] if parent is not None else not i['parent_ids'])]
            else:
                result = [i for i in result if i.get(key) == value]
        return {'issues': [self.view(i, items) for i in result[offset:offset+limit]],
                'total': len(result), 'next_offset': offset+limit if offset+limit < len(result) else None}

    @staticmethod
    def titles_only(event):
        """A comment without its text: `title` (when the author wrote one, else a one-line `excerpt` of the first 160
        characters) and `body_bytes` say what is there; the text is read on demand with `event()`. Other activity (changes, summaries) is small and unchanged."""
        if event.get('kind') != 'comment' or 'body' not in event:
            return event
        short = {k: v for k, v in event.items() if k != 'body'}
        short['body_bytes'] = len(event['body'].encode())
        if not event.get('title'):
            line = ' '.join(''.join(' ' if unicodedata.category(c) in ('Cc', 'Zl', 'Zp') else c for c in event['body'][:4 * EXCERPT_CHARS]).split())
            short['excerpt'] = line if len(line) <= EXCERPT_CHARS else line[:EXCERPT_CHARS - 1].rstrip() + '…'
        return short

    @staticmethod
    def concealed(event):
        """A hidden comment as listed: who hid it and when, without its title or text (read it with `event()`)."""
        return {k: v for k, v in event.items() if k not in ('body', 'title', 'mentions')}

    def event(self, issue_id, event_id):
        with self.connect() as db:
            self.get(self.all(db), issue_id)
            row = db.execute('SELECT id,data FROM activity WHERE id=? AND issue_id=?', (event_id, issue_id)).fetchone()
        if row is None:
            fail('event_not_found', 'No such activity on this issue', 404)
        return dict(json.loads(row['data']), id=row['id'])

    def show(self, issue_id, offset=0, limit=50, titles=False, events='all', hidden=False):
        """An issue with its activity, newest first. events='comments' lists comments only (no system entries);
        hidden comments are listed as stubs unless hidden=True."""
        if type(offset) is not int or offset < 0 or not 1 <= limit <= 100:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        if events not in ('all', 'comments'):
            fail('invalid_field', 'events must be all or comments')
        where = " AND json_extract(data,'$.kind')='comment'" if events == 'comments' else ''
        with self.connect() as db:
            items = self.all(db)
            issue = self.view(self.get(items, issue_id), items)
            rows = db.execute(f'SELECT id,data FROM activity WHERE issue_id=?{where} ORDER BY id DESC LIMIT ? OFFSET ?',
                              (issue_id, limit+1, offset)).fetchall()
            referenced = refs.referenced_by(db, issue_id, items)
        activity = []
        for row in rows[:limit]:
            event = dict(json.loads(row['data']), id=row['id'])
            if event.get('hidden') and not hidden:
                event = self.concealed(event)
            elif titles:
                event = self.titles_only(event)
            activity.append(event)
        kids = [n for n in items.kids.get(issue_id, ()) if not items[n].get('hidden')]
        return {'issue': issue, 'referenced_by': [r for r in referenced if not items[r['issue']['id']].get('hidden')],
                'children': [self.view(items[n], items) for n in kids][:100], 'children_total': len(kids),
                'activity': activity, 'next_activity_offset': offset+limit if len(rows) > limit else None}

    SEARCH_FIELDS = ('q', 'status', 'assignee', 'label', 'author', 'from', 'to', 'limit')

    def search(self, params):
        """GET /v1/search: validates every parameter, then ranks matches over titles, descriptions and comments. Results
        name who wrote the matched text and carry a bounded plain-text snippet that is untrusted author data. Hidden
        issues and hidden comments are not searched."""
        fields(params, self.SEARCH_FIELDS)
        if not self.search_ready:
            fail('search_index_building', 'The search index is not ready', 503)
        q = params.get('q')
        if not isinstance(q, str) or not q.strip() or len(q.encode()) > fulltext.MAX_QUERY_BYTES or len(q.split()) > fulltext.MAX_TERMS:
            fail('invalid_field', f'q is required: 1..{fulltext.MAX_TERMS} words, at most {fulltext.MAX_QUERY_BYTES} bytes')
        try:
            limit = int(params.get('limit', 20))
        except (TypeError, ValueError):
            fail('invalid_pagination', 'limit must be 1..50')
        if not 1 <= limit <= 50:
            fail('invalid_pagination', 'limit must be 1..50')
        if params.get('status') is not None and params['status'] not in STATUSES:
            fail('invalid_field', 'unknown status')
        for key in ('assignee', 'label', 'author'):
            if key in params and (not isinstance(params[key], str) or not 0 < len(params[key]) <= 100):
                fail('invalid_field', f'{key} must be a short string')
        clean = {k: params[k] for k in ('status', 'assignee', 'label', 'author') if k in params}
        clean['q'] = q.strip()
        clean['from'] = fulltext.parse_date(params['from']) if 'from' in params else None
        clean['to'] = fulltext.parse_date(params['to'], end=True) if 'to' in params else None
        if clean['from'] is not None and clean['to'] is not None and clean['from'] > clean['to']:
            fail('invalid_field', 'from is after to')
        if not any(len(t) >= 3 for t in clean['q'].split()) and len(clean['q'].split()) > fulltext.MAX_SHORT_ONLY_TERMS:
            fail('invalid_field', f'a query of only one- and two-character words may have at most {fulltext.MAX_SHORT_ONLY_TERMS} of them')
        with self.connect() as db:
            items = self.all(db)
            hidden = {r[0] for r in db.execute("SELECT id FROM activity WHERE json_extract(data,'$.hidden') IS NOT NULL")}
            visible = {k: v for k, v in items.items() if not v.get('hidden')}
            results, truncated = fulltext.search(db, visible, clean, limit, skip_events=hidden)
        return {'results': results, 'count': len(results), 'limit': limit, 'candidate_cap': fulltext.CANDIDATES, 'truncated': truncated}

    def children(self, issue_id, offset=0, limit=50):
        if offset < 0 or not 1 <= limit <= 100:
            fail('invalid_pagination', 'limit 1..100; offset >=0')
        with self.connect() as db:
            items = self.all(db)
        self.get(items, issue_id)
        children = [items[n] for n in items.kids.get(issue_id, ()) if not items[n].get('hidden')]
        return {'issues': [self.view(i, items) for i in children[offset:offset+limit]],
                'total': len(children), 'next_offset': offset+limit if offset+limit<len(children) else None}

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

    LINKS_PER_MINUTE = 30

    def rate_links(self, principal, now, weight=1):
        """Relation changes are limited per principal (a sliding minute), counted per link ADDED (removing links is free, so a
        full set can always be cleared), so one request that adds 50 links costs 50 and no one can churn hierarchies."""
        hits = [t for t in self.link_hits.get(principal, []) if now - t < 60]
        if len(hits) + weight > self.LINKS_PER_MINUTE:
            self.link_hits[principal] = hits
            fail('rate_limited', f'At most {self.LINKS_PER_MINUTE} parent/child changes per minute', 429)
        hits.extend([now] * weight)
        self.link_hits[principal] = hits
        if len(self.link_hits) > 1000:
            self.link_hits = {k: v for k, v in self.link_hits.items() if v and now - v[-1] < 60}

    @staticmethod
    def wanted_parents(body):
        """The parent set a request asks for: `parent_ids` (a list) or the legacy `parent_id` (an ID or null); None if neither."""
        if 'parent_id' in body and 'parent_ids' in body:
            fail('invalid_field', 'Send parent_ids or parent_id, not both')
        if 'parent_ids' in body:
            return body['parent_ids']
        if 'parent_id' in body:
            if body['parent_id'] is None:
                return []
            ident(body['parent_id'])
            return [body['parent_id']]
        return None

    @staticmethod
    def record(db, issue_id, actor_id, kind, summary, now, changes=None):
        return db.execute('INSERT INTO activity(issue_id,data) VALUES(?,?)', (issue_id, json.dumps({
            'actor': actor_id, 'kind': kind, 'at': stamp(now), 'summary': summary,
            'changes': changes or {}, 'notification': {'status': 'not_requested'}}))).lastrowid

    def link_event(self, db, issue_id, kind, other, actor_id, now, other_role=None):
        role = other_role or 'child'
        summary = (f'Child #{other} linked' if kind == 'link' else f'Child #{other} unlinked') if role == 'child' \
            else (f'Linked under #{other}' if kind == 'link' else f'Unlinked from #{other}')
        self.record(db, issue_id, actor_id, kind, summary, now, {role: other})

    def link_children(self, parent_id, body, actor):
        """POST /v1/issues/ID/children {add:[…], remove:[…]}: attach EXISTING issues under this one (or detach them). It
        changes nothing about a child but its relation; every change leaves a trace in both issues' activity."""
        fields(body, ('add', 'remove'))
        lists = {}
        for key in ('add', 'remove'):
            value = body.get(key, [])
            if not isinstance(value, list) or len(value) > hierarchy.MAX_CHILDREN or any(type(v) is not int or v < 1 for v in value):
                fail('invalid_field', f'{key} must be a list of at most {hierarchy.MAX_CHILDREN} issue IDs')
            lists[key] = list(dict.fromkeys(value))
        if not lists['add'] and not lists['remove']:
            fail('invalid_field', 'Nothing to add or remove')
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            now = self.clock()
            items = self.all(db)
            self.get(items, parent_id)
            self.rate_links(actor['id'], now, len(lists['add']))
            for child_id in lists['add']:
                child = self.get(items, child_id)
                if parent_id in child['parent_ids']:
                    continue                                                     # already linked: idempotent
                hierarchy.check_parents(child_id, [*child['parent_ids'], parent_id], items, fail)
                hierarchy.write(db, child_id, child['parent_ids'], [*child['parent_ids'], parent_id], actor['id'], now)
                self.link_event(db, parent_id, 'link', child_id, actor['id'], now)
                self.link_event(db, child_id, 'link', parent_id, actor['id'], now, 'parent')
                items = self.all(db)
            for child_id in lists['remove']:
                child = self.get(items, child_id)
                if parent_id not in child['parent_ids']:
                    continue
                hierarchy.write(db, child_id, child['parent_ids'], [p for p in child['parent_ids'] if p != parent_id], actor['id'], now)
                self.link_event(db, parent_id, 'unlink', child_id, actor['id'], now)
                self.link_event(db, child_id, 'unlink', parent_id, actor['id'], now, 'parent')
                items = self.all(db)
        return self.show(parent_id)

    def set_hidden(self, issue_id, event_id, hide, actor):
        """Hide (soft-delete) or restore an issue (event_id None) or one comment. Nothing is removed: the flag records
        who and when, an activity entry records the change, and restoring clears the flag."""
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            now = self.clock()
            items = self.all(db)
            issue = self.get(items, issue_id)
            mark = {'by': actor['id'], 'at': stamp(now)} if hide else None
            if event_id is None:
                if bool(issue.get('hidden')) != hide:
                    issue['hidden'] = mark
                    issue['version'] += 1
                    issue['updated_at'] = stamp(now)
                    db.execute('UPDATE issues SET data=? WHERE id=?', (json.dumps({k: v for k, v in issue.items() if k not in ('parent_id', 'parent_ids')}), issue_id))
                    self.record(db, issue_id, actor['id'], 'hide' if hide else 'restore', 'Issue hidden' if hide else 'Issue restored', now)
            else:
                row = db.execute('SELECT data FROM activity WHERE id=? AND issue_id=?', (event_id, issue_id)).fetchone()
                if row is None:
                    fail('event_not_found', 'No such activity on this issue', 404)
                event = json.loads(row['data'])
                if event.get('kind') != 'comment':
                    fail('not_a_comment', 'Only comments can be hidden; other activity is history', 409)
                if bool(event.get('hidden')) != hide:
                    if hide:
                        event['hidden'] = mark
                    else:
                        event.pop('hidden', None)
                    db.execute('UPDATE activity SET data=? WHERE id=?', (json.dumps(event), event_id))
                    self.record(db, issue_id, actor['id'], 'hide' if hide else 'restore',
                                f'Comment {event_id} hidden' if hide else f'Comment {event_id} restored', now, {'comment': event_id})
        return self.show(issue_id, hidden=True)

    def mutate(self, issue_id, action, body, actor, directory, paths=None):
        allowed = ('title', 'description', 'status', 'priority', 'labels', 'assignee', 'parent_id', 'parent_ids', 'blocked_by')
        fields(body, allowed if action == 'create' else (*allowed, 'version') if action == 'update' else ('body', 'title'))
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            now = self.clock()
            items = self.all(db)
            new_parents = self.wanted_parents(body) if action in ('create', 'update') else None      # None: not touching the relations
            if action == 'create':
                issue = dict(title='', description='', status='open', priority='normal', labels=[],
                             assignee=None, blocked_by=[], creator=actor['id'],
                             created_at=stamp(now), updated_at=stamp(now), version=0, **{'id': -1})
                issue.update({k: v for k, v in body.items() if k not in ('parent_id', 'parent_ids')})
                issue['parent_ids'], issue['parent_id'] = [], None
                self.validate(issue, items, directory)
                if new_parents:
                    hierarchy.check_parents(None, new_parents, items, fail)
                    self.rate_links(actor['id'], now, len(new_parents))
                issue_id = db.execute('INSERT INTO issues(data) VALUES(?)', ('{}',)).lastrowid
                issue['id'] = issue_id
                old_assignee = None
                before = {}
                summary = 'Issue created'
            else:
                issue = self.get(items, issue_id)
                old_assignee = issue['assignee']
                before = dict(issue)
                if action == 'update':
                    if 'version' in body and (type(body['version']) is not int or body['version'] != issue['version']):
                        fail('version_conflict', 'Reload the issue before editing', 409)
                    issue.update({k: v for k, v in body.items() if k not in ('version', 'parent_id', 'parent_ids')})
                    if new_parents is not None and new_parents != issue['parent_ids']:
                        hierarchy.check_parents(issue_id, new_parents, items, fail)
                        self.rate_links(actor['id'], now, len(set(new_parents) - set(issue['parent_ids'])))
                    self.validate(issue, items, directory)
                    summary = 'Issue updated: ' + ', '.join(k for k in body if k != 'version')
                elif action == 'comment':
                    text(body.get('body'), 'comment', 8192)
                    if body.get('title') is not None:
                        title = text(body['title'], 'comment title', COMMENT_TITLE_BYTES)
                        if title != title.strip() or any(unicodedata.category(c) in ('Cc', 'Zl', 'Zp') for c in title):
                            fail('invalid_field', 'comment title must be one trimmed line without control characters')
                    summary = 'New comment'
                else:
                    fail('route_not_found', 'Unknown action', 404)
            changed = {k for k, v in body.items() if k not in ('version', 'body', 'parent_id', 'parent_ids') and before.get(k) != v} if action == 'update' else set()
            link_events = []
            if new_parents is not None:
                old_parents = [] if action == 'create' else before['parent_ids']
                if new_parents != old_parents:
                    added, removed = hierarchy.write(db, issue_id, old_parents, new_parents, actor['id'], now)
                    issue['parent_ids'], issue['parent_id'] = list(new_parents), (new_parents[0] if new_parents else None)
                    if action == 'update':
                        changed.add('parent_ids')
                    link_events = [('link', p) for p in added] + [('unlink', p) for p in removed]
            if action == 'comment':
                mention_text, earlier = '\n'.join(v for v in (body.get('title'), body['body']) if v), ''
            else:
                mention_text = issue['title'] + '\n' + issue['description']
                earlier = before.get('title', '') + '\n' + before.get('description', '')
            mentions = sorted(resolve_mentions(mention_text, directory, paths) - resolve_mentions(earlier, directory, paths) - {actor['id']})
            if action != 'comment' and issue['status'] in ('in_progress', 'done') and any(items[d]['status'] not in RESOLVED for d in issue['blocked_by']):
                fail('dependencies_unresolved', 'Resolve blocking issues before starting or completing', 409)
            issue['version'] += 1
            issue['updated_at'] = stamp(now)
            event = {'actor': actor['id'], 'kind': action, 'at': stamp(now), 'summary': summary,
                     'changes': {k: v for k, v in body.items() if k != 'body'} if action in ('create', 'update') else {},
                     'notification': {'status': 'pending'}}
            if link_events:
                event['changes'] = {**{k: v for k, v in event['changes'].items() if k not in ('parent_id', 'parent_ids')}, 'parent_ids': issue['parent_ids']}
            if action == 'comment':
                event['body'] = body['body']
                if body.get('title') is not None:
                    event['title'] = body['title']
            if mentions:
                event['mentions'] = mentions
            db.execute('UPDATE issues SET data=? WHERE id=?', (json.dumps({k: v for k, v in issue.items() if k not in ('parent_id', 'parent_ids')}), issue_id))
            event_id = db.execute('INSERT INTO activity(issue_id,data) VALUES(?,?)', (issue_id, json.dumps(event))).lastrowid
            for kind, parent in link_events:        # the other end sees it too: a trace in the parent's activity (who, when, which child)
                self.link_event(db, parent, kind, issue_id, actor['id'], now)
            fulltext.on_write(db, action, issue, before, event, event_id, actor, now)
            known = {r[0] for r in db.execute('SELECT id FROM issues')}
            refs.on_write(db, action, issue, before, event, event_id, actor, now, known.__contains__)
            names = {p['id']: p.get('name', p['id']) for p in directory}
            notice = notice_for(action, actor, before, issue, changed, mentions, names, body.get('title') if action == 'comment' else None)
            recipients = recipients_for(action, issue, old_assignee, changed, mentions, actor['id']) if notice else []
            assigned = issue['assignee'] if issue['assignee'] != old_assignee and issue['assignee'] != actor['id'] else None
            missed = unresolved_mentions(mention_text, directory, paths) if mention_text != earlier else []
        return issue_id, event_id, event, recipients, notice, missed, assigned

    # -- wake-ups --------------------------------------------------------------------------------------------------
    def wake_claim(self, target, issue_id, now, window):
        """True (and the slot is taken) unless this Agent was woken about this issue less than `window` seconds ago."""
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT at FROM wakes WHERE target=? AND issue_id=?', (target, issue_id)).fetchone()
            if row and window > 0 and now - row['at'] < window:
                return False
            db.execute('INSERT OR REPLACE INTO wakes(target,issue_id,at) VALUES(?,?,?)', (target, issue_id, now))
            db.execute('DELETE FROM wakes WHERE at < ?', (now - max(window, 3600) * 4,))
        return True

    def wake_release(self, target, issue_id):
        """A wake that failed for a transient reason does not hold the dedupe slot."""
        with self.connect() as db:
            db.execute('DELETE FROM wakes WHERE target=? AND issue_id=?', (target, issue_id))

    def notification(self, event_id, result):
        """Stores the delivery result. Wake outcomes a background wake already recorded are kept, whichever write is first."""
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT data FROM activity WHERE id=?', (event_id,)).fetchone()
            event = json.loads(row['data'])
            done = {k: v for k, v in ((event.get('notification') or {}).get('wake') or {}).items() if v != 'pending'}
            event['notification'] = dict(result, wake={**result['wake'], **done}) if 'wake' in result else result
            db.execute('UPDATE activity SET data=? WHERE id=?', (json.dumps(event), event_id))

    def record_wake(self, event_id, outcome):
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT data FROM activity WHERE id=?', (event_id,)).fetchone()
            event = json.loads(row['data'])
            event.setdefault('notification', {}).setdefault('wake', {}).update(outcome)
            db.execute('UPDATE activity SET data=? WHERE id=?', (json.dumps(event), event_id))
