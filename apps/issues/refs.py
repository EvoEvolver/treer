"""`#123` cross-issue references: a backlink on the referenced issue ("referenced by"). Links only: no notification, no
side effect on either issue. Written in the same transaction as the text that contains the reference."""
import re

MAX_PER_SOURCE = 20
MAX_REFERENCED_BY = 50
TITLE_CHARS = 80
REF = re.compile(r'(?<![\w#/&.\-])#([1-9]\d{0,8})(?![\w])')


def ensure(db):
    db.executescript('''
        CREATE TABLE IF NOT EXISTS issue_refs(from_issue INTEGER NOT NULL, source TEXT NOT NULL, event_id INTEGER NOT NULL DEFAULT 0,
            to_issue INTEGER NOT NULL, author TEXT NOT NULL, at REAL NOT NULL, PRIMARY KEY(from_issue, source, event_id, to_issue));
        CREATE INDEX IF NOT EXISTS issue_refs_to ON issue_refs(to_issue, at);
        CREATE TABLE IF NOT EXISTS refs_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    ''')


def targets(text, exists, own):
    """Distinct existing issue IDs written as #N in text (never the issue itself), in order, at most MAX_PER_SOURCE."""
    found = []
    for match in REF.finditer(text or ''):
        number = int(match.group(1))
        if number != own and number not in found and exists(number):
            found.append(number)
            if len(found) >= MAX_PER_SOURCE:
                break
    return found


def put(db, from_issue, source, event_id, author, at, text, exists):
    """Replaces the references this one text (title, description or one comment) makes."""
    db.execute('DELETE FROM issue_refs WHERE from_issue=? AND source=? AND event_id=?', (from_issue, source, event_id))
    for number in targets(text, exists, from_issue):
        db.execute('INSERT OR IGNORE INTO issue_refs(from_issue,source,event_id,to_issue,author,at) VALUES(?,?,?,?,?,?)',
                   (from_issue, source, event_id, number, author, at))


def on_write(db, action, issue, before, event, event_id, actor, now, exists):
    if action in ('create', 'update'):
        for kind in ('title', 'description'):
            if action == 'create' or before.get(kind) != issue.get(kind):
                put(db, issue['id'], kind, 0, actor['id'], now, issue.get(kind) or '', exists)
    elif action == 'comment':
        put(db, issue['id'], 'comment', event_id, actor['id'], now, '\n'.join(p for p in (event.get('title'), event.get('body')) if p), exists)


def rebuild_if_needed(db, stamp_epoch, writers):
    """Idempotent derivation of every reference from the stored issues and comments (first start after the upgrade)."""
    marker = db.execute("SELECT value FROM refs_meta WHERE key='version'").fetchone()
    if marker and marker[0] == '1':
        return
    import json
    ids = {row[0] for row in db.execute('SELECT id FROM issues')}
    exists = ids.__contains__
    db.execute('DELETE FROM issue_refs')
    for row in db.execute('SELECT id,data FROM issues ORDER BY id').fetchall():
        issue = json.loads(row['data'])
        events = [(e['id'], json.loads(e['data'])) for e in db.execute('SELECT id,data FROM activity WHERE issue_id=? ORDER BY id', (row['id'],)).fetchall()]
        found = writers(issue, [data for _, data in events])
        for kind in ('title', 'description'):
            author, at = found[kind]
            put(db, row['id'], kind, 0, author, at, issue.get(kind) or '', exists)
        for event_id, data in events:
            if data.get('kind') == 'comment':
                put(db, row['id'], 'comment', event_id, data.get('actor', ''), stamp_epoch(data['at']), '\n'.join(p for p in (data.get('title'), data.get('body')) if p), exists)
    db.execute("INSERT OR REPLACE INTO refs_meta(key,value) VALUES('version','1')")


def referenced_by(db, issue_id, items):
    """Issues that mention #issue_id, newest first, one entry per referencing issue (its latest reference), at most 50.
    Titles are other principals' text: cut to a fixed length and meant to be shown as plain text."""
    rows = db.execute('SELECT r.from_issue, r.source, r.event_id, r.author, r.at FROM issue_refs r JOIN '
                      '(SELECT from_issue, MAX(rowid) AS latest FROM issue_refs WHERE to_issue=? GROUP BY from_issue) g ON r.rowid = g.latest '
                      'ORDER BY r.at DESC, r.rowid DESC LIMIT ?', (issue_id, MAX_REFERENCED_BY)).fetchall()
    out, seen = [], set()
    for row in rows:
        issue = items.get(row['from_issue'])
        if issue is None or issue['id'] in seen:
            continue
        seen.add(issue['id'])
        title = ' '.join((issue['title'] or '').split())
        out.append({'issue': {'id': issue['id'], 'title': title if len(title) <= TITLE_CHARS else title[:TITLE_CHARS - 1] + '…', 'status': issue['status']},
                    'source': row['source'], 'event_id': row['event_id'] or None, 'author': row['author'] or 'unknown', 'at': row['at']})
        if len(out) >= MAX_REFERENCED_BY:
            break
    return out
