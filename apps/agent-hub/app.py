#!/usr/bin/env python3
"""Agent Hub: fresh, single-task coding Agents from versioned templates (stdlib only).

Each job gets a NEW worker: a `treer-launch --no-register` child process of the
hub (its own process group, fresh provider session) whose system instructions
are a template's agent.md. Workers are App-owned subtree Agents
(`/<hub>/jobs/<id>`), never Agents in the workspace Tree. The hub talks to each
worker over its loopback Agent Interface, reverse-proxies its UI under
`/workers/<id>/`, accepts the structured handoff the worker submits with a
per-job secret, then archives the job (handoff, transcript export, provider
session id), ends the worker process group and notifies the requester. Callers
chain jobs themselves by passing earlier job ids in `inputs.handoffs`, or another
hub's handoffs by address (`/<other-hub>/j-0003`), which the hub fetches once at
job creation through its workload proxy.
"""
import argparse
import contextlib
import fcntl
import hashlib
import hmac
import html
import http.client
import json
import os
import re
import secrets
import selectors
import shutil
import signal
import socket
import sqlite3
import subprocess
import threading
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import parse_qs, quote, unquote, urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

HERE = Path(__file__).resolve().parent
KEY = re.compile(r'[A-Za-z0-9][A-Za-z0-9_-]{0,63}\Z')
JOB_ID = re.compile(r'j-[0-9]{4,9}\Z')
# A handoff on another hub: `/<hub path>/j-0003` (also `<hub path>/jobs/j-0003`, a job's `address`).
HANDOFF_REF = re.compile(r'/?((?:[A-Za-z0-9][A-Za-z0-9_.-]{0,127}/){1,8}?)(?:jobs/|handoffs/)?(j-[0-9]{4,9})\Z')
SERVICE_ID = re.compile(r'svc_[A-Za-z0-9]{1,64}\Z')
PRINCIPAL = re.compile(r'[A-Za-z0-9_-]{1,128}\Z')
HARNESSES = ('codex', 'claude')
LIVE = ('queued', 'starting', 'running', 'handoff')
FINAL = ('archived', 'failed', 'stopped')
STATUSES = LIVE + FINAL
HANDOFF_STATUS = ('done', 'partial', 'blocked')
HANDOFF_SECTIONS = ('What changed', 'Verification', 'Open questions', 'Next steps')
VERDICTS = ('approve', 'changes-needed')
REVIEW_TEMPLATE = 'reviewer'          # its handoffs must carry `verdict` and implementer notes
REVIEW_NOTES = 'Notes for the implementer'
MAX_BODY = 1024 * 1024
MAX_TASK = 64 * 1024
MAX_AGENT_MD = 128 * 1024
MAX_HANDOFF = 256 * 1024
MAX_INPUT_HANDOFFS = 8
MAX_TRANSCRIPT = 8 * 1024 * 1024
MAX_PAGE = 100


def now():
    return time.time()


def iso(value):
    return None if value is None else datetime.fromtimestamp(value, timezone.utc).isoformat().replace('+00:00', 'Z')


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'))


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


class Fail(Exception):
    def __init__(self, status, code, message):
        super().__init__(message)
        self.status, self.code, self.message = status, code, message


def text_field(body, name, limit, required=True):
    value = body.get(name)
    if value is None and not required:
        return None
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > limit:
        raise Fail(400, 'invalid_' + name, f'{name} must be a non-empty string of at most {limit} bytes')
    return value


# ---------------------------------------------------------------- templates

def parse_front_matter(text):
    """`---` delimited `key: value` lines (values are single-line strings)."""
    if not text.startswith('---\n'):
        return None, text
    end = text.find('\n---', 3)
    if end < 0:
        return None, text
    front, rest = {}, text[end + 4:]
    for line in text[4:end].splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        name, sep, value = line.partition(':')
        if not sep or not re.fullmatch(r'[a-z_]{1,32}', name.strip()):
            raise Fail(400, 'invalid_front_matter', f'front matter line is not `key: value`: {line[:80]}')
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in '"\'':
            value = value[1:-1]
        front[name.strip()] = value
    return front, rest.lstrip('\n')


def template_fields(body, previous=None):
    """Validate template input; PUT inherits unspecified fields from the previous version."""
    known = {'name', 'agent_md', 'harness', 'model', 'effort', 'cwd', 'description'}
    unknown = set(body) - known
    if unknown:
        raise Fail(400, 'unknown_fields', 'unknown template fields: ' + ', '.join(sorted(unknown)))
    base = dict(previous or {})
    for name in ('agent_md', 'harness', 'model', 'effort', 'cwd', 'description'):
        if name in body:
            base[name] = body[name]
    agent_md = base.get('agent_md')
    if not isinstance(agent_md, str) or not agent_md.strip() or len(agent_md.encode()) > MAX_AGENT_MD:
        raise Fail(400, 'invalid_agent_md', f'agent_md must be non-empty text of at most {MAX_AGENT_MD} bytes')
    if base.get('harness', 'codex') not in HARNESSES:
        raise Fail(400, 'invalid_harness', 'harness must be codex or claude')
    base['harness'] = base.get('harness') or 'codex'
    for name in ('model', 'effort'):
        value = base.get(name)
        if value in ('', None):
            base[name] = None
        elif not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9._:\[\]-]{1,64}', value):
            raise Fail(400, 'invalid_' + name, f'{name} must be a short identifier')
    cwd = base.get('cwd') or 'job'
    if not isinstance(cwd, str) or not (cwd in ('job', 'home') or re.fullmatch(r'[A-Za-z0-9._/-]{1,200}', cwd)) \
            or cwd.startswith('/') or '..' in cwd.split('/'):
        raise Fail(400, 'invalid_cwd', 'cwd is job, home, or a path relative to the Host root')
    base['cwd'] = cwd
    description = base.get('description') or ''
    if not isinstance(description, str) or len(description.encode()) > 512:
        raise Fail(400, 'invalid_description', 'description is at most 512 bytes')
    base['description'] = description
    return {k: base[k] for k in ('agent_md', 'harness', 'model', 'effort', 'cwd', 'description')}


def template_file(path):
    """A seed file: front matter (harness, model, effort, cwd, description) + agent.md body."""
    front, body = parse_front_matter(Path(path).read_text())
    fields = dict(front or {})
    fields['agent_md'] = body
    return template_fields(fields)


# ---------------------------------------------------------------- handoffs

def handoff_ref(ref, local):
    """`j-0003` (this hub) or `/<hub path>/j-0003` -> (hub path or None, job id); `local` is this hub's address."""
    if not isinstance(ref, str):
        return None
    if JOB_ID.fullmatch(ref):
        return None, ref
    match = HANDOFF_REF.fullmatch(ref)
    if not match:
        return None
    hub = '/' + match.group(1).rstrip('/')
    return (None if hub == local else hub), match.group(2)


def section(body, title):
    """The text under a `## title` heading up to the next heading of the same or a higher level."""
    match = re.search(r'^(#{1,3})\s+' + re.escape(title) + r'\b[^\n]*\n', body, re.M | re.I)
    if not match:
        return None
    rest = body[match.end():]
    end = re.search(r'^#{1,%d}\s' % len(match.group(1)), rest, re.M)
    return (rest[:end.start()] if end else rest).strip()


def validate_handoff(markdown, job, hub=None):
    if not isinstance(markdown, str) or not markdown.strip():
        raise Fail(400, 'invalid_handoff', 'handoff markdown is required')
    if len(markdown.encode()) > MAX_HANDOFF:
        raise Fail(413, 'handoff_too_large', f'handoff is at most {MAX_HANDOFF} bytes')
    markdown = markdown.replace('\r\n', '\n').lstrip('﻿')
    front, body = parse_front_matter(markdown)
    problems = []
    if front is None:
        raise Fail(400, 'invalid_handoff', 'handoff must start with `---` front matter (see GET /api/handoff-format)')
    if front.get('job') != job['id']:
        problems.append(f"front matter `job` must be {job['id']}")
    if front.get('template', '').split('@')[0] != job['template']:
        problems.append(f"front matter `template` must be {job['template']}")
    if front.get('status') not in HANDOFF_STATUS:
        problems.append('front matter `status` must be done, partial or blocked')
    if not front.get('summary'):
        problems.append('front matter `summary` (one line) is required')
    if hub and front.get('hub') not in (None, hub):
        problems.append(f'front matter `hub` must be {hub}')
    reviewer = job['template'] == REVIEW_TEMPLATE
    if (reviewer or 'verdict' in front) and front.get('verdict') not in VERDICTS:
        problems.append('front matter `verdict` must be approve or changes-needed'
                        + (' (required for reviewer jobs)' if reviewer else ''))
    headings = {m.group(1).strip().lower() for m in re.finditer(r'^#{1,3}\s+(.+?)\s*#*\s*$', body, re.M)}
    for name in HANDOFF_SECTIONS + ((REVIEW_NOTES,) if reviewer else ()):
        if not any(h.startswith(name.lower()) for h in headings):
            problems.append(f'section `## {name}` is missing')
    if problems:
        raise Fail(400, 'invalid_handoff', 'handoff rejected: ' + '; '.join(problems))
    if hub and 'hub' not in front:
        # Every stored handoff names its hub, so a copy read elsewhere stays attributable.
        markdown = markdown.replace('\n', f'\nhub: {hub}\n', 1)
        front['hub'] = hub
    refs = {'branch': front.get('branch') or None,
            'commits': sorted(set(re.findall(r'\b[0-9a-f]{7,40}\b', front.get('commits', ''))))}
    if front.get('verdict'):
        refs['verdict'] = front['verdict']
    return markdown, front, refs


HANDOFF_FORMAT = '''---
job: {job}
hub: {hub}
template: {template}
status: done            # done | partial | blocked
summary: One line the caller can act on.{verdict}
branch: feat/example    # optional; the branch you committed to
commits: abc1234 def5678 # optional; space-separated hashes
---

## What changed
Files, behavior, design decisions (or "nothing" for read-only work).

## Verification
Exact commands run and their results (pass/fail counts, errors).

## Open questions
Anything the caller must decide; "none" if none.

## Next steps
What the next job should do; "none" if finished.
{notes}'''
REVIEW_FORMAT = ('\nverdict: changes-needed  # approve | changes-needed', '''
## Notes for the implementer
Written to be passed verbatim to a fresh implementer: each required change with
path:line, why, and how to verify it; "none" when the verdict is approve.
''')


def handoff_format(job, template, hub):
    verdict, notes = REVIEW_FORMAT if template == REVIEW_TEMPLATE else ('', '')
    return HANDOFF_FORMAT.format(job=job, template=template, hub=hub, verdict=verdict, notes=notes)


# ---------------------------------------------------------------- store

class Store:
    def __init__(self, root, lock=True):
        root = Path(root)
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if root.is_symlink() or root.stat().st_mode & 0o077:
            raise ValueError('state_directory_must_be_private')
        self.root = root
        self.lock = None
        if lock:
            fd = os.open(root / 'hub.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            self.lock = os.fdopen(fd, 'a')
            try:
                fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BaseException:
                self.lock.close()
                raise
        path = root / 'hub.sqlite'
        self.db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        os.chmod(path, 0o600)
        self.db.row_factory = sqlite3.Row
        self.mutex = threading.RLock()
        self.db.executescript('''
          PRAGMA journal_mode=WAL;
          CREATE TABLE IF NOT EXISTS templates (name TEXT NOT NULL, version INTEGER NOT NULL, harness TEXT NOT NULL,
            model TEXT, effort TEXT, cwd TEXT NOT NULL, description TEXT NOT NULL, agent_md TEXT NOT NULL,
            created_at REAL NOT NULL, created_by TEXT NOT NULL, PRIMARY KEY(name, version));
          CREATE TABLE IF NOT EXISTS jobs (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
            template TEXT NOT NULL, template_version INTEGER NOT NULL, task TEXT NOT NULL, inputs TEXT NOT NULL,
            model TEXT, requester_id TEXT NOT NULL, requester_kind TEXT NOT NULL, source_message TEXT UNIQUE,
            status TEXT NOT NULL, error TEXT, created_at REAL NOT NULL, started_at REAL, finished_at REAL,
            agent_id TEXT, agent_name TEXT, ui_url TEXT, cwd TEXT, session_id TEXT, prompted INTEGER NOT NULL DEFAULT 0,
            idle_since REAL, nudged_at REAL, stop_requested_at REAL, notice TEXT NOT NULL DEFAULT 'pending',
            notice_attempts INTEGER NOT NULL DEFAULT 0, cleanup TEXT);
          CREATE TABLE IF NOT EXISTS handoffs (job_id TEXT PRIMARY KEY, markdown TEXT NOT NULL, front TEXT NOT NULL,
            refs TEXT NOT NULL, submitted_by TEXT NOT NULL, submitted_at REAL NOT NULL);
          CREATE TABLE IF NOT EXISTS archives (job_id TEXT PRIMARY KEY, transcript TEXT, archived_at REAL NOT NULL);
          CREATE TABLE IF NOT EXISTS job_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL,
            at REAL NOT NULL, sender TEXT NOT NULL, mode TEXT NOT NULL, text TEXT NOT NULL, state TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS seen_messages (message_id TEXT PRIMARY KEY, at REAL NOT NULL);
          CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status);
        ''')
        # Worker process columns (agent_id/agent_name/ui_url/cleanup remain for jobs of the
        # earlier hub that created real Tree Agents).
        columns = {row[1] for row in self.db.execute('PRAGMA table_info(jobs)')}
        for name, decl in (('worker_pid', 'INTEGER'), ('worker_port', 'INTEGER'), ('worker_instance', 'TEXT'),
                           ('token_hash', 'TEXT'), ('spawned_at', 'REAL'), ('restarts', 'INTEGER NOT NULL DEFAULT 0'),
                           ('remote_handoffs', 'TEXT')):
            if name not in columns:
                self.db.execute(f'ALTER TABLE jobs ADD COLUMN {name} {decl}')

    def close(self):
        self.db.close()
        if self.lock:
            self.lock.close()

    @contextlib.contextmanager
    def tx(self):
        with self.mutex:
            self.db.execute('BEGIN IMMEDIATE')
            try:
                yield self.db
            except BaseException:
                self.db.execute('ROLLBACK')
                raise
            self.db.execute('COMMIT')

    def q(self, sql, args=()):
        with self.mutex:
            return self.db.execute(sql, args).fetchall()

    def one(self, sql, args=()):
        with self.mutex:
            return self.db.execute(sql, args).fetchone()

    # templates
    def template(self, name, version=None):
        if version is None:
            row = self.one('SELECT * FROM templates WHERE name=? ORDER BY version DESC LIMIT 1', (name,))
        else:
            row = self.one('SELECT * FROM templates WHERE name=? AND version=?', (name, version))
        return dict(row) if row else None

    def put_template(self, name, body, author, create=False):
        if not isinstance(name, str) or not KEY.fullmatch(name):
            raise Fail(400, 'invalid_name', 'template name is 1-64 of [A-Za-z0-9_-], starting alphanumeric')
        with self.tx() as db:
            previous = db.execute('SELECT * FROM templates WHERE name=? ORDER BY version DESC LIMIT 1', (name,)).fetchone()
            if create and previous:
                raise Fail(409, 'template_exists', f'template {name} exists; PUT /api/templates/{name} adds a version')
            if not create and not previous:
                raise Fail(404, 'template_not_found', f'template {name} not found')
            fields = template_fields({k: v for k, v in body.items() if k != 'name'}, dict(previous) if previous else None)
            if previous and all(previous[k] == fields[k] for k in fields):
                return dict(previous)
            version = previous['version'] + 1 if previous else 1
            db.execute('INSERT INTO templates VALUES(?,?,?,?,?,?,?,?,?,?)',
                       (name, version, fields['harness'], fields['model'], fields['effort'], fields['cwd'],
                        fields['description'], fields['agent_md'], now(), author))
        return self.template(name, version)

    def templates(self):
        return [dict(r) for r in self.q('''SELECT t.* FROM templates t JOIN (SELECT name, max(version) v FROM templates GROUP BY name) m
                                           ON t.name=m.name AND t.version=m.v ORDER BY t.name''')]

    def versions(self, name):
        return [dict(r) for r in self.q('SELECT version, created_at, created_by FROM templates WHERE name=? ORDER BY version DESC', (name,))]

    # jobs
    def job(self, job_id):
        row = self.one('SELECT * FROM jobs WHERE id=?', (job_id,))
        return dict(row) if row else None

    def create_job(self, body, requester, source_message=None, remote=None):
        """`remote` maps each non-local `inputs.handoffs` reference to its fetched snapshot (Hub.create_job)."""
        remote = remote or {}
        if not isinstance(body, dict):
            raise Fail(400, 'invalid_job', 'job body must be a JSON object')
        unknown = set(body) - {'template', 'task', 'inputs', 'model'}
        if unknown:
            raise Fail(400, 'unknown_fields', 'unknown job fields: ' + ', '.join(sorted(unknown)))
        ref = body.get('template')
        if not isinstance(ref, str):
            raise Fail(400, 'invalid_template', 'template is required (name or name@version)')
        name, _, version = ref.partition('@')
        if version and not version.isdigit():
            raise Fail(400, 'invalid_template', 'template version must be a number')
        template = self.template(name, int(version) if version else None)
        if not template:
            raise Fail(404, 'template_not_found', f'template {ref} not found')
        task = text_field(body, 'task', MAX_TASK)
        inputs = body.get('inputs') or {}
        if not isinstance(inputs, dict) or set(inputs) - {'handoffs', 'repo', 'branch', 'notes'}:
            raise Fail(400, 'invalid_inputs', 'inputs is an object with handoffs, repo, branch, notes')
        handoffs = inputs.get('handoffs') or []
        if not isinstance(handoffs, list) or len(handoffs) > MAX_INPUT_HANDOFFS or len(set(handoffs)) != len(handoffs) \
                or not all(isinstance(h, str) and (JOB_ID.fullmatch(h) or h in remote) for h in handoffs):
            raise Fail(400, 'invalid_inputs', f'inputs.handoffs is a list of at most {MAX_INPUT_HANDOFFS} distinct job ids '
                       'or other hubs\' handoff references (/<hub>/j-0003)')
        for h in handoffs:
            if h not in remote and not self.one('SELECT 1 FROM handoffs WHERE job_id=?', (h,)):
                raise Fail(409, 'input_handoff_missing', f'job {h} has no handoff yet')
        for name_ in ('repo', 'branch', 'notes'):
            value = inputs.get(name_)
            if value is not None and (not isinstance(value, str) or len(value.encode()) > (MAX_TASK if name_ == 'notes' else 512)):
                raise Fail(400, 'invalid_inputs', f'inputs.{name_} must be a string')
        model = body.get('model')
        if model is not None and (not isinstance(model, str) or not re.fullmatch(r'[A-Za-z0-9._:\[\]-]{1,64}', model)):
            raise Fail(400, 'invalid_model', 'model must be a short identifier')
        clean = {'handoffs': handoffs, **{k: inputs[k] for k in ('repo', 'branch', 'notes') if inputs.get(k)}}
        with self.tx() as db:
            if source_message:
                old = db.execute('SELECT id FROM jobs WHERE source_message=?', (source_message,)).fetchone()
                if old:
                    return self.job(old['id'])
            seq = (db.execute('SELECT coalesce(max(seq),0)+1 FROM jobs').fetchone()[0])
            job_id = f'j-{seq:04d}'
            db.execute('''INSERT INTO jobs(seq,id,template,template_version,task,inputs,model,requester_id,requester_kind,
                          source_message,status,created_at,remote_handoffs) VALUES(?,?,?,?,?,?,?,?,?,?,'queued',?,?)''',
                       (seq, job_id, template['name'], template['version'], task, encoded(clean), model,
                        requester['id'], requester['kind'], source_message, now(),
                        encoded({h: remote[h] for h in handoffs if h in remote}) if remote else None))
        return self.job(job_id)

    def update_job(self, job_id, **fields):
        with self.mutex:
            self.db.execute('UPDATE jobs SET ' + ','.join(f'{k}=?' for k in fields) + ' WHERE id=?',
                            (*fields.values(), job_id))

    def jobs(self, status=None, requester=None, limit=MAX_PAGE):
        sql, args = 'SELECT * FROM jobs WHERE 1=1', []
        if status:
            sql += ' AND status IN (%s)' % ','.join('?' * len(status))
            args += status
        if requester:
            sql += ' AND requester_id=?'
            args.append(requester)
        return [dict(r) for r in self.q(sql + ' ORDER BY seq DESC LIMIT ?', (*args, limit))]

    def handoff(self, job_id):
        row = self.one('SELECT * FROM handoffs WHERE job_id=?', (job_id,))
        return dict(row) if row else None

    def messages(self, job_id):
        return [dict(r) for r in self.q('SELECT * FROM job_messages WHERE job_id=? ORDER BY id DESC LIMIT 50', (job_id,))][::-1]

    def add_message(self, job_id, sender, mode, text, state):
        with self.mutex:
            return self.db.execute('INSERT INTO job_messages(job_id,at,sender,mode,text,state) VALUES(?,?,?,?,?,?)',
                                   (job_id, now(), sender, mode, text, state)).lastrowid

    def seen(self, message_id):
        return self.one('SELECT 1 FROM seen_messages WHERE message_id=?', (message_id,)) is not None

    def mark_seen(self, message_id):
        with self.mutex:
            self.db.execute('INSERT OR IGNORE INTO seen_messages VALUES(?,?)', (message_id, now()))
            self.db.execute('DELETE FROM seen_messages WHERE at < ?', (now() - 30 * 86400,))


# ---------------------------------------------------------------- Core CLI

class Cli:
    """The hub's own `treer` CLI (Core Messages, requester wake-ups, discovery)."""

    def __init__(self, executable, timeout=30):
        self.executable = executable
        self.timeout = timeout

    def run(self, args, stdin=None, timeout=None):
        result = subprocess.run([self.executable] + args, input=stdin, text=True, capture_output=True,
                                timeout=timeout or self.timeout)
        if result.returncode:
            raise RuntimeError((result.stderr or result.stdout or 'treer failed').strip()[-400:])
        out = result.stdout.strip()
        try:
            return json.loads(out) if out else {}
        except ValueError:
            return {'text': out}


# ---------------------------------------------------------------- worker processes

# Environment a worker must not inherit from the hub runtime: anything that would
# make treer-launch act as a registered Agent or reuse the hub's private state.
WORKER_ENV_DROP = ('AGENT_UI_', 'CODEX_UI_', 'RUST_SPIKE_', 'TREER_HUB_')
WORKER_ENV_DROP_EXACT = ('TREER_AGENT_PRIVATE_STATE_DIR', 'TREER_RUST_SPIKE_STATE_DIR')
PROMPT_INLINE = 60 * 1024   # treer-launch accepts prompts up to 64 KiB


def worker_env(base, extra):
    env = {k: v for k, v in base.items()
           if not k.startswith(WORKER_ENV_DROP) and k not in WORKER_ENV_DROP_EXACT}
    for name, value in extra.items():
        if name.startswith('GIT_') and name in env:
            continue   # an explicit Git identity in the hub's environment wins
        env[name] = value
    return env


def ais(port, method, path, body=None, timeout=10, limit=MAX_TRANSCRIPT):
    """One loopback native-client request to a worker's Agent Interface."""
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=timeout)
    try:
        data = None if body is None else encoded(body).encode()
        connection.request(method, path, body=data, headers={'Content-Type': 'application/json'} if data else {})
        response = connection.getresponse()
        raw = response.read(limit + 1)
        if len(raw) > limit:
            raise RuntimeError('worker response too large')
        try:
            value = json.loads(raw) if raw else {}
        except ValueError:
            value = {'text': raw[:200].decode('utf-8', 'replace')}
        return response.status, value
    except OSError as error:
        raise RuntimeError(f'worker interface unavailable: {error}') from error
    finally:
        connection.close()


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def stop_group(pid, proc=None, grace=15):
    """SIGTERM the worker's process group (the launcher stops its provider), then SIGKILL."""
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(pid, signal.SIGTERM)
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        if proc is not None:
            if proc.poll() is not None:
                break
        elif not alive(pid):
            break
        time.sleep(0.1)
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(pid, signal.SIGKILL)
    if proc is not None:
        with contextlib.suppress(subprocess.TimeoutExpired):
            proc.wait(5)


# ---------------------------------------------------------------- hub

class Hub:
    """Job supervisor. HTTP handlers mutate the Store; `tick()` drives the worker processes."""

    def __init__(self, store, cli, config):
        self.store, self.cli, self.config = store, cli, config
        self.base_url = config['base_url'].rstrip('/') + '/'
        self.address = config['address']
        self.log = config.get('log') or (lambda *_: None)
        self.procs = {}          # job id -> Popen of its worker launcher (this hub process's children)
        self.lock = threading.RLock()

    @property
    def hub_url(self):
        """Loopback URL a worker uses to reach this hub (handoff submission)."""
        return self.config['hub_url'].rstrip('/')

    # ---- requests from the API / Messages
    def create_job(self, body, requester, source_message=None):
        remote = {}
        inputs = body.get('inputs') if isinstance(body, dict) else None
        refs = inputs.get('handoffs') if isinstance(inputs, dict) else None
        if isinstance(refs, list) and len(refs) <= MAX_INPUT_HANDOFFS:
            normalized = []
            for ref in refs:
                parsed = handoff_ref(ref, self.address)
                if parsed is None:
                    raise Fail(400, 'invalid_inputs', f'inputs.handoffs entry {str(ref)[:80]!r} is neither a job id nor '
                               'a handoff reference /<hub>/j-0003')
                other, job_id = parsed
                if other is None:
                    normalized.append(job_id)
                    continue
                key = f'{other}/{job_id}'
                if key not in remote:
                    remote[key] = self.fetch_remote_handoff(other, job_id)
                normalized.append(key)
            body = {**body, 'inputs': {**inputs, 'handoffs': normalized}}
        job = self.store.create_job(body, requester, source_message, remote)
        self.log('job_created', job['id'])
        return job

    def fetch_remote_handoff(self, other, job_id):
        """Another hub's handoff, read once at job creation: resolve its App path to a service with the hub's
        `treer`, then GET it through the workload proxy (http_proxy) as this hub's runtime identity."""
        ref = f'{other}/{job_id}'

        def unresolved(why):
            return Fail(400, 'input_handoff_unresolved', f'input handoff {ref}: {why}')
        try:
            app = (self.cli.run(['app', 'show', other], timeout=20) or {}).get('app') or {}
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
            raise unresolved(f'no App {other} ({str(error)[:200]})')
        service = app.get('service_id')
        if not isinstance(service, str) or not SERVICE_ID.fullmatch(service):
            raise unresolved(f'{other} has no service')
        url = f'http://{service}.treer/api/handoffs/{quote(job_id)}?format=markdown'
        try:
            with build_opener(NoRedirect()).open(Request(url, headers={'Accept': 'text/markdown'}), timeout=30) as response:
                markdown = response.read(MAX_HANDOFF + 1).decode('utf-8', 'strict')
        except HTTPError as error:
            detail = ''
            with contextlib.suppress(Exception):
                detail = json.loads(error.read(65536))['error']['message']
            raise unresolved(f'{other} answered HTTP {error.code} {detail}'.strip())
        except (OSError, ValueError) as error:
            raise unresolved(f'{other} ({service}) is unreachable: {str(error)[:200]}')
        if len(markdown.encode()) > MAX_HANDOFF:
            raise unresolved('handoff is too large')
        try:
            front, _ = parse_front_matter(markdown.replace('\r\n', '\n'))
        except Fail:
            front = None
        if not front or front.get('job') != job_id:
            raise unresolved(f'{other} did not return a hub handoff for {job_id}')
        self.log('remote_handoff_fetched', ref, service)
        return {'markdown': markdown, 'hub': other, 'service_id': service,
                'template': front.get('template', ''), 'fetched_at': now()}

    def worker_principal(self, job_id, token):
        """The per-job secret in the worker's environment (TREER_HUB_TOKEN)."""
        job = self.store.job(job_id) if isinstance(job_id, str) and JOB_ID.fullmatch(job_id) else None
        if not job or not job['token_hash'] or not isinstance(token, str) or len(token) > 256 \
                or not hmac.compare_digest(digest(token), job['token_hash']):
            raise Fail(401, 'invalid_job_token', 'X-Hub-Job-Token does not match this job\'s live worker')
        return {'id': 'worker:' + job_id, 'kind': 'worker'}

    def submit_handoff(self, job_id, markdown, principal):
        job = self.require_job(job_id)
        if not (principal['kind'] == 'human' or (principal['kind'] == 'worker' and principal['id'] == 'worker:' + job_id)):
            raise Fail(403, 'handoff_forbidden', 'only the job\'s worker (X-Hub-Job-Token) or a human may submit its handoff')
        if job['status'] not in ('starting', 'running') and not (job['status'] == 'queued' and principal['kind'] == 'human'):
            raise Fail(409, 'job_not_running', f"job {job_id} is {job['status']}; its handoff can no longer change")
        markdown, front, refs = validate_handoff(markdown, job, self.address)
        with self.store.tx() as db:
            current = db.execute('SELECT status FROM jobs WHERE id=?', (job_id,)).fetchone()['status']
            if current not in ('queued', 'starting', 'running'):
                raise Fail(409, 'job_not_running', f'job {job_id} is {current}')
            db.execute('INSERT OR REPLACE INTO handoffs VALUES(?,?,?,?,?,?)',
                       (job_id, markdown, encoded(front), encoded(refs), principal['id'], now()))
            db.execute("UPDATE jobs SET status='handoff' WHERE id=?", (job_id,))
        self.log('handoff_accepted', job_id)
        return {'job': job_id, 'status': 'handoff', 'accepted': True, 'handoff_status': front['status']}

    def request_stop(self, job_id, principal):
        job = self.require_job(job_id)
        if job['status'] in FINAL or job['status'] == 'handoff':
            return self.job_view(job)
        if job['status'] == 'queued':
            self.finish(job, 'stopped', 'stopped before start')
        elif not job['stop_requested_at']:
            self.store.update_job(job_id, stop_requested_at=now())
            if job['prompted'] and self.worker_port(job):
                try:
                    self.prompt(job, self.stop_prompt(job), f"hub-{job_id}-stop", steer=True)
                except RuntimeError as error:
                    # The grace timeout still stops it.
                    self.log('stop_prompt_failed', job_id, str(error)[:200])
        return self.job_view(self.store.job(job_id))

    def send_message(self, job_id, text, mode, sender):
        job = self.require_job(job_id)
        if mode not in ('queue', 'steer'):
            raise Fail(400, 'invalid_mode', 'mode is queue or steer')
        if job['status'] in FINAL:
            raise Fail(409, 'job_finished', f"job {job_id} is {job['status']}; start a new job with inputs.handoffs")
        mid = self.store.add_message(job_id, sender, mode, text, 'pending')
        state = 'pending'   # delivered right after the first prompt
        if job['prompted'] and job['status'] in ('running', 'handoff') and self.worker_port(job):
            state = self.deliver(job, {'id': mid, 'sender': sender, 'mode': mode, 'text': text})
        if state == 'failed':
            raise Fail(502, 'forward_failed', 'the worker did not accept the prompt')
        return {'job': job_id, 'state': state}

    def deliver(self, job, message):
        body = f"[Agent Hub message for job {job['id']} from {message['sender']}]\n{message['text']}"
        try:
            self.prompt(job, body, f"hub-{job['id']}-m{message['id']}", steer=message['mode'] == 'steer')
            state = 'delivered'
        except RuntimeError as error:
            self.log('forward_failed', job['id'], str(error)[:200])
            state = 'failed'
        with self.store.mutex:
            self.store.db.execute('UPDATE job_messages SET state=? WHERE id=?', (state, message['id']))
        return state

    def require_job(self, job_id):
        job = self.store.job(job_id) if isinstance(job_id, str) and JOB_ID.fullmatch(job_id) else None
        if not job:
            raise Fail(404, 'job_not_found', f'job {job_id} not found')
        return job

    # ---- prompts
    HANDOFF_CMD = ('curl -fsS -X POST -H "X-Hub-Job-Token: $TREER_HUB_TOKEN" -H \'Content-Type: text/markdown\' '
                   '--data-binary @handoff.md "$TREER_HUB_URL/api/jobs/$TREER_HUB_JOB/handoff"')
    RECORD_CMD = 'curl -fsS -H "X-Hub-Job-Token: $TREER_HUB_TOKEN" "$TREER_HUB_URL/api/jobs/$TREER_HUB_JOB"'

    def first_prompt(self, job, template):
        inputs = json.loads(job['inputs'])
        parts = [f"# Agent Hub job {job['id']} ({template['name']}@{template['version']})", '',
                 'You are a fresh worker created by the Agent Hub for this one job. Do the task below, then submit a handoff. '
                 'The handoff is the only thing the caller reads; after it is accepted this worker is archived and its '
                 'process ends.', '', '## Task', '', job['task'].strip(), '']
        extra = [f'- {k}: {inputs[k]}' for k in ('repo', 'branch') if inputs.get(k)]
        if extra or inputs.get('notes'):
            parts += ['## Inputs', ''] + extra + ([f"- notes:\n\n{inputs['notes'].strip()}"] if inputs.get('notes') else []) + ['']
        remote = json.loads(job.get('remote_handoffs') or '{}')
        for ref in inputs.get('handoffs', []):
            if ref in remote:
                # Fetched from another hub at job creation; labelled with its full reference.
                parts += [f"## Input handoff {ref} ({remote[ref]['template'] or 'template unknown'}; from hub {remote[ref]['hub']})", '',
                          remote[ref]['markdown'].strip(), '']
                continue
            handoff = self.store.handoff(ref)
            source = self.store.job(ref)
            parts += [f"## Input handoff {self.address}/{ref} ({source['template']}@{source['template_version']})", '',
                      handoff['markdown'].strip() if handoff else '(missing)', '']
        parts += ['## This job', '',
                  f"- Job id: {job['id']} (also `$TREER_HUB_JOB`)",
                  f"- Address: {self.address}/jobs/{job['id']} (caller messages arrive here as prompts)",
                  f"- Job record: `{self.RECORD_CMD}`",
                  '- `TREER_HUB_URL`, `TREER_HUB_JOB` and `TREER_HUB_TOKEN` are in your environment; the token is this '
                  'job\'s secret: use it only in the commands below and never print it.',
                  '', '## Submit the handoff (last step, once)', '',
                  'Write `handoff.md` in exactly this format (front matter + the four sections):', '',
                  '```markdown', handoff_format(job['id'], template['name'], self.address).strip(), '```', '',
                  'Then submit it; a 400 response explains what to fix, so fix and resubmit:', '',
                  '```sh', self.HANDOFF_CMD, '```', '',
                  'If you cannot finish, submit with `status: partial` or `status: blocked` and say why.']
        text = '\n'.join(parts) + '\n'
        if len(text.encode()) <= PROMPT_INLINE:
            return text
        path = self.worker_dir(job) / 'job-prompt.md'
        path.write_text(text)
        return (f"# Agent Hub job {job['id']} ({template['name']}@{template['version']})\n\n"
                f"Your job description (task, inputs, input handoffs and how to submit the handoff) is too large for one "
                f"prompt. Read all of `{path}` now, then do exactly what it says.\n")

    def resume_prompt(self, job):
        return (f"The Agent Hub restarted your worker process for job {job['id']} on this same session. Continue the task "
                f"where you left off (an interrupted command may need to be rerun) and submit the handoff when done:\n\n"
                f"{self.HANDOFF_CMD}\n")

    def stop_prompt(self, job):
        return (f"STOP: job {job['id']} is being stopped by its caller. Stop working now, do not start anything new, "
                f"and within {self.config['stop_grace']} seconds submit an interruption handoff with `status: partial` "
                f"(what is done, what is not, how to continue):\n\n{self.HANDOFF_CMD}\n")

    def nudge_prompt(self, job):
        return (f"Job {job['id']}: you are idle and no handoff was received. If the task is done, submit the handoff now; "
                f"if you are stuck, submit it with `status: blocked` and the reason:\n\n{self.HANDOFF_CMD}\n")

    def prompt(self, job, text, operation, steer=False):
        port = self.worker_port(job)
        if not port:
            raise RuntimeError('worker is not running')
        status, body = ais(port, 'POST', '/v1/prompts',
                           {'text': text, 'delivery': 'steer' if steer else 'queue', 'operation_id': operation})
        if status not in (200, 202) or body.get('accepted') is not True:
            raise RuntimeError(f'worker refused the prompt: HTTP {status} {encoded(body)[:200]}')

    # ---- worker processes
    def worker_dir(self, job):
        path = self.store.root / 'workers' / job['id']
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        return path

    def worker_port(self, job):
        """The live worker's loopback AIS port, or None."""
        proc = self.procs.get(job['id'])
        if proc is None or proc.poll() is not None:
            return None
        return self.store.job(job['id'])['worker_port']

    def job_cwd(self, job, template):
        if template['cwd'] == 'home':
            path = self.config['host_root']
        elif template['cwd'] != 'job':
            path = os.path.join(self.config['host_root'], template['cwd'])
        else:
            path = Path(self.config['jobs_root']) / job['id']
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
            path = str(path)
        if os.path.relpath(path, self.config['host_root']).startswith('..'):
            raise RuntimeError('worker directory is outside the Host root')
        return str(path)

    def instructions_file(self, template):
        path = self.store.root / 'instructions' / f"{template['name']}@{template['version']}.md"
        if not path.exists():
            path.parent.mkdir(mode=0o700, exist_ok=True)
            tmp = path.with_suffix('.tmp')
            tmp.write_text(template['agent_md'])
            os.replace(tmp, path)
        return str(path)

    def spawn(self, job):
        """Start the job's worker launcher as this hub's child, resuming its session if it has one."""
        template = self.store.template(job['template'], job['template_version'])
        cwd = job['cwd'] or self.job_cwd(job, template)
        directory = self.worker_dir(job)
        state = directory / 'launcher'
        state.mkdir(mode=0o700, exist_ok=True)
        port_file = directory / 'port'
        port_file.unlink(missing_ok=True)
        token = secrets.token_urlsafe(32)
        instance = f"hub-{job['id']}-{secrets.token_hex(6)}"
        argv = [self.config['launcher'], '--harness', template['harness'], '--instructions', self.instructions_file(template)]
        if job['model'] or template['model']:
            argv += ['--model', job['model'] or template['model']]
        if template['effort']:
            argv += ['--effort', template['effort']]
        argv += ['--no-register', '--state-dir', str(state), '--listen', '127.0.0.1:0', '--port-file', str(port_file)]
        if job['session_id']:
            argv += ['--session-id', job['session_id']]
        name = f"{self.address.strip('/')}/jobs/{job['id']}"
        env = worker_env(os.environ, {
            'TREER_HUB_URL': self.hub_url, 'TREER_HUB_JOB': job['id'], 'TREER_HUB_TOKEN': token,
            'AGENT_UI_INSTANCE_ID': instance,
            'GIT_AUTHOR_NAME': name, 'GIT_COMMITTER_NAME': name,
            'GIT_AUTHOR_EMAIL': f"{job['id']}.{self.config.get('app_id') or 'agent-hub'}@agents.treer.local",
            'GIT_COMMITTER_EMAIL': f"{job['id']}.{self.config.get('app_id') or 'agent-hub'}@agents.treer.local"})
        log_path = directory / 'launcher.log'
        if log_path.exists() and log_path.stat().st_size > 4 * 1024 * 1024:
            os.replace(log_path, directory / 'launcher.log.1')
        with open(log_path, 'ab') as log:
            log.write(f'--- {iso(now())} spawn {instance}\n'.encode())
            log.flush()
            proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                    start_new_session=True, close_fds=True)
        self.procs[job['id']] = proc
        self.store.update_job(job['id'], status='starting', cwd=cwd, worker_pid=proc.pid, worker_port=None,
                              worker_instance=instance, token_hash=digest(token), spawned_at=now())
        self.log('worker_spawned', job['id'], proc.pid, 'resume' if job['session_id'] or job['prompted'] else 'new')
        return proc

    def kill(self, job):
        """Terminate the job's worker process group; idempotent."""
        proc = self.procs.pop(job['id'], None)
        if proc is not None:
            if proc.poll() is None:
                stop_group(proc.pid, proc, self.config.get('kill_grace', 15))
            else:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.killpg(proc.pid, signal.SIGKILL)
        self.store.update_job(job['id'], worker_pid=None, worker_port=None, token_hash=None)

    def log_tail(self, job):
        with contextlib.suppress(OSError):
            data = (self.worker_dir(job) / 'launcher.log').read_bytes()[-600:]
            return data.decode('utf-8', 'replace').strip().replace('\n', ' | ')[-400:]
        return ''

    def recover(self):
        """Hub start: stop orphaned workers of a previous hub process; live jobs are respawned on their sessions."""
        for job in self.store.jobs(status=list(LIVE), limit=1000):
            if job['agent_id'] and not job['worker_instance']:
                # A worker from the hub version that created real Tree Agents.
                self.finish(job, 'failed', 'hub upgraded to process workers; the legacy worker Agent was deleted',
                            cleanup='pending')
                continue
            if job['worker_pid']:
                if job['worker_port']:
                    try:
                        status, manifest = ais(job['worker_port'], 'GET', '/v1/manifest', timeout=3)
                    except RuntimeError:
                        status, manifest = 0, {}
                    if status == 200 and manifest.get('instance_id') == job['worker_instance']:
                        self.save_transcript(job)
                        stop_group(job['worker_pid'], None, self.config.get('kill_grace', 15))
                        self.log('orphan_worker_stopped', job['id'], job['worker_pid'])
                self.store.update_job(job['id'], worker_pid=None, worker_port=None, token_hash=None)
            if job['status'] == 'running':
                self.store.update_job(job['id'], status='starting', idle_since=None)
                self.log('worker_resume_scheduled', job['id'])

    def shutdown(self):
        """Hub stop: end every worker; live jobs resume on the next hub start."""
        for job_id in list(self.procs):
            job = self.store.job(job_id)
            self.save_transcript(job)
            self.kill(job)

    # ---- lifecycle
    def tick(self):
        """One supervisor pass. Each step is idempotent; a failure is retried next tick."""
        with self.lock:
            for job in self.store.jobs(status=['handoff', 'starting', 'running'], limit=1000):
                try:
                    if job['status'] == 'handoff':
                        self.archive(job)
                    elif job['status'] == 'starting':
                        self.advance_start(job)
                    else:
                        self.watch(job)
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    self.log('tick_error', job['id'], str(error)[:200])
            active = len(self.store.jobs(status=['starting', 'running', 'handoff'], limit=1000))
            for job in reversed(self.store.jobs(status=['queued'], limit=1000)):
                if active >= self.config['concurrency']:
                    break
                try:
                    self.start(job)
                    active += 1
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    self.log('start_error', job['id'], str(error)[:200])
                    self.finish(self.store.job(job['id']), 'failed', 'worker start failed: ' + str(error)[:300])
            for job in self.store.jobs(status=list(FINAL), limit=1000):
                if job['notice'] == 'pending' or (job['cleanup'] or '').startswith('pending'):
                    self.complete(job)

    def start(self, job):
        template = self.store.template(job['template'], job['template_version'])
        cwd = self.job_cwd(job, template)
        self.store.update_job(job['id'], status='starting', started_at=now(), cwd=cwd)
        self.spawn(self.store.job(job['id']))

    def advance_start(self, job):
        """Spawn (or respawn) the worker, then prompt once its interface is ready."""
        if job['stop_requested_at'] and not job['prompted']:
            return self.terminate(job, 'stopped', 'stopped while starting')
        proc = self.procs.get(job['id'])
        if proc is None:
            self.spawn(job)
            return
        if proc.poll() is not None:
            return self.crashed(job, proc.returncode)
        if not job['worker_port']:
            with contextlib.suppress(OSError, ValueError):
                port = int((self.worker_dir(job) / 'port').read_text().strip())
                self.store.update_job(job['id'], worker_port=port)
                job = self.store.job(job['id'])
        ready = False
        if job['worker_port']:
            with contextlib.suppress(RuntimeError):
                status, health = ais(job['worker_port'], 'GET', '/api/health', timeout=3)
                ready = status == 200 and health.get('ready') is True
        if not ready:
            if now() - (job['spawned_at'] or now()) > self.config['start_timeout']:
                self.terminate(job, 'failed', 'worker never became ready: ' + self.log_tail(job))
            return
        template = self.store.template(job['template'], job['template_version'])
        resumed = bool(job['prompted'])
        try:
            if resumed:
                self.prompt(job, self.resume_prompt(job), f"hub-{job['id']}-resume-{job['worker_instance']}")
            else:
                self.prompt(job, self.first_prompt(job, template), f"hub-{job['id']}-first")
        except RuntimeError as error:
            if now() - job['spawned_at'] > self.config['start_timeout']:
                self.terminate(job, 'failed', 'worker never accepted its first prompt: ' + str(error)[:200])
            return
        self.store.update_job(job['id'], status='running', prompted=1, idle_since=None)
        self.log('worker_running', job['id'], 'resumed' if resumed else 'first prompt')
        self.refresh_session(self.store.job(job['id']))
        for message in self.store.messages(job['id']):
            if message['state'] == 'pending':
                self.deliver(self.store.job(job['id']), message)

    def refresh_session(self, job):
        if job['session_id'] or not self.worker_port(job):
            return
        with contextlib.suppress(RuntimeError):
            status, state = ais(job['worker_port'], 'GET', '/api/state')
            session = ((state.get('thread') or {}).get('id')) if status == 200 else None
            if isinstance(session, str) and session:
                self.store.update_job(job['id'], session_id=session)

    def crashed(self, job, code):
        self.procs.pop(job['id'], None)
        if job['worker_pid']:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(job['worker_pid'], signal.SIGKILL)   # what is left of its process group
        tail = self.log_tail(job)
        self.log('worker_exited', job['id'], code, tail[-200:])
        if job['restarts'] < self.config.get('max_restarts', 2):
            self.store.update_job(job['id'], status='starting', restarts=job['restarts'] + 1, worker_pid=None,
                                  worker_port=None, token_hash=None, idle_since=None)
            return
        self.terminate(job, 'failed', f"worker exited (code {code}) without a handoff after {job['restarts']} restarts: {tail}")

    def watch(self, job):
        t = now()
        if job['stop_requested_at'] and t - job['stop_requested_at'] > self.config['stop_grace']:
            return self.terminate(job, 'stopped', 'stopped by request; no interruption handoff within the grace period')
        if t - job['started_at'] > self.config['max_runtime']:
            return self.terminate(job, 'failed', 'exceeded the maximum job runtime')
        proc = self.procs.get(job['id'])
        if proc is None:
            return self.store.update_job(job['id'], status='starting')
        if proc.poll() is not None:
            return self.crashed(job, proc.returncode)
        self.refresh_session(job)
        try:
            status, state = ais(job['worker_port'], 'GET', '/v1/status', timeout=5)
        except RuntimeError as error:
            self.log('status_unavailable', job['id'], str(error)[:200])
            return
        idle = status == 200 and state.get('status') == 'idle' and not state.get('queue_length')
        if not idle:
            if job['idle_since']:
                self.store.update_job(job['id'], idle_since=None)
            return
        if not job['idle_since']:
            return self.store.update_job(job['id'], idle_since=t)
        idle_for = t - job['idle_since']
        if idle_for > self.config['stall'] and not job['nudged_at']:
            self.prompt(job, self.nudge_prompt(job), f"hub-{job['id']}-nudge")
            self.store.update_job(job['id'], nudged_at=t, idle_since=None)
        elif idle_for > self.config['stall'] and job['nudged_at']:
            self.terminate(job, 'failed', 'worker stalled: idle without a handoff after a reminder')

    def archive(self, job):
        final = 'stopped' if job['stop_requested_at'] else 'archived'
        self.terminate(job, final, None)

    def save_transcript(self, job):
        """Store the live worker's transcript export and session id; False if it is not reachable."""
        port = job['worker_port'] if job['worker_pid'] else None
        if not port:
            return False
        try:
            status, exported = ais(port, 'GET', '/v1/transcript?page=0&limit=1000', timeout=30)
            if status != 200:
                return False
            status, state = ais(port, 'GET', '/api/state', timeout=10)
        except RuntimeError:
            return False
        session = (state.get('thread') or {}).get('id') if status == 200 else None
        if isinstance(session, str) and session and not job['session_id']:
            self.store.update_job(job['id'], session_id=session)
        exported['session_id'] = session or job['session_id']
        transcript = encoded(exported)
        if len(transcript.encode()) > MAX_TRANSCRIPT:
            transcript = encoded({'truncated': True, 'bytes': len(transcript.encode()), 'session_id': exported['session_id']})
        with self.store.mutex:
            self.store.db.execute('INSERT OR REPLACE INTO archives VALUES(?,?,?)', (job['id'], transcript, now()))
        return True

    def terminate(self, job, status, error):
        """Archive what exists (transcript, session id), end the worker process group, then finish."""
        saved = self.worker_port(job) and self.save_transcript(self.store.job(job['id']))
        if not saved and not self.store.one('SELECT 1 FROM archives WHERE job_id=?', (job['id'],)):
            with self.store.mutex:
                self.store.db.execute('INSERT OR REPLACE INTO archives VALUES(?,?,?)',
                                      (job['id'], encoded({'unavailable': 'the worker was not running'}), now()))
        self.kill(job)
        directory = self.store.root / 'workers' / job['id']
        if directory.is_dir():
            # The provider session itself stays in the provider's storage (session id above).
            for child in directory.iterdir():
                if not child.name.startswith('launcher.log'):
                    shutil.rmtree(child) if child.is_dir() else child.unlink()
        self.finish(job, status, error)

    def finish(self, job, status, error, cleanup=None):
        self.store.update_job(job['id'], status=status, error=error, finished_at=now(), cleanup=cleanup)
        self.log('job_finished', job['id'], status)
        self.complete(self.store.job(job['id']))

    def complete(self, job):
        """Notify the requester (and delete a legacy worker Agent); retried until done."""
        if job['cleanup'] and job['cleanup'].startswith('pending') and job['agent_id']:
            try:
                self.cli.run(['agent', 'admin', 'delete', job['agent_id']], timeout=60)
                self.store.update_job(job['id'], cleanup='deleted')
            except (RuntimeError, subprocess.TimeoutExpired) as exc:
                self.store.update_job(job['id'], cleanup='deleted' if 'not_found' in str(exc) else 'pending: ' + str(exc)[:200])
        job = self.store.job(job['id'])
        if job['notice'] != 'pending':
            return
        handoff = self.store.handoff(job['id'])
        front = json.loads(handoff['front']) if handoff else {}
        text = (f"[agent-hub] job {job['id']} ({job['template']}@{job['template_version']}) {job['status']}"
                + (f" — handoff status: {front.get('status')}; {front.get('summary', '')}" if handoff else f" — no handoff ({job['error']})")
                + f"\nRead the handoff: curl -fsS {self.base_url}api/handoffs/{job['id']}?format=markdown"
                + f"\nChain the next job with inputs.handoffs: [\"{job['id']}\"] (see {self.base_url})")
        try:
            self.cli.run(['message', 'send', '--to', job['requester_id'], '--from-endpoint', f"jobs/{job['id']}",
                          '--idempotency-key', 'agent-hub-notice-' + digest(self.address + job['id']), '--stdin-envelope'],
                         stdin=encoded({'body': text}))
            state = 'sent'
        except (RuntimeError, subprocess.TimeoutExpired) as exc:
            attempts = job['notice_attempts'] + 1
            self.store.update_job(job['id'], notice_attempts=attempts, notice='pending' if attempts < 5 else 'failed: ' + str(exc)[:200])
            return
        if job['requester_kind'] == 'agent' and job['requester_id'].startswith('ag_'):
            try:
                self.cli.run(['agent', 'prompt', job['requester_id'], text])
                state = 'sent+woken'
            except (RuntimeError, subprocess.TimeoutExpired):
                state = 'sent; wake failed'
        self.store.update_job(job['id'], notice=state)

    # ---- Core Messages to the App root / jobs/<id>
    def handle_delivery(self, delivery):
        message, recipient = delivery['message'], delivery.get('recipient') or {}
        mid, sender = message['message_id'], message.get('sender') or {}
        if self.store.seen(mid):
            return
        endpoint = recipient.get('app_endpoint') or ''
        body = message.get('body') or ''
        principal = {'id': sender.get('id'), 'kind': sender.get('kind')}
        own = principal['id'] == self.config['app_id'] or body.startswith('[agent-hub]')
        if not own and isinstance(principal['id'], str) and PRINCIPAL.fullmatch(principal['id']) \
                and principal['kind'] in ('agent', 'human'):
            if not endpoint:
                self.root_message(mid, body, principal)
            elif endpoint.startswith('jobs/') and JOB_ID.fullmatch(endpoint[5:]):
                self.job_message(mid, endpoint[5:], body, principal)
        self.store.mark_seen(mid)

    def reply(self, mid, to, text, endpoint=None):
        args = ['message', 'send', '--to', to, '--context', mid, '--idempotency-key', 'agent-hub-reply-' + digest(mid), '--stdin-envelope']
        if endpoint:
            args += ['--from-endpoint', endpoint]
        try:
            self.cli.run(args, stdin=encoded({'body': '[agent-hub] ' + text}))
        except (RuntimeError, subprocess.TimeoutExpired) as exc:
            self.log('reply_failed', mid, str(exc)[:200])

    def root_message(self, mid, body, principal):
        try:
            request = json.loads(body)
            job = self.create_job(request, principal, source_message=mid)
            self.reply(mid, principal['id'], f"job {job['id']} queued ({job['template']}@{job['template_version']}). "
                       f"You will get a notice when it is archived; status: curl -fsS {self.base_url}api/jobs/{job['id']}")
        except ValueError:
            self.reply(mid, principal['id'], 'send a JSON job: {"template":"researcher","task":"...","inputs":{"handoffs":[]}} '
                       f'(manual: curl -fsS {self.base_url})')
        except Fail as error:
            self.reply(mid, principal['id'], f'job refused: {error.code}: {error.message}')

    def job_message(self, mid, job_id, body, principal):
        mode = 'queue'
        try:
            parsed = json.loads(body)
            if isinstance(parsed, dict) and isinstance(parsed.get('text'), str):
                body, mode = parsed['text'], parsed.get('mode', 'queue')
        except ValueError:
            pass
        try:
            self.send_message(job_id, body, mode, principal['id'])
        except Fail as error:
            self.reply(mid, principal['id'], f'{error.code}: {error.message}', endpoint=f'jobs/{job_id}')

    def poll_messages(self):
        page = self.cli.run(['message', 'receive', '--limit', '20', '--wait', '1000'])
        for delivery in page.get('deliveries') or []:
            did = delivery.get('delivery_id')
            if not isinstance(did, str):
                continue
            try:
                self.handle_delivery(delivery)
            except (KeyError, TypeError, AttributeError) as exc:
                self.log('poison_delivery', did, str(exc)[:200])
            except (RuntimeError, sqlite3.Error, subprocess.TimeoutExpired) as exc:
                self.log('delivery_retry', did, str(exc)[:200])
                continue
            self.cli.run(['message', 'ack', did, '--operation-id', 'agent-hub-ack-' + digest(did)])

    # ---- views
    def job_view(self, job, full=False):
        handoff = self.store.handoff(job['id'])
        running = bool(self.worker_port(job)) and job['status'] in LIVE
        view = {'id': job['id'], 'template': f"{job['template']}@{job['template_version']}", 'task': job['task'],
                'inputs': json.loads(job['inputs']), 'model': job['model'],
                'requester': {'id': job['requester_id'], 'kind': job['requester_kind']},
                'status': job['status'], 'error': job['error'], 'created_at': iso(job['created_at']),
                'started_at': iso(job['started_at']), 'finished_at': iso(job['finished_at']),
                'worker': {'running': running, 'pid': job['worker_pid'] if running else None, 'restarts': job['restarts'],
                           'ui': f"workers/{job['id']}/" if running else None},
                'session_id': job['session_id'], 'stop_requested': bool(job['stop_requested_at']),
                'hub': self.address, 'address': f"{self.address}/jobs/{job['id']}",
                'handoff': None if not handoff else {'status': json.loads(handoff['front']).get('status'),
                                                     'summary': json.loads(handoff['front']).get('summary'),
                                                     'verdict': json.loads(handoff['front']).get('verdict'),
                                                     'ref': f"{self.address}/{job['id']}",
                                                     'url': f"api/handoffs/{job['id']}"}}
        if full:
            view.update({'cwd': job['cwd'], 'notice': job['notice'], 'messages': [
                             {'at': iso(m['at']), 'sender': m['sender'], 'mode': m['mode'], 'text': m['text'], 'state': m['state']}
                             for m in self.store.messages(job['id'])],
                         'transcript': f"api/jobs/{job['id']}/transcript" if self.store.one('SELECT 1 FROM archives WHERE job_id=?', (job['id'],)) else None,
                         'remote_handoffs': {ref: {'hub': r['hub'], 'service_id': r['service_id'], 'fetched_at': iso(r['fetched_at'])}
                                             for ref, r in json.loads(job.get('remote_handoffs') or '{}').items()}})
            if handoff:
                view['handoff'].update({'markdown': handoff['markdown'], 'front': json.loads(handoff['front']),
                                        'refs': json.loads(handoff['refs']), 'submitted_by': handoff['submitted_by'],
                                        'submitted_at': iso(handoff['submitted_at'])})
        return view

    def template_view(self, template, full=False):
        view = {k: template[k] for k in ('name', 'version', 'harness', 'model', 'effort', 'cwd', 'description')}
        view.update({'updated_at': iso(template['created_at']), 'updated_by': template['created_by']})
        if full:
            view['agent_md'] = template['agent_md']
            view['versions'] = [{'version': v['version'], 'created_at': iso(v['created_at']), 'created_by': v['created_by']}
                                for v in self.store.versions(template['name'])]
        return view

    def tree(self, path):
        def node(key, name, kind, caps):
            return {'key': key, 'name': name[:120], 'kind': kind, 'capabilities': caps}
        parts = path.split('/') if path else []
        if not parts:
            nodes = [node('templates', 'Templates', 'app', ['tree.read']), node('jobs', 'Jobs', 'app', ['tree.read']),
                     node('handoffs', 'Handoffs', 'app', ['tree.read'])]
        elif parts == ['templates']:
            nodes = [node(t['name'], f"{t['name']} v{t['version']} ({t['harness']})", 'app', ['page.read'])
                     for t in self.store.templates()[:MAX_PAGE]]
        elif parts == ['jobs']:
            nodes = [node(j['id'], f"{j['id']} · {j['template']} · {j['status']}", 'agent' if j['status'] in LIVE else 'app',
                          ['message.send', 'message.read', 'page.read'] if j['status'] in LIVE else ['message.read', 'page.read'])
                     for j in self.store.jobs()]
        elif parts == ['handoffs']:
            rows = self.store.q('''SELECT h.job_id, h.front, j.template FROM handoffs h JOIN jobs j ON j.id=h.job_id
                                   ORDER BY h.submitted_at DESC LIMIT ?''', (MAX_PAGE,))
            nodes = [node(r['job_id'], f"{r['job_id']} · {json.loads(r['front']).get('status')} · {json.loads(r['front']).get('summary', '')}",
                          'app', ['page.read']) for r in rows]
        else:
            raise Fail(404, 'app_not_found', 'no such App node')
        return {'schema_version': 1, 'path': parts, 'nodes': nodes}


# ---------------------------------------------------------------- identity

class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


class Identity:
    """Verify `X-Treer-Identity-Token` with the Proxy for this service; never decode claims locally."""

    def __init__(self, proxy_url, service_id, workspace):
        url = urlsplit(proxy_url)
        if url.scheme not in ('http', 'https') or not url.netloc or url.username or url.password:
            raise ValueError('proxy_origin_required')
        self.url = proxy_url.rstrip('/') + '/.treer/apps/identity/verify'
        self.service_id, self.workspace = service_id, workspace
        # A worker UI page loads many assets with the same forwarded token: remember a
        # verified token briefly instead of asking the Proxy for every request.
        self.cache, self.mutex = {}, threading.Lock()

    CACHE_SECONDS = 20

    def __call__(self, headers):
        token = headers.get('X-Treer-Identity-Token')
        if not token or len(token) > 8192:
            return None
        auth = headers.get('Authorization')
        if auth is not None and auth != 'Bearer ' + token:
            return None
        key, moment = digest(token), time.monotonic()
        with self.mutex:
            hit = self.cache.get(key)
            if hit and hit[1] > moment:
                return dict(hit[0])
        principal = self.verify(token)
        if principal:
            with self.mutex:
                if len(self.cache) > 512:
                    self.cache = {k: v for k, v in self.cache.items() if v[1] > moment}
                self.cache[key] = (principal, moment + self.CACHE_SECONDS)
        return principal

    def verify(self, token):
        request = Request(self.url, data=encoded({'token': token, 'audience': self.service_id}).encode(),
                          headers={'Content-Type': 'application/json'}, method='POST')
        try:
            with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=5) as response:
                result = json.loads(response.read(65536))
            claims = result.get('claims') or {}
            if (result.get('active') is True and claims.get('service_id') == self.service_id
                    and claims.get('workspace_id') == self.workspace and claims.get('principal_kind') in ('human', 'agent')
                    and isinstance(claims.get('sub'), str) and PRINCIPAL.fullmatch(claims['sub'])):
                return {'id': claims['sub'], 'kind': claims['principal_kind']}
        except (OSError, ValueError, TypeError, AttributeError):
            pass
        return None


# ---------------------------------------------------------------- HTTP

def wants_html(headers):
    accept = headers.get('Accept', '')
    choices = []
    for order, raw in enumerate(accept.split(',')):
        parts = [p.strip().lower() for p in raw.split(';')]
        if parts[0] in ('text/html', 'text/markdown'):
            quality = 1.0
            for parameter in parts[1:]:
                if parameter.startswith('q='):
                    try:
                        quality = float(parameter[2:])
                    except ValueError:
                        quality = 0.0
            if quality > 0:
                choices.append((-quality, order, parts[0]))
    if choices:
        return min(choices)[2] == 'text/html'
    return 'mozilla/' in headers.get('User-Agent', '').lower()


HOP_HEADERS = ('connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te', 'trailer',
               'transfer-encoding', 'upgrade', 'proxy-connection')


def server(hub, host, port, authorize, origins=()):
    manual = (HERE / 'AGENT.md').read_text()
    page = (HERE / 'ui.html').read_text()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def setup(self):
            super().setup()
            self.connection.settimeout(30)

        def send(self, status, data, content_type='application/json; charset=utf-8', extra=()):
            raw = data if isinstance(data, bytes) else (encoded(data) if content_type.startswith('application/json') else data).encode()
            self.send_response(status)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(raw)))
            self.send_header('Cache-Control', 'no-store')
            for name, value in extra:
                self.send_header(name, value)
            self.end_headers()
            if self.command != 'HEAD':
                self.wfile.write(raw)

        def read_body(self):
            # Always drain the body: a keep-alive connection must not see it as the next request.
            length = int(self.headers.get('Content-Length') or 0)
            if length > MAX_BODY:
                self.close_connection = True
                raise Fail(413, 'body_too_large', 'request body is too large')
            self.raw = self.rfile.read(length) if length > 0 else b''

        def body(self):
            raw = self.raw
            kind = (self.headers.get('Content-Type') or '').split(';')[0].strip().lower()
            if kind in ('text/markdown', 'text/plain'):
                return {'markdown': raw.decode('utf-8', 'strict')}
            try:
                value = json.loads(raw or b'{}')
            except ValueError:
                raise Fail(400, 'invalid_json', 'request body must be JSON')
            if not isinstance(value, dict):
                raise Fail(400, 'invalid_json', 'request body must be a JSON object')
            return value

        def principal(self, mutation=False, hub_request=True):
            who = authorize(self.headers)
            if not who:
                raise Fail(401, 'authentication_required', 'X-Treer-Identity-Token for this App is required '
                           '(Agents get it automatically at http://SERVICE_ID.treer/; humans through the Console)')
            if mutation:
                site = self.headers.get('Sec-Fetch-Site')
                origin = self.headers.get('Origin')
                if site is not None and site not in ('same-origin', 'same-site', 'none'):
                    raise Fail(403, 'csrf_rejected', 'cross-site browser mutation rejected')
                if origin is not None and origins and origin not in origins:
                    raise Fail(403, 'csrf_rejected', 'browser origin is not this App or its Treer tunnel')
                if hub_request and who['kind'] == 'human' and self.headers.get('X-Hub-Request') != '1':
                    raise Fail(403, 'csrf_rejected', 'human mutations require X-Hub-Request: 1')
            return who

        def do_HEAD(self):
            self.do_GET()

        def do_GET(self):
            self.route('GET')

        def do_POST(self):
            self.route('POST')

        def do_PUT(self):
            self.route('PUT')

        def do_DELETE(self):
            self.route('DELETE')

        def route(self, method):
            url = urlsplit(self.path)
            path = unquote(url.path)
            query = {k: v[-1] for k, v in parse_qs(url.query).items()}
            self.raw = b''
            if self.command == 'HEAD':
                method = 'HEAD' if path.startswith('/workers/') else 'GET'
            try:
                if path.startswith('/workers/'):
                    return self.worker_proxy(method, url)
                if method != 'GET':
                    self.read_body()
                if method == 'GET' and path == '/health':
                    return self.send(200, {'ok': True})
                if method == 'GET' and path == '/':
                    if wants_html(self.headers):
                        return self.send(200, page, 'text/html; charset=utf-8', [('Vary', 'Accept, User-Agent')])
                    return self.send(200, manual.replace('HUB_URL', hub.base_url.rstrip('/')).replace('HUB_ADDRESS', hub.address),
                                     'text/markdown; charset=utf-8', [('Vary', 'Accept, User-Agent')])
                if method == 'GET' and path == '/api/handoff-format':
                    template = query.get('template', 'TEMPLATE')
                    return self.send(200, handoff_format('j-0000', template if KEY.fullmatch(template) else 'TEMPLATE', hub.address),
                                     'text/markdown; charset=utf-8')
                if not path.startswith('/api/'):
                    raise Fail(404, 'not_found', 'not found')
                parts = path[5:].strip('/').split('/')
                token = self.headers.get('X-Hub-Job-Token')
                if token is not None:
                    # A worker over loopback: its own job record and handoff only, never from a browser.
                    if not (len(parts) in (2, 3) and parts[0] == 'jobs' and (
                            (len(parts) == 2 and method == 'GET') or (parts[2:] == ['handoff'] and method == 'POST'))):
                        raise Fail(403, 'job_token_scope', 'X-Hub-Job-Token is only for GET /api/jobs/<id> and its handoff')
                    if self.headers.get('Origin') is not None or any(k.lower().startswith('sec-fetch-') for k in self.headers):
                        raise Fail(403, 'csrf_rejected', 'X-Hub-Job-Token is not accepted from browsers')
                    who = hub.worker_principal(parts[1], token)
                else:
                    who = self.principal(mutation=method != 'GET')
                return self.api(method, parts, query, who)
            except Fail as error:
                return self.send(error.status, {'error': {'code': error.code, 'message': error.message}})
            except (UnicodeDecodeError, ValueError) as error:
                return self.send(400, {'error': {'code': 'invalid_request', 'message': str(error)[:200]}})
            except (RuntimeError, OSError, sqlite3.Error, subprocess.TimeoutExpired) as error:
                hub.log('http_error', method, path, str(error)[:200])
                return self.send(502, {'error': {'code': 'hub_unavailable', 'message': 'the hub could not complete the request'}})

        # ---- worker UI reverse proxy: /workers/<job>/... -> the worker's loopback AIS
        def worker_proxy(self, method, url):
            job_id, slash, rest = url.path[len('/workers/'):].partition('/')
            if method not in ('GET', 'HEAD', 'POST', 'PUT', 'DELETE'):
                raise Fail(405, 'method_not_allowed', 'unsupported method')
            self.principal(mutation=method not in ('GET', 'HEAD'), hub_request=False)
            job = hub.require_job(unquote(job_id))
            if not slash:
                # The worker page uses relative URLs: its base must end in a slash.
                self.send_response(308)
                self.send_header('Location', f"{job['id']}/" + (f'?{url.query}' if url.query else ''))
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            port = hub.worker_port(job) if job['status'] in LIVE else None
            if not port:
                raise Fail(503, 'worker_not_running', f"job {job['id']} has no running worker ({job['status']})")
            target = '/' + rest + (f'?{url.query}' if url.query else '')
            headers = [(k, v) for k, v in self.headers.items() if not self.dropped(k)]
            upgrade = (self.headers.get('Upgrade') or '').lower() == 'websocket'
            if upgrade:
                return self.websocket_pass(port, target, headers)
            if self.headers.get('Transfer-Encoding'):
                raise Fail(411, 'length_required', 'request bodies need Content-Length')
            self.read_body()
            upstream = http.client.HTTPConnection('127.0.0.1', port, timeout=120)
            try:
                upstream.putrequest(method, target, skip_host=True, skip_accept_encoding=True)
                upstream.putheader('Host', f'127.0.0.1:{port}')
                for name, value in headers:
                    upstream.putheader(name, value)
                if method not in ('GET', 'HEAD') or self.raw:
                    upstream.putheader('Content-Length', str(len(self.raw)))
                upstream.endheaders(self.raw or None)
                response = upstream.getresponse()
                self.send_response(response.status)
                length = response.getheader('Content-Length')
                for name, value in response.getheaders():
                    if name.lower() not in HOP_HEADERS and name.lower() != 'content-length':
                        self.send_header(name, value)
                if length is not None:
                    self.send_header('Content-Length', length)
                else:
                    self.close_connection = True
                    self.send_header('Connection', 'close')
                self.end_headers()
                if method == 'HEAD':
                    return
                while True:
                    chunk = response.read(65536)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
            except OSError as error:
                hub.log('worker_proxy_failed', job['id'], str(error)[:200])
                self.close_connection = True
            finally:
                upstream.close()

        @staticmethod
        def dropped(name):
            """Headers never forwarded to a worker: identity, browser provenance and hop-by-hop."""
            name = name.lower()
            return (name in HOP_HEADERS or name in ('host', 'origin', 'referer', 'cookie', 'authorization', 'content-length',
                                                    'forwarded', 'x-real-ip', 'x-hub-request', 'x-hub-job-token')
                    or name.startswith(('x-treer-', 'sec-fetch-', 'x-forwarded-')))

        def websocket_pass(self, port, target, headers):
            """Minimal WebSocket passthrough: forward the upgrade, then splice bytes both ways.

            Clients send no frames before the 101 response, so nothing is left in rfile's buffer."""
            upstream = socket.create_connection(('127.0.0.1', port), timeout=10)
            try:
                lines = [f'GET {target} HTTP/1.1', f'Host: 127.0.0.1:{port}', 'Connection: Upgrade', 'Upgrade: websocket']
                lines += [f'{name}: {value}' for name, value in headers]
                upstream.sendall(('\r\n'.join(lines) + '\r\n\r\n').encode('latin-1'))
                head = b''
                while b'\r\n\r\n' not in head:
                    chunk = upstream.recv(4096)
                    if not chunk or len(head) > 65536:
                        raise Fail(502, 'worker_upgrade_failed', 'the worker refused the WebSocket upgrade')
                    head += chunk
                self.close_connection = True
                self.connection.sendall(head)
                if not head.startswith((b'HTTP/1.1 101', b'HTTP/1.0 101')):
                    return
                self.connection.settimeout(None)
                upstream.settimeout(None)
                with selectors.DefaultSelector() as selector:
                    selector.register(self.connection, selectors.EVENT_READ, upstream)
                    selector.register(upstream, selectors.EVENT_READ, self.connection)
                    while True:
                        for key, _ in selector.select():
                            data = key.fileobj.recv(65536)
                            if not data:
                                return
                            key.data.sendall(data)
            except OSError:
                return
            finally:
                upstream.close()

        def api(self, method, parts, query, who):
            n = len(parts)
            if parts == ['me'] and method == 'GET':
                return self.send(200, {'principal': who, 'address': hub.address, 'base_url': hub.base_url,
                                       'concurrency': hub.config['concurrency']})
            if parts == ['tree'] and method == 'GET':
                return self.send(200, hub.tree(query.get('path', '')))
            if parts[0] == 'templates':
                if n == 1 and method == 'GET':
                    return self.send(200, {'templates': [hub.template_view(t) for t in hub.store.templates()]})
                if n == 1 and method == 'POST':
                    body = self.body()
                    t = hub.store.put_template(body.get('name'), body, who['id'], create=True)
                    return self.send(201, hub.template_view(t, full=True))
                if n == 2 and method == 'GET':
                    version = query.get('version')
                    t = hub.store.template(parts[1], int(version) if version and version.isdigit() else None)
                    if not t:
                        raise Fail(404, 'template_not_found', f'template {parts[1]} not found')
                    return self.send(200, hub.template_view(t, full=True))
                if n == 2 and method == 'PUT':
                    t = hub.store.put_template(parts[1], self.body(), who['id'])
                    return self.send(200, hub.template_view(t, full=True))
            if parts[0] == 'jobs':
                if n == 1 and method == 'GET':
                    status = [s for s in query.get('status', '').split(',') if s]
                    if any(s not in STATUSES for s in status):
                        raise Fail(400, 'invalid_status', 'status is one of ' + ', '.join(STATUSES))
                    requester = who['id'] if query.get('requester') == 'me' else query.get('requester')
                    limit = min(MAX_PAGE, int(query['limit'])) if query.get('limit', '').isdigit() else MAX_PAGE
                    return self.send(200, {'jobs': [hub.job_view(j) for j in hub.store.jobs(status or None, requester, limit)]})
                if n == 1 and method == 'POST':
                    job = hub.create_job(self.body(), who)
                    return self.send(201, hub.job_view(job, full=True))
                job_id = parts[1]
                if n == 2 and method == 'GET':
                    return self.send(200, hub.job_view(hub.require_job(job_id), full=True))
                if n == 3 and method == 'POST' and parts[2] == 'message':
                    body = self.body()
                    text = text_field(body, 'text', MAX_TASK)
                    return self.send(200, hub.send_message(job_id, text, body.get('mode', 'queue'), who['id']))
                if n == 3 and method == 'POST' and parts[2] == 'stop':
                    return self.send(200, hub.request_stop(job_id, who))
                if n == 3 and method == 'POST' and parts[2] == 'handoff':
                    return self.send(200, hub.submit_handoff(job_id, self.body().get('markdown'), who))
                if n == 3 and method == 'GET' and parts[2] == 'transcript':
                    hub.require_job(job_id)
                    row = hub.store.one('SELECT transcript FROM archives WHERE job_id=?', (job_id,))
                    if not row:
                        raise Fail(404, 'transcript_not_found', 'no archived transcript for this job')
                    return self.send(200, (row['transcript'] or '{}').encode(), 'application/json; charset=utf-8')
            if parts[0] == 'handoffs':
                if n == 1 and method == 'GET':
                    rows = hub.store.q('''SELECT h.job_id, h.front, h.submitted_at, j.template, j.template_version, j.status
                                          FROM handoffs h JOIN jobs j ON j.id=h.job_id ORDER BY h.submitted_at DESC LIMIT ?''', (MAX_PAGE,))
                    return self.send(200, {'handoffs': [{'job': r['job_id'], 'ref': f"{hub.address}/{r['job_id']}",
                                                         'template': f"{r['template']}@{r['template_version']}",
                                                         'job_status': r['status'], 'status': json.loads(r['front']).get('status'),
                                                         'summary': json.loads(r['front']).get('summary'),
                                                         'submitted_at': iso(r['submitted_at'])} for r in rows]})
                if n == 2 and method == 'GET':
                    job = hub.require_job(parts[1])
                    handoff = hub.store.handoff(job['id'])
                    if not handoff:
                        raise Fail(404, 'handoff_not_found', f"job {job['id']} has no handoff (status {job['status']})")
                    if query.get('format') == 'markdown':
                        return self.send(200, handoff['markdown'], 'text/markdown; charset=utf-8')
                    return self.send(200, {'job': job['id'], 'hub': hub.address, 'ref': f"{hub.address}/{job['id']}",
                                           'address': f"{hub.address}/jobs/{job['id']}",
                                           'template': f"{job['template']}@{job['template_version']}",
                                           'job_status': job['status'], 'markdown': handoff['markdown'],
                                           'front': json.loads(handoff['front']), 'refs': json.loads(handoff['refs']),
                                           'session_id': job['session_id'], 'submitted_by': handoff['submitted_by'],
                                           'submitted_at': iso(handoff['submitted_at'])})
            raise Fail(404 if method == 'GET' else 405, 'not_found', 'no such API route')

        def log_message(self, *_):
            pass

    class Server(ThreadingHTTPServer):
        daemon_threads = True
    return Server((host, port), Handler)


# ---------------------------------------------------------------- main

def seed(store):
    """Insert bundled templates; a bundled template nobody edited (latest version by `seed`)
    gets the new bundled text as a new version. Edits are never overwritten."""
    for path in sorted((HERE / 'templates').glob('*.md')):
        current = store.template(path.stem)
        if not current:
            store.put_template(path.stem, template_file(path), 'seed', create=True)
        elif current['created_by'] == 'seed':
            store.put_template(path.stem, template_file(path), 'seed')


def origin(url):
    parts = urlsplit(url)
    return f'{parts.scheme}://{parts.netloc}'


def discover(cli):
    me = os.environ.get('TREER_AGENT_ID')
    apps = cli.run(['app', 'list']).get('apps') or []
    app = next((a for a in apps if a.get('runtime_agent_id') == me), None)
    if not app:
        raise RuntimeError('this process is not a Managed App runtime; pass --service-id, --workspace and --app-name')
    return app


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--state-dir', required=True)
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--treer', default=os.environ.get('TREER_BIN') or 'treer')
    parser.add_argument('--launcher', default='treer-launch',
                        help='treer-launch that supports --instructions and --no-register (absolute path recommended)')
    parser.add_argument('--proxy-url', default=os.environ.get('TREER_PROXY_URL'))
    parser.add_argument('--jobs-root', help='directory for per-job working directories (default STATE/jobs)')
    parser.add_argument('--concurrency', type=int, default=3)
    parser.add_argument('--stall-seconds', type=int, default=900)
    parser.add_argument('--stop-grace-seconds', type=int, default=180)
    parser.add_argument('--start-timeout-seconds', type=int, default=600)
    parser.add_argument('--max-runtime-seconds', type=int, default=12 * 3600)
    parser.add_argument('--max-restarts', type=int, default=2, help='worker crashes restarted on the same session per job')
    parser.add_argument('--service-id')
    parser.add_argument('--workspace')
    parser.add_argument('--app-name')
    args = parser.parse_args(argv)
    os.umask(0o077)
    cli = Cli(args.treer)
    app = {} if (args.service_id and args.workspace and args.app_name) else discover(cli)
    service = args.service_id or app['service_id']
    workspace = args.workspace or app['workspace_id']
    name = args.app_name
    if not name:
        with contextlib.suppress(RuntimeError, KeyError, TypeError, subprocess.TimeoutExpired):
            name = cli.run(['agent', 'show', app['runtime_agent_id']]).get('path')
        name = name or app['name']
    host_root = cli.run(['whoami'])['machine']['root']
    state = Path(args.state_dir).resolve()
    if not args.proxy_url:
        raise SystemExit('--proxy-url (or TREER_PROXY_URL) is required for identity verification')
    with contextlib.closing(Store(state)) as store:
        seed(store)
        hub = Hub(store, cli, {
            'base_url': f'http://{service}.treer/', 'address': '/' + name.strip('/'), 'app_id': os.environ.get('TREER_AGENT_ID'),
            'concurrency': max(1, args.concurrency), 'stall': args.stall_seconds, 'stop_grace': args.stop_grace_seconds,
            'start_timeout': args.start_timeout_seconds, 'max_runtime': args.max_runtime_seconds,
            'max_restarts': max(0, args.max_restarts), 'launcher': args.launcher, 'host_root': host_root,
            'jobs_root': str(Path(args.jobs_root).resolve()) if args.jobs_root else str(state / 'jobs'),
            'log': lambda *parts: print(' '.join(str(p) for p in parts), flush=True)})
        origins = {origin(args.proxy_url)} | ({origin(app['public_url'])} if app.get('public_url') else set())
        httpd = server(hub, args.host, args.port, Identity(args.proxy_url, service, workspace), origins)
        hub.config['hub_url'] = f'http://127.0.0.1:{httpd.server_address[1]}'
        stopped = threading.Event()
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, lambda *_: stopped.set())
        # Workers of a previous hub process are stopped; their jobs resume on the same sessions.
        hub.recover()
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        print(f'agent hub listening on {args.host}:{httpd.server_address[1]} as {hub.address} ({hub.base_url})', flush=True)
        last_tick = 0
        try:
            while not stopped.is_set():
                try:
                    hub.poll_messages()
                except (RuntimeError, OSError, ValueError, sqlite3.Error, subprocess.TimeoutExpired) as error:
                    print('receive_error', str(error)[:200], flush=True)
                    stopped.wait(2)
                if now() - last_tick >= 3:
                    last_tick = now()
                    try:
                        hub.tick()
                    except (sqlite3.Error, OSError) as error:
                        print('tick_failed', str(error)[:200], flush=True)
        finally:
            with hub.lock:
                hub.shutdown()
            httpd.shutdown()
            httpd.server_close()


if __name__ == '__main__':
    main()
