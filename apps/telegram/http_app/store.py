"""Private SQLite files. Public handlers open QueueStore only, never AdminStore."""
import contextlib
import hmac
import json
import os
from pathlib import Path
import secrets
import sqlite3
import stat
import threading
import time

from telegram_app.registry import REGISTRY_SCHEMA, RegistryMixin, PAIR_ALPHABET
from .common import (Error, ID, TOKEN, MAX_BODY, MAX_ITEMS, MAX_CLIENT_BYTES,
                     MAX_TOTAL_BYTES, digest, fields, invalid, text)


class Database:
    def __init__(self, path):
        self.path = Path(path)
        os.umask(0o077)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.path.parent.is_symlink() or self.path.parent.stat().st_uid != os.getuid():
            raise ValueError("state directory must be private and not a symlink")
        self.path.parent.chmod(0o700)
        for candidate in (self.path, Path(str(self.path) + "-wal"), Path(str(self.path) + "-shm")):
            if candidate.exists() or candidate.is_symlink():
                info = candidate.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid():
                    raise ValueError("state file must be an ordinary private file")
                candidate.chmod(0o600)
        self._lock = threading.RLock()
        with self._connect() as db:
            db.execute("PRAGMA journal_mode=WAL")
        self.path.chmod(0o600)

    @contextlib.contextmanager
    def _connect(self):
        db = sqlite3.connect(self.path, timeout=5)
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA foreign_keys=ON")
        try:
            with db:
                yield db
        finally:
            db.close()


class QueueStore(Database):
    def __init__(self, path, clock=time.time, max_items=MAX_ITEMS, max_client_bytes=MAX_CLIENT_BYTES,
                 max_total_bytes=MAX_TOTAL_BYTES):
        super().__init__(path)
        self.clock, self.max_items = clock, max_items
        self.max_client_bytes, self.max_total_bytes = max_client_bytes, max_total_bytes
        with self._connect() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS clients (
                    id TEXT PRIMARY KEY, token_hash TEXT UNIQUE NOT NULL,
                    generation INTEGER NOT NULL DEFAULT 1, expires_at REAL,
                    revoked INTEGER NOT NULL DEFAULT 0, paused INTEGER NOT NULL DEFAULT 0,
                    next_seq INTEGER NOT NULL DEFAULT 1, served_seq INTEGER NOT NULL DEFAULT 0,
                    ack_seq INTEGER NOT NULL DEFAULT 0, pair_hash TEXT);
                CREATE TABLE IF NOT EXISTS inbox (
                    message_id TEXT PRIMARY KEY, client_id TEXT NOT NULL REFERENCES clients(id),
                    idem_hash TEXT NOT NULL, content_hash TEXT NOT NULL, body TEXT NOT NULL,
                    context TEXT, state TEXT NOT NULL DEFAULT 'accepted', lease_until REAL NOT NULL DEFAULT 0,
                    core_id TEXT, created_at REAL NOT NULL, UNIQUE(client_id,idem_hash));
                CREATE TABLE IF NOT EXISTS outbox (
                    message_id TEXT PRIMARY KEY, client_id TEXT NOT NULL REFERENCES clients(id),
                    seq INTEGER NOT NULL, core_id TEXT UNIQUE NOT NULL, delivery_id TEXT UNIQUE NOT NULL,
                    body TEXT NOT NULL, created_at REAL NOT NULL, acked_at REAL, UNIQUE(client_id,seq));
                CREATE TABLE IF NOT EXISTS maps (
                    public_id TEXT PRIMARY KEY, core_id TEXT UNIQUE NOT NULL, client_id TEXT NOT NULL,
                    created_at REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS inbox_client ON inbox(client_id, state);
                CREATE INDEX IF NOT EXISTS outbox_client ON outbox(client_id, seq);
            """)

    def prepare_pair(self, client, handle):
        with self._lock, self._connect() as db:
            db.execute("""INSERT INTO clients(id,token_hash,revoked,pair_hash) VALUES (?,?,1,?)
                ON CONFLICT(id) DO UPDATE SET revoked=1,generation=generation+1,pair_hash=excluded.pair_hash""",
                       (client, digest(secrets.token_urlsafe(32)), digest(handle)))

    def install_token(self, client, token, expires=None, expected_handle=None):
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            if expected_handle is not None:
                row = db.execute("SELECT pair_hash FROM clients WHERE id=?", (client,)).fetchone()
                if row is None or not hmac.compare_digest(row[0] or "", digest(expected_handle)):
                    raise invalid()
            db.execute("""INSERT INTO clients(id,token_hash,expires_at) VALUES (?,?,?)
                ON CONFLICT(id) DO UPDATE SET token_hash=excluded.token_hash,
                expires_at=excluded.expires_at,generation=generation+1,revoked=0,pair_hash=NULL""",
                       (client, digest(token), expires))

    def authenticate(self, token):
        if not isinstance(token, str) or not TOKEN.fullmatch(token):
            raise invalid()
        hashed = digest(token)
        with self._connect() as db:
            row = db.execute("SELECT * FROM clients WHERE token_hash=?", (hashed,)).fetchone()
        expected = row["token_hash"] if row else "0" * 64
        valid = hmac.compare_digest(hashed, expected)
        if not valid or row is None or row["revoked"] or (row["expires_at"] is not None and row["expires_at"] <= self.clock()):
            raise invalid()
        return dict(row)

    def available(self, db, client):
        row = db.execute("SELECT * FROM clients WHERE id=?", (client,)).fetchone()
        if row is None or row["revoked"] or row["expires_at"] is not None and row["expires_at"] <= self.clock():
            raise invalid()
        paused = db.execute("SELECT value FROM settings WHERE key='paused'").fetchone()
        if row["paused"] or paused is not None and paused[0] == "1":
            raise Error(503, "paused", "Endpoint or client paused", 5)
        return row

    def control(self, client=None, *, revoke=False, paused=None):
        with self._lock, self._connect() as db:
            if client is None:
                db.execute("INSERT INTO settings VALUES ('paused',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                           ("1" if paused else "0",))
            else:
                if revoke:
                    db.execute("UPDATE clients SET revoked=1,generation=generation+1,pair_hash=NULL WHERE id=?", (client,))
                if paused is not None:
                    db.execute("UPDATE clients SET paused=? WHERE id=?", (int(paused), client))

    def _capacity(self, db, client, table, size):
        clause = "state!='delivered'" if table == "inbox" else "acked_at IS NULL"
        row = db.execute(f"SELECT count(*),coalesce(sum(length(CAST(body AS BLOB))),0) FROM {table} WHERE client_id=? AND {clause}", (client,)).fetchone()
        total_count = sum(db.execute(f"SELECT count(*) FROM {name}").fetchone()[0] for name in ("inbox", "outbox"))
        total = sum(db.execute(f"SELECT coalesce(sum(length(CAST(body AS BLOB))),0) FROM {name}").fetchone()[0]
                    for name in ("inbox", "outbox"))
        if total_count >= 10000 or row[0] >= self.max_items or row[1] + size > self.max_client_bytes or total + size > self.max_total_bytes:
            raise Error(429, "queue_full", "Queue capacity reached", 60)

    def enqueue(self, client, value, admit=None, expected_generation=None):
        fields(value, ("body", "idempotency_key"), ("context",))
        body = text(value["body"], MAX_BODY)
        key = text(value["idempotency_key"], 128)
        context = value.get("context")
        if context is not None and (not isinstance(context, str) or not ID.fullmatch(context)):
            raise Error(400, "invalid_context", "Invalid context")
        hashed = digest(json.dumps({"body": body, "context": context}, sort_keys=True, ensure_ascii=False))
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            current = self.available(db, client)
            if expected_generation is not None and current['generation'] != expected_generation:
                raise invalid()
            existing = db.execute("SELECT message_id,content_hash FROM inbox WHERE client_id=? AND idem_hash=?",
                                  (client, digest(key))).fetchone()
            if existing:
                if not hmac.compare_digest(existing["content_hash"], hashed):
                    raise Error(409, "idempotency_conflict", "Key already used with different content")
                return existing["message_id"]
            if context is not None and db.execute("SELECT 1 FROM maps WHERE public_id=? AND client_id=?", (context, client)).fetchone() is None:
                raise Error(404, "context_unavailable", "Context unavailable")
            if admit is not None:
                admit()
            self._capacity(db, client, "inbox", len(body.encode()))
            if db.execute("SELECT count(*) FROM maps").fetchone()[0] >= 100000:
                raise Error(429, "mapping_capacity", "Mapping capacity reached", 60)
            message_id = secrets.token_hex(16)
            db.execute("INSERT INTO inbox(message_id,client_id,idem_hash,content_hash,body,context,created_at) VALUES (?,?,?,?,?,?,?)",
                       (message_id, client, digest(key), hashed, body, context, self.clock()))
            return message_id

    def lease(self):
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            paused = db.execute("SELECT value FROM settings WHERE key='paused'").fetchone()
            if paused is not None and paused[0] == '1':
                return None
            # Exclude unavailable clients BEFORE the batch limit: twenty old
            # paused rows must not starve a different active client's queue.
            rows = db.execute("""SELECT i.* FROM inbox i JOIN clients c ON c.id=i.client_id
                WHERE i.state!='delivered' AND i.lease_until<=? AND c.revoked=0 AND c.paused=0
                AND (c.expires_at IS NULL OR c.expires_at>?) ORDER BY i.created_at LIMIT 20""",
                              (self.clock(), self.clock())).fetchall()
            for row in rows:
                try:
                    self.available(db, row["client_id"])
                except Error:
                    continue
                db.execute("UPDATE inbox SET state='leased',lease_until=? WHERE message_id=?", (self.clock() + 60, row["message_id"]))
                return dict(row)
        return None

    def mapped(self, public_id, client):
        with self._connect() as db:
            row = db.execute("SELECT core_id FROM maps WHERE public_id=? AND client_id=?", (public_id, client)).fetchone()
        return row[0] if row else None

    def delivered(self, message_id, core_id):
        with self._lock, self._connect() as db:
            row = db.execute("SELECT * FROM inbox WHERE message_id=?", (message_id,)).fetchone()
            db.execute("INSERT OR IGNORE INTO maps VALUES (?,?,?,?)", (message_id, core_id, row["client_id"], self.clock()))
            db.execute("UPDATE inbox SET state='delivered',core_id=?,lease_until=0 WHERE message_id=?", (core_id, message_id))

    def route(self, contexts):
        if not contexts or len(contexts) > 32:
            return None
        with self._connect() as db:
            clients = set()
            for core_id in contexts:
                row = db.execute("SELECT client_id FROM maps WHERE core_id=?", (core_id,)).fetchone()
                if row is None:
                    return None
                clients.add(row[0])
        return next(iter(clients)) if len(clients) == 1 else None

    def outbound(self, client, core_id, delivery_id, body):
        text(body, MAX_BODY)
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            self.available(db, client)
            existing = db.execute("SELECT seq,client_id FROM outbox WHERE core_id=? OR delivery_id=?", (core_id, delivery_id)).fetchone()
            if existing:
                if existing["client_id"] != client:
                    raise Error(403, "route_conflict", "Delivery already belongs to another client")
                return existing[0]
            self._capacity(db, client, "outbox", len(body.encode()))
            if db.execute("SELECT count(*) FROM maps").fetchone()[0] >= 100000:
                raise Error(429, "mapping_capacity", "Mapping capacity reached", 60)
            row = db.execute("SELECT next_seq FROM clients WHERE id=?", (client,)).fetchone()
            seq, message_id = row[0], secrets.token_hex(16)
            db.execute("INSERT INTO outbox VALUES (?,?,?,?,?,?,?,NULL)", (message_id, client, seq, core_id, delivery_id, body, self.clock()))
            db.execute("INSERT INTO maps VALUES (?,?,?,?)", (message_id, core_id, client, self.clock()))
            db.execute("UPDATE clients SET next_seq=next_seq+1 WHERE id=?", (client,))
            return seq

    def poll(self, client, after):
        with self._connect() as db:
            self.available(db, client)
            rows = db.execute("SELECT seq,message_id,body,created_at FROM outbox WHERE client_id=? AND seq>? AND acked_at IS NULL ORDER BY seq LIMIT 20",
                              (client, after)).fetchall()
        items, size = [], 0
        for row in rows:
            if items and size + len(row["body"].encode()) > 256000:
                break
            items.append(dict(row)); size += len(row["body"].encode())
        if items:
            with self._connect() as db:
                db.execute("UPDATE clients SET served_seq=max(served_seq,?) WHERE id=?", (items[-1]["seq"], client))
        return {"items": items, "next_after": items[-1]["seq"] if items else after}

    def ack(self, client, up_to, expected_generation=None):
        if type(up_to) is not int or up_to < 0:
            raise Error(400, "invalid_cursor", "Invalid cursor")
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = self.available(db, client)
            if expected_generation is not None and row['generation'] != expected_generation:
                raise invalid()
            if up_to > row["served_seq"]:
                raise Error(400, "invalid_cursor", "Cannot acknowledge unseen messages")
            db.execute("UPDATE outbox SET acked_at=coalesce(acked_at,?) WHERE client_id=? AND seq<=?", (self.clock(), client, up_to))
            db.execute("UPDATE clients SET ack_seq=max(ack_seq,?) WHERE id=?", (up_to, client))

    def cleanup(self):
        with self._connect() as db:
            db.execute("DELETE FROM outbox WHERE acked_at<?", (self.clock() - 7 * 86400,))
            db.execute("DELETE FROM inbox WHERE state='delivered' AND created_at<?", (self.clock() - 86400,))
            db.execute("DELETE FROM maps WHERE created_at<?", (self.clock() - 30 * 86400,))

    def summaries(self):
        with self._connect() as db:
            return [dict(row) for row in db.execute("""SELECT id,revoked,paused,expires_at,generation,
                (SELECT count(*) FROM inbox WHERE client_id=c.id AND state!='delivered') AS inbound,
                (SELECT count(*) FROM outbox WHERE client_id=c.id AND acked_at IS NULL) AS outbound
                FROM clients c ORDER BY id LIMIT 1000""")]


class AdminStore(Database, RegistryMixin):
    """Not opened by the public App. Pair validation runs in the bridge process."""
    CHANNEL = "http"
    ACCOUNT = "endpoint"

    def __init__(self, path, queues, clock=time.time):
        super().__init__(path)
        self.queues, self.clock = queues, clock
        # Failures are bounded, volatile and never invalidate unrelated pairing codes.
        self.failures = {}
        self.pair_window, self.pair_attempts, self.cooldown_until, self.backoff_steps = 0, 0, 0, 0
        with self._connect() as db:
            db.executescript(REGISTRY_SCHEMA)
            db.executescript("""
                CREATE TABLE IF NOT EXISTS http_pairs (
                    handle_hash TEXT PRIMARY KEY, salt TEXT NOT NULL, code_hash TEXT NOT NULL,
                    client_id TEXT NOT NULL, creator TEXT NOT NULL, poc TEXT NOT NULL,
                    label TEXT NOT NULL, created_at REAL NOT NULL, expires_at REAL NOT NULL,
                    token_expires REAL, spent INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS http_undeliverable (
                    delivery_id TEXT PRIMARY KEY, first_at REAL NOT NULL,
                    attempts INTEGER NOT NULL, reason TEXT NOT NULL, size INTEGER NOT NULL);
            """)

        with self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            db.execute("INSERT OR IGNORE INTO settings VALUES ('http.account',?,?)", (secrets.token_hex(16), self.clock()))
            self.ACCOUNT = db.execute("SELECT value FROM settings WHERE key='http.account'").fetchone()[0]
            if not ID.fullmatch(self.ACCOUNT):
                raise ValueError("invalid endpoint account")

    def issue(self, actor, poc, label, client=None, expires=None):
        label = text(label, 128)
        if not isinstance(poc, str) or not poc.startswith("ag_") or len(poc) > 128 or any(ch not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_" for ch in poc):
            raise Error(400, "invalid_poc", "A verified Agent ID is required")
        if client is not None and not ID.fullmatch(client):
            raise Error(400, "invalid_client", "Invalid client")
        if expires is not None and (type(expires) not in (int, float) or not self.clock() < expires <= self.clock() + 366 * 86400):
            raise Error(400, "invalid_expiry", "Expiry must be within one year")
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            if client is not None and db.execute("SELECT 1 FROM im_users WHERE channel='http' AND account_id=? AND user_id=?", (self.ACCOUNT, client)).fetchone() is None:
                raise Error(404, "client_unavailable", "Client unavailable")
            if client is None and db.execute("SELECT count(*) FROM im_users WHERE channel='http'").fetchone()[0] >= 1000:
                raise Error(429, "client_capacity", "Client capacity reached", 60)
            db.execute("DELETE FROM http_pairs WHERE expires_at<?", (self.clock() - 86400,))
            if db.execute("SELECT count(*) FROM http_pairs").fetchone()[0] >= 10000:
                raise Error(429, "pair_capacity", "Pairing capacity reached", 600)
            client = client or secrets.token_hex(16)
            handle = secrets.token_hex(16)
            secret = "".join(secrets.choice(PAIR_ALPHABET) for _ in range(10))
            salt = secrets.token_hex(16)
            db.execute("UPDATE http_pairs SET spent=1 WHERE client_id=?", (client,))
            db.execute("INSERT INTO http_pairs VALUES (?,?,?,?,?,?,?,?,?,?,0)",
                       (digest(handle), salt, self._code_hash(salt, secret), client, actor, poc, label,
                        self.clock(), self.clock() + 600, expires))
            # Reserve the client and its permitted POC in private state, never in the public queue file.
            db.execute("INSERT INTO im_users VALUES ('http',?,?,?,1,'control') ON CONFLICT(channel,account_id,user_id) DO UPDATE SET label=excluded.label,enabled=1",
                       (self.ACCOUNT, client, label))
            db.execute("INSERT INTO poc_bindings VALUES ('http',?,?,?,0,1,?,?) ON CONFLICT(channel,account_id,conversation) DO UPDATE SET target_agent_id=excluded.target_agent_id,enabled=1,updated_at=excluded.updated_at,updated_by=excluded.updated_by",
                       (self.ACCOUNT, client, poc, self.clock(), actor))
            self._audit(db, self.clock(), None, actor, "settings", "pair.create", "http", self.ACCOUNT, client, None, poc, "ok")
            db.execute("DELETE FROM http_pairs WHERE expires_at<?", (self.clock() - 86400,))
            # Reserve a generation before the code can be read by the bridge.
            # Only a hash of the random lookup handle enters the public file.
            self.queues.prepare_pair(client, handle)
        return {"client_id": client, "code": handle + "." + secret, "expires_at": self.clock() + 600}

    def redeem(self, code):
        # An anonymous request never writes the database. Only successfully
        # spending a valid issued code does; per-code failures are bounded RAM.
        now = self.clock()
        with self._lock:
            if now < self.cooldown_until:
                raise Error(429, "pair_backoff", "Pairing temporarily cooling down", max(1, int(self.cooldown_until - now)))
            if now - self.pair_window >= 60:
                self.pair_window, self.pair_attempts = now, 0
            self.pair_attempts += 1
            if self.pair_attempts > 10:
                delay = min(180, 60 * 2 ** min(2, self.backoff_steps))
                self.backoff_steps += 1
                self.cooldown_until = now + delay
                raise Error(429, "pair_backoff", "Pairing temporarily cooling down", delay)
            if not isinstance(code, str) or len(code) != 43 or code[32] != "." or not ID.fullmatch(code[:32]) or any(ch not in PAIR_ALPHABET for ch in code[33:]):
                raise invalid()
            handle, secret = code.split(".", 1)
            with self._connect() as db:
                row = db.execute("SELECT * FROM http_pairs WHERE handle_hash=?", (digest(handle),)).fetchone()
            handle_valid = hmac.compare_digest(digest(handle), row["handle_hash"] if row else "0" * 64)
            secret_valid = hmac.compare_digest(self._code_hash(row["salt"] if row else "0", secret), row["code_hash"] if row else "0" * 64)
            valid = handle_valid & secret_valid
            failures = self.failures.get(handle, 0) if row else 0
            if row is None or row["spent"] or row["expires_at"] <= now or failures >= 10 or not valid:
                if row is not None and not row["spent"] and row["expires_at"] > now:
                    if len(self.failures) >= 1000 and handle not in self.failures:
                        self.failures.pop(next(iter(self.failures)))
                    self.failures[handle] = failures + 1
                raise invalid()
            self.backoff_steps = 0
            token = secrets.token_urlsafe(32)
            # Commit code consumption before installing/returning the new bearer.
            # A crash between files burns the code; it cannot issue two tokens.
            with self._connect() as db:
                db.execute("BEGIN IMMEDIATE")
                changed = db.execute("UPDATE http_pairs SET spent=1 WHERE handle_hash=? AND spent=0 AND expires_at>?", (digest(handle), now)).rowcount
                if changed != 1:
                    raise invalid()
                self._audit(db, now, None, "client:" + row["client_id"], "pair", "pair.redeem", "http", self.ACCOUNT,
                            row["client_id"], None, row["poc"], "ok")
            self.queues.install_token(row["client_id"], token, row["token_expires"], expected_handle=handle)
            return {"client_id": row["client_id"], "token": token, "token_type": "Bearer", "expires_at": row["token_expires"]}

    def client(self, client):
        binding = self.get_binding("http", self.ACCOUNT, client)
        user = self.get_user("http", self.ACCOUNT, client)
        if binding is None or user is None or not binding.enabled or not user[1]:
            raise Error(403, "client_disabled", "Client is not admitted")
        return binding, user[0]

    def revoke(self, client, actor):
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            if db.execute("SELECT 1 FROM im_users WHERE channel='http' AND account_id=? AND user_id=?", (self.ACCOUNT, client)).fetchone() is None:
                raise Error(404, "client_unavailable", "Client unavailable")
            db.execute("UPDATE http_pairs SET spent=1 WHERE client_id=?", (client,))
            db.execute("UPDATE im_users SET enabled=0 WHERE channel='http' AND account_id=? AND user_id=?", (self.ACCOUNT, client))
            self.queues.control(client, revoke=True)
            self._audit(db, self.clock(), None, actor, "settings", "client.revoke", "http", self.ACCOUNT, client, None, None, "ok")

    def _unreplied(self, _db, conversation):
        # RegistryMixin's default uses Telegram message_mappings. HTTP keeps its
        # delivery mappings in a separate queue file and has no numeric topics.
        with self.queues._connect() as db:
            last = db.execute("SELECT coalesce(max(created_at),0) FROM outbox WHERE client_id=?", (conversation,)).fetchone()[0]
            return db.execute("SELECT count(*) FROM inbox WHERE client_id=? AND state='delivered' AND created_at>?",
                              (conversation, last)).fetchone()[0]

    def notice(self, actor, operation, client=None, *, size=0, delivery=None, reason="ok"):
        self.audit(actor, "http", operation, "http", self.ACCOUNT, client,
                   new_target=f"bytes:{size}", outcome=reason, delivery_id=delivery)

    def unroutable(self, delivery, size, reason):
        """No body retention. Defer three attempts, then audit+ack; cap at 128 IDs."""
        with self._lock, self._connect() as db:
            db.execute("BEGIN IMMEDIATE")
            if db.execute("SELECT 1 FROM poc_audit WHERE delivery_id=?", ("drop:" + delivery,)).fetchone():
                return True
            row = db.execute("SELECT * FROM http_undeliverable WHERE delivery_id=?", (delivery,)).fetchone()
            count = db.execute("SELECT count(*) FROM http_undeliverable").fetchone()[0]
            attempts = row["attempts"] + 1 if row else 1
            should_ack = count >= 128 and row is None or attempts >= 3 or row is not None and self.clock() - row["first_at"] >= 86400
            if should_ack:
                db.execute("DELETE FROM http_undeliverable WHERE delivery_id=?", (delivery,))
                self._audit(db, self.clock(), "drop:" + delivery, "bridge", "http", "undeliverable", "http", self.ACCOUNT,
                            None, None, f"bytes:{size}", reason)
            else:
                db.execute("INSERT INTO http_undeliverable VALUES (?,?,?,?,?) ON CONFLICT(delivery_id) DO UPDATE SET attempts=excluded.attempts",
                           (delivery, self.clock(), attempts, reason, size))
            return should_ack
