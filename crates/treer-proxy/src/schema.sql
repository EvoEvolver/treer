CREATE TABLE IF NOT EXISTS proxy_secrets (
    name TEXT PRIMARY KEY,
    value BYTEA NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    preferred_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS users_email_lower ON users(lower(email));

CREATE TABLE IF NOT EXISTS organizations (
    organization_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS organization_members (
    organization_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner', 'admin', 'member')),
    joined_at TEXT NOT NULL,
    PRIMARY KEY(organization_id, user_id),
    FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS workspaces_organization_id ON workspaces(organization_id);

CREATE TABLE IF NOT EXISTS invitations (
    token TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    used_at TEXT,
    used_by TEXT,
    kind TEXT NOT NULL CONSTRAINT invitations_kind_check
        CHECK(kind IN ('personal', 'organization')),
    organization_id TEXT,
    role TEXT CHECK(role IN ('owner', 'admin', 'member')),
    CONSTRAINT invitations_target_check CHECK(
        (kind = 'personal' AND organization_id IS NULL AND role IS NULL) OR
        (kind = 'organization' AND organization_id IS NOT NULL AND role IS NOT NULL)
    ),
    FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE
);

-- Upgrade invitations created before invite kinds were explicit. Existing
-- initial-owner invitations remain valid organization invitations.
ALTER TABLE invitations ADD COLUMN IF NOT EXISTS kind TEXT;
UPDATE invitations SET kind = 'organization' WHERE kind IS NULL;
ALTER TABLE invitations ALTER COLUMN kind SET NOT NULL;
ALTER TABLE invitations ALTER COLUMN organization_id DROP NOT NULL;
ALTER TABLE invitations ALTER COLUMN role DROP NOT NULL;
DROP INDEX IF EXISTS invitations_pending_owner;
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'invitations'::regclass
          AND conname = 'invitations_kind_check'
    ) THEN
        ALTER TABLE invitations ADD CONSTRAINT invitations_kind_check
            CHECK(kind IN ('personal', 'organization'));
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'invitations'::regclass
          AND conname = 'invitations_target_check'
    ) THEN
        ALTER TABLE invitations ADD CONSTRAINT invitations_target_check CHECK(
            (kind = 'personal' AND organization_id IS NULL AND role IS NULL) OR
            (kind = 'organization' AND organization_id IS NOT NULL AND role IS NOT NULL)
        );
    END IF;
END
$$;

CREATE TABLE IF NOT EXISTS sessions (
    token TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS sessions_expires_at ON sessions(expires_at);

CREATE TABLE IF NOT EXISTS admin_sessions (
    token TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS machine_enrollments (
    enrollment_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    used_at TEXT,
    server_id TEXT
);

CREATE TABLE IF NOT EXISTS machines (
    server_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    installation_id TEXT,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    enrolled_by TEXT NOT NULL,
    revoked_at TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS machines_workspace_installation
    ON machines(workspace_id, installation_id) WHERE installation_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS machine_names (
    server_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_names (
    agent_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_names_workspace_id ON agent_names(workspace_id);

CREATE TABLE IF NOT EXISTS deleted_agents (
    agent_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    deleted_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS deleted_agents_workspace_id ON deleted_agents(workspace_id);

CREATE TABLE IF NOT EXISTS machine_services (
    service_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    server_id TEXT NOT NULL,
    target_host TEXT NOT NULL,
    target_port BIGINT NOT NULL CHECK(target_port BETWEEN 1 AND 65535),
    protocol TEXT NOT NULL CHECK(protocol IN ('tcp', 'http')),
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS machine_services_workspace_name_lower
    ON machine_services(workspace_id, lower(name));
CREATE INDEX IF NOT EXISTS machine_services_server
    ON machine_services(workspace_id, server_id);

CREATE TABLE IF NOT EXISTS virtual_network_hosts (
    workspace_id TEXT NOT NULL,
    hostname TEXT NOT NULL,
    service_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    PRIMARY KEY(workspace_id, hostname),
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(service_id) REFERENCES machine_services(service_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS virtual_network_hosts_service
    ON virtual_network_hosts(workspace_id, service_id);
