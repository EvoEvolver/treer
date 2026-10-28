"""Rebuild tests/fixtures/legacy-issues.db with the code the live deployment ran before the slim release.

    git -C REPO archive issues-default-cap-245 apps/issues | tar -x -C /tmp/legacy
    python3 -I tests/fixtures/make_legacy_db.py /tmp/legacy/apps/issues tests/fixtures/legacy-issues.db
"""
import json, sqlite3, sys, tempfile
from pathlib import Path
legacy, out = sys.argv[1], sys.argv[2]
sys.path.insert(0, legacy)
from store import Store
LEAD = 'ag_' + '1' * 32
PEOPLE = [{'id': 'ag_a', 'kind': 'agent', 'name': 'builder'}, {'id': 'ag_b', 'kind': 'agent', 'name': 'reviewer'},
          {'id': LEAD, 'kind': 'agent', 'name': 'leader'}, {'id': 'usr_o', 'kind': 'human', 'name': 'owner', 'role': 'owner'}]
A, B, L, O = ({k: v for k, v in p.items()} for p in PEOPLE)
now = [1_790_000_000.0]
def tick(): now[0] += 37; return now[0]
with tempfile.TemporaryDirectory() as d:
    path = Path(d) / 'issues.sqlite3'
    s = Store(path, clock=lambda: now[0], leader_agent=LEAD, release_agent=LEAD)
    m = lambda *a, **k: (tick(), s.mutate(*a, PEOPLE, **k))[1]
    epic = m(None, 'create', {'title': 'Epic: platform', 'labels': ['epic'], 'description': 'Tracks #2 and #3'}, O)[0]
    one = m(None, 'create', {'title': 'Build the thing', 'assignee': 'ag_a', 'parent_id': epic, 'priority': 'high'}, O)[0]
    two = m(None, 'create', {'title': 'Review it', 'assignee': 'ag_b', 'parent_ids': [epic], 'blocked_by': [one]}, A)[0]
    note = m(None, 'create', {'title': 'Decision record', 'status': 'note', 'description': 'We chose **SQLite**.\n\n- a\n- b'}, L)[0]
    m(one, 'claim', {'action': 'acquire', 'seconds': 86400}, A)
    m(one, 'update', {'status': 'in_progress'}, A)
    m(one, 'comment', {'title': '[decision] keep stdlib', 'body': 'Long reasoning with `code` and <script>alert(1)</script> @reviewer'}, A)
    m(one, 'comment', {'body': 'plain remark, see #%d' % note}, B)
    m(two, 'update', {'status': 'blocked', 'waiting_on': 'leader'}, B)
    m(two, 'comment', {'title': '[release] deployed canary_passed 0123456789abcdef', 'body': 'shipped'}, L)
    m(note, 'comment', {'title': '[action-builder] read this', 'body': 'x'}, O)
    for i in range(5):
        m(None, 'create', {'title': f'Bulk {i}', 'assignee': ['ag_a', 'ag_b', None][i % 3], 'status': ['open', 'done', 'closed'][i % 3]}, O)
    s.set_policy('ag_a', 'asks', 'usr_o', tick())
    s.claim_notifications(one, ['ag_b'], 'New comment', tick(), 'ag_a')
    s.prove_notifications(['ag_b'], 'ag_a', now[0])
    s.record_reminder(int(now[0] // 3600), 'ag_a', [one])
    s.reminder_sync(s.reminder_candidates(), tick(), 600)
    s.ask_push_allowed('ag_a', 'ag_b', now[0])
    with s.connect() as db: db.execute('INSERT INTO reminder_budget VALUES(?,?,1)', ('ag_a', int(now[0] // 3600)))
    with s.connect() as db:
        db.execute("INSERT INTO stall_state(issue_id,episode,version,observed,owner,dependency,owner_due,dependency_due) VALUES(?,1,3,?,?,?,?,?)", (two, now[0], 'ag_b', LEAD, now[0] + 600, now[0] + 1200))
        db.execute("INSERT INTO stall_outbox(recipient,role,entries,body,key,next_attempt,updated) VALUES('ag_b','owner','[2]','Issue #2 stalled','k1',?,?)", (now[0], now[0]))
        db.execute("INSERT INTO stall_leader_budget VALUES(?,?)", (LEAD, now[0]))
        db.execute("INSERT INTO delivery_credits VALUES(?,?,1)", (two, one))
        db.execute("INSERT INTO sessions VALUES('secret-digest','token',?)", (now[0] + 1e9,))
        tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
    con = sqlite3.connect(path)
    con.execute('PRAGMA journal_mode=DELETE')
    Path(out).unlink(missing_ok=True)
    con.execute('VACUUM INTO ?', (out,))
    con.close()
    print(json.dumps(tables))
