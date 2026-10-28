PRAGMA foreign_keys=ON;
PRAGMA synchronous=FULL;
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS settings (
 id INTEGER PRIMARY KEY CHECK(id=1), binding BLOB NOT NULL CHECK(length(binding)=32),
 generation TEXT NOT NULL CHECK(length(generation)=64), reviewed INTEGER NOT NULL CHECK(reviewed IN (0,1)),
 high_water INTEGER NOT NULL CHECK(high_water>=0));
CREATE TABLE IF NOT EXISTS tokens (
 id TEXT PRIMARY KEY CHECK(length(id)=32), name TEXT NOT NULL CHECK(length(CAST(name AS BLOB))<=128),
 verifier BLOB NOT NULL CHECK(length(verifier)=32), generation TEXT NOT NULL,
 created INTEGER NOT NULL, expires INTEGER NOT NULL CHECK(expires>created), creator TEXT NOT NULL,
 revoked_at INTEGER, revoker TEXT, last_used INTEGER);
CREATE TABLE IF NOT EXISTS execution_audit (
 request TEXT PRIMARY KEY CHECK(length(request)=32), token TEXT NOT NULL REFERENCES tokens(id),
 generation TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation IN ('registry','inbox','ack','send','cli','list','status','output','transcript','prompt','stop','start','restart')),
 target TEXT, started INTEGER NOT NULL, ended INTEGER,
 outcome TEXT NOT NULL CHECK(outcome IN ('reserved','dispatched','completed','policy_denied','unavailable','outcome_unknown')));
CREATE INDEX IF NOT EXISTS audit_token ON execution_audit(token);
CREATE TABLE IF NOT EXISTS token_sources (token TEXT PRIMARY KEY REFERENCES tokens(id), peer TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS management_audit (
 id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL,
 action TEXT NOT NULL CHECK(action IN ('created','revoked','disabled','policy_reviewed','rotated')),
 token TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS external_agents (
 key TEXT PRIMARY KEY CHECK(length(key) BETWEEN 1 AND 64), name TEXT NOT NULL,
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)), created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS external_token_bindings (
 token TEXT PRIMARY KEY REFERENCES tokens(id), endpoint TEXT NOT NULL REFERENCES external_agents(key));
PRAGMA application_id=1413824817;
CREATE TABLE IF NOT EXISTS device_requests (
 device_hash TEXT PRIMARY KEY, user_hash TEXT NOT NULL UNIQUE,
 key TEXT NOT NULL, name TEXT NOT NULL, created INTEGER NOT NULL, expires INTEGER NOT NULL,
 poll_after INTEGER NOT NULL, generation TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','approved','denied','claimed')),
 days INTEGER, approver TEXT, decided_at INTEGER, decision TEXT, token_id TEXT);
CREATE TABLE IF NOT EXISTS device_attempt_budget (
 id INTEGER PRIMARY KEY CHECK(id=1), minute INTEGER NOT NULL, attempts INTEGER NOT NULL);
PRAGMA user_version=5;
COMMIT;
