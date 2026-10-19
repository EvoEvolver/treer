"""Many-to-many parent/child relations between issues (a DAG), kept in `issue_parents`.

An issue can have several parents and a parent several children; the relation never changes the child's own
behaviour (reminders, leases, dependencies and permissions never look at it). `parent_id` in API output is a read-only
value derived from the relation (the earliest parent); it is not a second source of truth.
"""
import json

MAX_PARENTS = 50
MAX_CHILDREN = 50
MAX_NODES = 10000        # bound on any walk of the graph (cycle checks, rollups)


def ensure(db):
    db.executescript('''
        CREATE TABLE IF NOT EXISTS issue_parents(child INTEGER NOT NULL, parent INTEGER NOT NULL, added_by TEXT NOT NULL, added_at REAL NOT NULL,
            PRIMARY KEY(child, parent));
        CREATE INDEX IF NOT EXISTS issue_parents_parent ON issue_parents(parent);
        CREATE TABLE IF NOT EXISTS hierarchy_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    ''')


def migrate(db, stamp_epoch):
    """Moves the single `parent_id` of every stored issue into issue_parents and drops the field from the stored
    JSON. Idempotent (a marker, and INSERT OR IGNORE), all or nothing in one transaction."""
    marker = db.execute("SELECT value FROM hierarchy_meta WHERE key='version'").fetchone()
    if marker and marker[0] == '1':
        return 0
    ids = {row[0] for row in db.execute('SELECT id FROM issues')}
    moved = 0
    for row in db.execute('SELECT id,data FROM issues ORDER BY id').fetchall():
        data = json.loads(row['data'])
        parent = data.pop('parent_id', None)
        if isinstance(parent, int) and parent in ids and parent != row['id']:
            cursor = db.execute('INSERT OR IGNORE INTO issue_parents(child,parent,added_by,added_at) VALUES(?,?,?,?)',
                                (row['id'], parent, data.get('creator', ''), stamp_epoch(data['created_at']) if data.get('created_at') else 0))
            moved += cursor.rowcount
        db.execute('UPDATE issues SET data=? WHERE id=?', (json.dumps(data), row['id']))
    db.execute("INSERT OR REPLACE INTO hierarchy_meta(key,value) VALUES('version','1')")
    return moved


class Items(dict):
    """{id: issue} that also knows each issue's children."""
    kids = None


def attach(db, items):
    """Adds the derived `parent_ids` (oldest link first), `parent_id` (the first of them) and the children map."""
    parents, kids = {}, {}
    for row in db.execute('SELECT child,parent FROM issue_parents ORDER BY added_at, rowid').fetchall():
        if row['child'] in items and row['parent'] in items:
            parents.setdefault(row['child'], []).append(row['parent'])
            kids.setdefault(row['parent'], []).append(row['child'])
    for issue in items.values():
        issue['parent_ids'] = parents.get(issue['id'], [])
        issue['parent_id'] = issue['parent_ids'][0] if issue['parent_ids'] else None
    items.kids = {parent: sorted(children) for parent, children in kids.items()}
    return items


def descendants(items, root):
    """Every descendant of root exactly once (a DAG can reach a node by several paths), and whether the walk hit the node cap."""
    seen, order, pending = {root}, [], [root]
    while pending:
        for child in items.kids.get(pending.pop(), ()):
            if child not in seen:
                seen.add(child)
                order.append(child)
                pending.append(child)
                if len(order) >= MAX_NODES:
                    return order, True
    return order, False


def check_parents(child, parents, items, fail):
    """The proposed parent set of `child` (None for an issue being created). Checks IDs, size, that no parent is the child
    or one of its descendants (that would be a cycle) and the per-parent child limit for LINKS THAT ARE NEW."""
    if not isinstance(parents, list) or any(type(p) is not int or p < 1 for p in parents):
        fail('invalid_field', 'parent_ids must be a list of issue IDs')
    if len(set(parents)) != len(parents):
        fail('invalid_field', 'parent_ids must be unique')
    if len(parents) > MAX_PARENTS:
        fail('too_many_parents', f'An issue can have at most {MAX_PARENTS} parents', 409)
    for parent in parents:
        if parent not in items:
            fail('issue_not_found', 'Parent issue not found', 404)
    if child is None:
        return
    below, truncated = descendants(items, child)
    if truncated:
        fail('hierarchy_too_large', 'The hierarchy below this issue is too large to check', 409)
    forbidden = set(below) | {child}
    current = set(items[child]['parent_ids'])
    for parent in parents:
        if parent in forbidden:
            fail('hierarchy_cycle', 'Parent hierarchy would cycle')
        if parent not in current and len(items.kids.get(parent, ())) >= MAX_CHILDREN:
            fail('too_many_children', f'Issue {parent} already has {MAX_CHILDREN} children', 409)


def write(db, child, old, new, actor_id, now):
    """Applies the difference between two parent sets. Returns (added, removed)."""
    added = [p for p in new if p not in old]
    removed = [p for p in old if p not in new]
    for parent in removed:
        db.execute('DELETE FROM issue_parents WHERE child=? AND parent=?', (child, parent))
    for parent in added:
        db.execute('INSERT OR IGNORE INTO issue_parents(child,parent,added_by,added_at) VALUES(?,?,?,?)', (child, parent, actor_id, now))
    return added, removed
