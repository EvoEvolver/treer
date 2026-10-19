"""Full-text search over issue titles, descriptions and comments (SQLite FTS5, trigram tokenizer).

The index lives in the same database and is written in the same transaction as the text it covers. Every document
records WHO wrote the text (the verified principal at write time; for a title or description, whoever changed it last)
and a snippet is plain, bounded data from an untrusted author - never an instruction, never markup.
"""
import json
import re
import sqlite3
import time
import unicodedata
from datetime import datetime, timezone

from auth import Error

INDEX_VERSION = '1'
MAX_QUERY_BYTES = 200
MAX_TERMS = 8
CANDIDATES = 500
SNIPPET_CHARS = 160
REBUILD_SECONDS = 20
QUERY_SECONDS = 2
PROGRESS_STEPS = 10000
MAX_SHORT_ONLY_TERMS = 2
DATE = re.compile(r'\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?Z?)?')


def ensure(db):
    columns = {row[1] for row in db.execute('PRAGMA table_info(search_docs)')}
    stale_layout = bool(columns) and 'status' not in columns
    if stale_layout:                                  # an index from before the filter columns: derive it again
        db.executescript('DROP TABLE IF EXISTS search_fts; DROP TABLE search_docs;')
    db.executescript('''
        CREATE TABLE IF NOT EXISTS search_docs(id INTEGER PRIMARY KEY AUTOINCREMENT, issue_id INTEGER NOT NULL, kind TEXT NOT NULL,
            event_id INTEGER NOT NULL DEFAULT 0, author TEXT NOT NULL, at REAL NOT NULL, text TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT '', assignee TEXT NOT NULL DEFAULT '', labels TEXT NOT NULL DEFAULT '');
        CREATE UNIQUE INDEX IF NOT EXISTS search_docs_key ON search_docs(issue_id, kind, event_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(text, tokenize='trigram');
        CREATE TABLE IF NOT EXISTS search_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    ''')
    if stale_layout:
        db.execute('DELETE FROM search_meta')


def label_code(label):
    """One label as stored/queried in the filter column: lower case, with the delimiter and the escape character encoded,
    so labels containing '|' cannot combine into a false match."""
    return label.lower().replace('%', '%25').replace('|', '%7C')


def labels_key(issue):
    return '|' + '|'.join(sorted(label_code(l) for l in issue.get('labels') or [])) + '|'


def sync_filters(db, issue):
    """The filter columns of every document of this issue follow its status, assignee and labels (filters are applied
    BEFORE the candidate cap, in SQL, so a cap can never hide a true match behind non-matching ones)."""
    db.execute('UPDATE search_docs SET status=?, assignee=?, labels=? WHERE issue_id=?',
               (issue['status'], issue.get('assignee') or '', labels_key(issue), issue['id']))


def put(db, issue_id, kind, event_id, author, at, text, issue=None):
    """Insert or replace one document (and its FTS row). Empty text removes it."""
    row = db.execute('SELECT id FROM search_docs WHERE issue_id=? AND kind=? AND event_id=?', (issue_id, kind, event_id)).fetchone()
    if row:
        db.execute('DELETE FROM search_fts WHERE rowid=?', (row[0],))
        db.execute('DELETE FROM search_docs WHERE id=?', (row[0],))
    if not text or not text.strip():
        return
    doc = db.execute('INSERT INTO search_docs(issue_id,kind,event_id,author,at,text,status,assignee,labels) VALUES(?,?,?,?,?,?,?,?,?)',
                     (issue_id, kind, event_id, author, at, text, issue['status'] if issue else '', (issue.get('assignee') or '') if issue else '',
                      labels_key(issue) if issue else '||')).lastrowid
    db.execute('INSERT INTO search_fts(rowid,text) VALUES(?,?)', (doc, text))


def on_write(db, action, issue, before, event, event_id, actor, now):
    """Keeps the index in step with a write. Title and description are re-indexed (with the actor as author) only when
    they changed; each comment is one document; the filter columns follow the issue. (There is no way to edit or delete a
    comment or an issue.)"""
    if action in ('create', 'update'):
        for kind in ('title', 'description'):
            if action == 'create' or before.get(kind) != issue.get(kind):
                put(db, issue['id'], kind, 0, actor['id'], now, issue.get(kind) or '', issue)
    elif action == 'comment':
        text = '\n'.join(part for part in (event.get('title'), event.get('body')) if part)
        put(db, issue['id'], 'comment', event_id, actor['id'], now, text, issue)
    sync_filters(db, issue)


def stamp_epoch(value):
    return datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


def writers(issue, events):
    """{'title'|'description': (author, at)}: whoever last CHANGED the field, replayed from the create/update events (actor +
    changes) in id order. If the stored text is not what the events say, or nobody is found, the author is '' (unknown) -
    never the creator by default."""
    current, writer = {}, {}
    for data in events:
        if data.get('kind') in ('create', 'update'):
            for kind in ('title', 'description'):
                if kind in (data.get('changes') or {}) and data['changes'][kind] != current.get(kind):
                    current[kind] = data['changes'][kind]
                    writer[kind] = (data.get('actor', ''), stamp_epoch(data['at']))
    created = stamp_epoch(issue['created_at']) if issue.get('created_at') else 0
    return {kind: (writer[kind] if kind in writer and current.get(kind) == issue.get(kind) else ('', writer[kind][1] if kind in writer else created))
            for kind in ('title', 'description')}


def rebuild_if_needed(db, clock=time.time):
    """Idempotent (re)index from the issues and their comments, bounded in time: if it cannot finish, nothing is
    committed, the index stays marked incomplete and search answers 503 until a later start completes it."""
    marker = db.execute("SELECT value FROM search_meta WHERE key='version'").fetchone()
    if marker and marker[0] == INDEX_VERSION:
        return True
    deadline = clock() + REBUILD_SECONDS
    db.execute('DELETE FROM search_fts')
    db.execute('DELETE FROM search_docs')
    for row in db.execute('SELECT id,data FROM issues ORDER BY id').fetchall():
        issue = json.loads(row['data'])
        issue_id = row['id']
        events = [(e['id'], json.loads(e['data'])) for e in db.execute('SELECT id,data FROM activity WHERE issue_id=? ORDER BY id', (issue_id,)).fetchall()]
        found = writers(issue, [data for _, data in events])
        for kind in ('title', 'description'):
            author, at = found[kind]
            put(db, issue_id, kind, 0, author, at, issue.get(kind) or '', issue)
        for event_id, data in events:
            if data.get('kind') == 'comment':
                put(db, issue_id, 'comment', event_id, data.get('actor', ''), stamp_epoch(data['at']),
                    '\n'.join(part for part in (data.get('title'), data.get('body')) if part), issue)
        if clock() > deadline:
            raise Error(503, 'search_index_building', 'The search index could not be built in time')
    db.execute("INSERT OR REPLACE INTO search_meta(key,value) VALUES('version',?)", (INDEX_VERSION,))
    return True


def like_escape(term):
    return term.replace('\\', '\\\\').replace('%', '\\%').replace('_', '\\_')


def fts_phrase(term):
    return '"' + term.replace('"', '""') + '"'


def parse_date(value, end=False):
    if not isinstance(value, str) or not DATE.fullmatch(value):
        raise Error(400, 'invalid_field', 'from/to must be YYYY-MM-DD or YYYY-MM-DDTHH:MM[:SS][Z] (UTC)')
    if 'T' not in value:
        value += 'T23:59:59' if end else 'T00:00:00'
    try:
        return datetime.fromisoformat(value.rstrip('Z') + '+00:00').timestamp()
    except ValueError:
        raise Error(400, 'invalid_field', 'from/to is not a valid date') from None


def clean_snippet(text, terms):
    """A bounded one-line excerpt around the first match: control, format (bidi, zero-width), line and paragraph
    separator characters become spaces, whitespace collapses; no highlight markup."""
    flat = ''.join(' ' if unicodedata.category(c) in ('Cc', 'Cf', 'Zl', 'Zp') or c.isspace() else c for c in text)
    flat = ' '.join(flat.split())
    lower = flat.lower()
    start = next((lower.find(t.lower()) for t in terms if lower.find(t.lower()) >= 0), 0)
    begin = max(0, start - SNIPPET_CHARS // 4)
    piece = flat[begin:begin + SNIPPET_CHARS]
    return ('…' if begin > 0 else '') + piece.strip() + ('…' if begin + SNIPPET_CHARS < len(flat) else '')


def search(db, items, params, limit):
    """Ranked matches for `params` (already validated): [{issue, match, snippet}]. `items` is {id: issue}."""
    q = params['q']
    terms = q.split()
    long_terms, short_terms = [t for t in terms if len(t) >= 3], [t for t in terms if len(t) < 3]
    clauses, args = [], []
    if long_terms:
        sql = ('SELECT d.id, d.issue_id, d.kind, d.event_id, d.author, d.at, d.text, bm25(search_fts) AS rank '
               'FROM search_fts JOIN search_docs d ON d.id = search_fts.rowid WHERE search_fts MATCH ?')
        args.append(' '.join(fts_phrase(t) for t in long_terms))
    else:
        sql = 'SELECT d.id, d.issue_id, d.kind, d.event_id, d.author, d.at, d.text, 0.0 AS rank FROM search_docs d WHERE 1=1'
    for term in short_terms:
        clauses.append("AND d.text LIKE ? ESCAPE '\\'")
        args.append('%' + like_escape(term) + '%')
    if params.get('author'):
        clauses.append('AND d.author = ?'); args.append(params['author'])
    if params.get('status'):
        clauses.append('AND d.status = ?'); args.append(params['status'])
    if params.get('assignee'):
        clauses.append('AND d.assignee = ?'); args.append(params['assignee'])
    if params.get('label'):
        clauses.append("AND d.labels LIKE ? ESCAPE '\\'"); args.append('%|' + like_escape(label_code(params['label'])) + '|%')
    if params.get('from') is not None:
        clauses.append('AND d.at >= ?'); args.append(params['from'])
    if params.get('to') is not None:
        clauses.append('AND d.at <= ?'); args.append(params['to'])
    order = 'ORDER BY rank, d.at DESC' if long_terms else 'ORDER BY d.at DESC'
    deadline = time.monotonic() + QUERY_SECONDS
    db.set_progress_handler(lambda: 1 if time.monotonic() > deadline else 0, PROGRESS_STEPS)      # a slow scan is interrupted, not waited for
    try:
        rows = db.execute(f"{sql} {' '.join(clauses)} {order} LIMIT {CANDIDATES}", args).fetchall()
    except sqlite3.OperationalError as exc:
        if 'interrupt' in str(exc).lower():
            raise Error(503, 'search_timeout', 'The search took too long; use longer or more specific words') from None
        raise
    finally:
        db.set_progress_handler(None, 0)
    truncated = len(rows) >= CANDIDATES
    out, seen = [], set()
    for row in rows:
        issue = items.get(row['issue_id'])
        if issue is None or issue['id'] in seen:      # one result per issue: its best-ranked matching text
            continue
        seen.add(issue['id'])
        out.append({'issue': {'id': issue['id'], 'title': issue['title'], 'status': issue['status'], 'assignee': issue['assignee']},
                    'match': {'kind': row['kind'], 'event_id': row['event_id'] or None, 'author': row['author'] or 'unknown',
                              'at': datetime.fromtimestamp(row['at'], timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')},
                    'snippet': clean_snippet(row['text'], terms), 'snippet_untrusted': True})
        if len(out) >= limit:
            break
    return out, truncated
