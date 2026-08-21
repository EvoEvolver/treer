PRAGMA foreign_keys = ON;

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    sender_kind TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    sender_name TEXT NOT NULL,
    sender_role TEXT,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE recipients (
    message_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    recipient_kind TEXT NOT NULL,
    recipient_id TEXT NOT NULL,
    recipient_name TEXT NOT NULL,
    recipient_role TEXT,
    position BIGINT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT,
    PRIMARY KEY(message_id, recipient_kind, recipient_id),
    FOREIGN KEY(message_id) REFERENCES messages(message_id) ON DELETE CASCADE
);
CREATE TABLE contexts (
    message_id TEXT NOT NULL,
    context_message_id TEXT NOT NULL,
    position BIGINT NOT NULL,
    PRIMARY KEY(message_id, context_message_id),
    FOREIGN KEY(message_id) REFERENCES messages(message_id) ON DELETE CASCADE,
    FOREIGN KEY(context_message_id) REFERENCES messages(message_id) ON DELETE CASCADE
);
CREATE TABLE oauth_states (
    state_hash TEXT PRIMARY KEY,
    verifier TEXT NOT NULL,
    return_path TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE TABLE human_sessions (
    token_hash TEXT PRIMARY KEY,
    access_token TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    preferred_name TEXT NOT NULL,
    role TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

INSERT INTO messages VALUES
    ('legacy_root', 'workspace-a', 'human', 'user-a', 'Owner', 'owner', 'Root', '2026-08-20T12:00:00Z'),
    ('legacy_branch_a', 'workspace-a', 'agent', 'agent-a', 'Builder', NULL, 'Branch A', '2026-08-20T12:01:00Z'),
    ('legacy_branch_b', 'workspace-a', 'agent', 'agent-b', 'Reviewer', NULL, 'Branch B', '2026-08-20T12:02:00Z'),
    ('legacy_merge', 'workspace-a', 'human', 'user-a', 'Owner', 'owner', 'Merge', '2026-08-20T12:03:00Z');
INSERT INTO recipients VALUES
    ('legacy_root', 'workspace-a', 'agent', 'agent-a', 'Builder', NULL, 0, '2026-08-20T12:00:00Z', '2026-08-20T12:00:30Z'),
    ('legacy_root', 'workspace-a', 'agent', 'agent-b', 'Reviewer', NULL, 1, '2026-08-20T12:00:00Z', NULL),
    ('legacy_branch_a', 'workspace-a', 'human', 'user-a', 'Owner', 'owner', 0, '2026-08-20T12:01:00Z', '2026-08-20T12:01:30Z'),
    ('legacy_branch_b', 'workspace-a', 'human', 'user-a', 'Owner', 'owner', 0, '2026-08-20T12:02:00Z', NULL),
    ('legacy_merge', 'workspace-a', 'agent', 'agent-a', 'Builder', NULL, 0, '2026-08-20T12:03:00Z', NULL);
INSERT INTO contexts VALUES
    ('legacy_branch_a', 'legacy_root', 0),
    ('legacy_branch_b', 'legacy_root', 0),
    ('legacy_merge', 'legacy_branch_a', 0),
    ('legacy_merge', 'legacy_branch_b', 1);
INSERT INTO oauth_states VALUES
    ('expired-state', 'fixture-verifier', '/', '2020-01-01T00:00:00Z');
INSERT INTO human_sessions VALUES
    ('active-session', 'fixture-access-active', 'workspace-a', 'svc_mail', 'user-a', 'Owner', 'owner', '2099-01-01T00:00:00Z'),
    ('expired-session', 'fixture-access-expired', 'workspace-a', 'svc_mail', 'user-a', 'Owner', 'owner', '2020-01-01T00:00:00Z');
