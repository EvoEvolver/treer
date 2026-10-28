BEGIN IMMEDIATE;
CREATE TABLE execution_audit_cli (
 request TEXT PRIMARY KEY CHECK(length(request)=32), token TEXT NOT NULL REFERENCES tokens(id),
 generation TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation IN ('cli','list','status','output','transcript','prompt','stop','start','restart')),
 target TEXT, started INTEGER NOT NULL, ended INTEGER,
 outcome TEXT NOT NULL CHECK(outcome IN ('reserved','dispatched','completed','policy_denied','unavailable','outcome_unknown')));

INSERT INTO execution_audit_cli SELECT * FROM execution_audit;
DROP TABLE execution_audit;
ALTER TABLE execution_audit_cli RENAME TO execution_audit;
CREATE INDEX audit_token ON execution_audit(token);
PRAGMA user_version=3;
COMMIT;
