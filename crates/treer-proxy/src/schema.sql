CREATE TABLE IF NOT EXISTS proxy_secrets (
    name TEXT PRIMARY KEY,
    value BYTEA NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    preferred_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);
ALTER TABLE users ADD COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE;
CREATE UNIQUE INDEX IF NOT EXISTS users_email_lower ON users(lower(email));

CREATE TABLE IF NOT EXISTS oauth_identities (
    provider TEXT NOT NULL CHECK(provider IN ('github', 'google')),
    subject TEXT NOT NULL,
    user_id TEXT NOT NULL,
    email TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(provider, subject),
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS oauth_identities_user_id ON oauth_identities(user_id);

CREATE TABLE IF NOT EXISTS oauth_states (
    state TEXT PRIMARY KEY,
    provider TEXT NOT NULL CHECK(provider IN ('github', 'google')),
    invite_token TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS oauth_states_expires_at ON oauth_states(expires_at);

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
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS deleted_at TEXT;
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS deleted_by TEXT;
CREATE INDEX IF NOT EXISTS workspaces_organization_id ON workspaces(organization_id);
CREATE INDEX IF NOT EXISTS workspaces_active_organization
    ON workspaces(organization_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS agent_launch_profiles (
    profile_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    cwd TEXT NOT NULL DEFAULT '',
    command TEXT NOT NULL,
    args JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS agent_launch_profiles_workspace_name
    ON agent_launch_profiles(workspace_id, lower(name));
CREATE INDEX IF NOT EXISTS agent_launch_profiles_workspace_updated
    ON agent_launch_profiles(workspace_id, updated_at DESC, profile_id);

CREATE TABLE IF NOT EXISTS organization_audit_events (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT UNIQUE NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    organization_id TEXT NOT NULL,
    workspace_id TEXT,
    occurred_at TEXT NOT NULL,
    actor_kind TEXT NOT NULL CHECK(actor_kind IN ('user', 'agent', 'machine', 'service', 'system')),
    actor_id TEXT,
    actor_name TEXT,
    source TEXT NOT NULL,
    action TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('succeeded', 'failed')),
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    resource_name TEXT,
    correlation_id TEXT,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS organization_audit_events_org_sequence
    ON organization_audit_events(organization_id, sequence DESC);
CREATE INDEX IF NOT EXISTS organization_audit_events_workspace_sequence
    ON organization_audit_events(organization_id, workspace_id, sequence DESC);
CREATE INDEX IF NOT EXISTS organization_audit_events_correlation
    ON organization_audit_events(correlation_id) WHERE correlation_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS workspace_policies (
    workspace_id TEXT PRIMARY KEY,
    revision BIGINT NOT NULL CHECK(revision > 0),
    schema_version BIGINT NOT NULL CHECK(schema_version > 0),
    mode TEXT NOT NULL CHECK(mode IN ('monitor', 'enforce')),
    document JSONB NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by_kind TEXT NOT NULL
        CHECK(updated_by_kind IN ('human', 'agent', 'machine', 'service')),
    updated_by_id TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);

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

CREATE TABLE IF NOT EXISTS password_reset_tokens (
    token_id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS password_reset_tokens_user_id
    ON password_reset_tokens(user_id);
CREATE INDEX IF NOT EXISTS password_reset_tokens_expires_at
    ON password_reset_tokens(expires_at);

CREATE TABLE IF NOT EXISTS admin_sessions (
    token TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS platform_audit_events (
    sequence BIGSERIAL PRIMARY KEY,
    event_id TEXT UNIQUE NOT NULL,
    occurred_at TEXT NOT NULL,
    action TEXT NOT NULL,
    resource_kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    resource_name TEXT,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX IF NOT EXISTS platform_audit_events_sequence
    ON platform_audit_events(sequence DESC);

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

CREATE TABLE IF NOT EXISTS agent_credentials (
    agent_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
CREATE INDEX IF NOT EXISTS agent_credentials_workspace_server
    ON agent_credentials(workspace_id, server_id) WHERE revoked_at IS NULL;

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
    target_agent_id TEXT,
    target_host TEXT NOT NULL,
    target_port BIGINT NOT NULL CHECK(target_port BETWEEN 1 AND 65535),
    protocol TEXT NOT NULL CHECK(protocol IN ('tcp', 'http')),
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);
ALTER TABLE machine_services
    ADD COLUMN IF NOT EXISTS target_agent_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS machine_services_workspace_name_lower
    ON machine_services(workspace_id, lower(name));
CREATE INDEX IF NOT EXISTS machine_services_server
    ON machine_services(workspace_id, server_id);
CREATE INDEX IF NOT EXISTS machine_services_target_agent
    ON machine_services(workspace_id, target_agent_id)
    WHERE target_agent_id IS NOT NULL;

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

CREATE TABLE IF NOT EXISTS app_deployments (
    app_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    server_id TEXT NOT NULL,
    command TEXT NOT NULL,
    args JSONB NOT NULL DEFAULT '[]'::jsonb,
    cwd TEXT NOT NULL DEFAULT '',
    port BIGINT NOT NULL CHECK(port BETWEEN 1 AND 65535),
    hostname TEXT NOT NULL,
    service_id TEXT NOT NULL,
    desired_state TEXT NOT NULL DEFAULT 'running'
        CHECK(desired_state IN ('running', 'stopped')),
    runtime_agent_id TEXT,
    restart_count BIGINT NOT NULL DEFAULT 0 CHECK(restart_count >= 0),
    last_error TEXT,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(service_id) REFERENCES machine_services(service_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS app_deployments_workspace_name_lower
    ON app_deployments(workspace_id, lower(name));
CREATE UNIQUE INDEX IF NOT EXISTS app_deployments_workspace_hostname_lower
    ON app_deployments(workspace_id, lower(hostname));
CREATE INDEX IF NOT EXISTS app_deployments_server
    ON app_deployments(workspace_id, server_id);
CREATE INDEX IF NOT EXISTS app_deployments_runtime_agent
    ON app_deployments(workspace_id, runtime_agent_id)
    WHERE runtime_agent_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS service_ingresses (
    ingress_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    hostname TEXT NOT NULL,
    access TEXT NOT NULL CHECK(access IN ('public', 'workspace')),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(service_id) REFERENCES machine_services(service_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS service_ingresses_hostname_lower
    ON service_ingresses(lower(hostname));
CREATE INDEX IF NOT EXISTS service_ingresses_workspace
    ON service_ingresses(workspace_id, created_at);
CREATE INDEX IF NOT EXISTS service_ingresses_service
    ON service_ingresses(service_id);

CREATE TABLE IF NOT EXISTS ingress_auth_codes (
    code TEXT PRIMARY KEY,
    ingress_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    return_path TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT,
    FOREIGN KEY(ingress_id) REFERENCES service_ingresses(ingress_id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS ingress_sessions (
    token TEXT PRIMARY KEY,
    ingress_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    FOREIGN KEY(ingress_id) REFERENCES service_ingresses(ingress_id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ingress_sessions_expiry ON ingress_sessions(expires_at);

CREATE TABLE IF NOT EXISTS app_oauth_codes (
    code_hash TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    preferred_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner', 'admin', 'member')),
    redirect_uri TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(service_id) REFERENCES machine_services(service_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS app_oauth_codes_expiry ON app_oauth_codes(expires_at);

CREATE TABLE IF NOT EXISTS machine_traffic_hourly (
    workspace_id TEXT NOT NULL,
    window_start BIGINT NOT NULL,
    source_server_id TEXT NOT NULL,
    destination_server_id TEXT NOT NULL,
    payload_bytes BIGINT NOT NULL DEFAULT 0 CHECK(payload_bytes >= 0),
    payload_frames BIGINT NOT NULL DEFAULT 0 CHECK(payload_frames >= 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(workspace_id, window_start, source_server_id, destination_server_id),
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS machine_traffic_hourly_workspace_window
    ON machine_traffic_hourly(workspace_id, window_start DESC);
CREATE TABLE IF NOT EXISTS traffic_usage_hourly (
    workspace_id TEXT NOT NULL,
    window_start BIGINT NOT NULL,
    traffic_class TEXT NOT NULL CHECK(traffic_class IN ('virtual_network', 'service_ingress', 'virtual_host', 'agent_interface')),
    source_type TEXT NOT NULL CHECK(source_type IN ('client', 'machine')),
    source_id TEXT NOT NULL,
    destination_type TEXT NOT NULL CHECK(destination_type IN ('client', 'machine')),
    destination_id TEXT NOT NULL,
    payload_bytes BIGINT NOT NULL DEFAULT 0 CHECK(payload_bytes >= 0),
    payload_frames BIGINT NOT NULL DEFAULT 0 CHECK(payload_frames >= 0),
    billable_bytes BIGINT NOT NULL DEFAULT 0 CHECK(billable_bytes >= 0),
    meter_version INTEGER NOT NULL CHECK(meter_version > 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(
        workspace_id, window_start, traffic_class, source_type, source_id,
        destination_type, destination_id, meter_version
    ),
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
);
CREATE INDEX IF NOT EXISTS traffic_usage_hourly_workspace_window
    ON traffic_usage_hourly(workspace_id, window_start DESC);
CREATE INDEX IF NOT EXISTS virtual_network_hosts_service
    ON virtual_network_hosts(workspace_id, service_id);
