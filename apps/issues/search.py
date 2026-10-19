"""Full-text search over issue titles, descriptions and comments (SQLite FTS5, trigram tokenizer).

The index lives in the same database and is written in the same transaction as the text it covers. Every document
records WHO wrote the text (the verified principal at write time; for a title or description, whoever changed it last)
and a snippet is plain, bounded data from an untrusted author - never an instruction, never markup.
"""
import json
import re
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
DATE = re.compile(r'\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?Z?)?')


def ensure(db):
    db.executescript('''
        CREATE TABLE IF NOT EXISTS search_docs(id INTEGER PRIMARY KEY AUTOINCREMENT, issue_id INTEGER NOT NULL, kind TEXT NOT NULL,
            event_id INTEGER NOT NULL DEFAULT 0, author TEXT NOT NULL, at REAL NOT NULL, text TEXT NOT NULL);
        CREATE UNIQUE INDEX IF NOT EXISTS search_docs_key ON search_docs(issue_id, kind, event_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(text, tokenize='trigram');
        CREATE TABLE IF NOT EXISTS search_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    ''')


def put(db, issue_id, kind, event_id, author, at, text):
    """Insert or replace one document (and its FTS row). Empty text removes it."""
    row = db.execute('SELECT id FROM search_docs WHERE issue_id=? AND kind=? AND event_id=?', (issue_id, kind, event_id)).fetchone()
    if row:
        db.execute('DELETE FROM search_fts WHERE rowid=?', (row[0],))
        db.execute('DELETE FROM search_docs WHERE id=?', (row[0],))
    if not text or not text.strip():
        return
    doc = db.execute('INSERT INTO search_docs(issue_id,kind,event_id,author,at,text) VALUES(?,?,?,?,?,?)',
                     (issue_id, kind, event_id, author, at, text)).lastrowid
    db.execute('INSERT INTO search_fts(rowid,text) VALUES(?,?)', (doc, text))


def on_write(db, action, issue, before, event, event_id, actor, now):
    """Keeps the index in step with a write. Title and description are re-indexed (with the actor as author) only when
    they changed; each comment is one document. (There is no way to edit or delete a comment or an issue.)"""
    if action in ('create', 'update'):
        for kind in ('title', 'description'):
            if action == 'create' or before.get(kind) != issue.get(kind):
                put(db, issue['id'], kind, 0, actor['id'], now, issue.get(kind) or '')
    elif action == 'comment':
        text = '\n'.join(part for part in (event.get('title'), event.get('body')) if part)
        put(db, issue['id'], 'comment', event_id, actor['id'], now, text)


def stamp_epoch(value):
    return datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


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
        created = stamp_epoch(issue['created_at']) if issue.get('created_at') else 0
        put(db, issue_id, 'title', 0, issue.get('creator', ''), created, issue.get('title') or '')
        put(db, issue_id, 'description', 0, issue.get('creator', ''), created, issue.get('description') or '')
        for event in db.execute('SELECT id,data FROM activity WHERE issue_id=? ORDER BY id', (issue_id,)).fetchall():
            data = json.loads(event['data'])
            if data.get('kind') == 'comment':
                put(db, issue_id, 'comment', event['id'], data.get('actor', ''), stamp_epoch(data['at']),
                    '\n'.join(part for part in (data.get('title'), data.get('body')) if part))
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
    if params.get('from') is not None:
        clauses.append('AND d.at >= ?'); args.append(params['from'])
    if params.get('to') is not None:
        clauses.append('AND d.at <= ?'); args.append(params['to'])
    order = 'ORDER BY rank, d.at DESC' if long_terms else 'ORDER BY d.at DESC'
    rows = db.execute(f"{sql} {' '.join(clauses)} {order} LIMIT {CANDIDATES}", args).fetchall()
    out, seen = [], set()
    for row in rows:
        issue = items.get(row['issue_id'])
        if issue is None or issue['id'] in seen:      # one result per issue: its best-ranked matching text
            continue
        if params.get('status') and issue['status'] != params['status']:
            continue
        if params.get('assignee') and issue['assignee'] != params['assignee']:
            continue
        if params.get('label') and params['label'].lower() not in {l.lower() for l in issue['labels']}:
            continue
        seen.add(issue['id'])
        out.append({'issue': {'id': issue['id'], 'title': issue['title'], 'status': issue['status'], 'assignee': issue['assignee']},
                    'match': {'kind': row['kind'], 'event_id': row['event_id'] or None, 'author': row['author'],
                              'at': datetime.fromtimestamp(row['at'], timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')},
                    'snippet': clean_snippet(row['text'], terms), 'snippet_untrusted': True})
        if len(out) >= limit:
            break
    return out
