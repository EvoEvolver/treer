CREATE TABLE IF NOT EXISTS proxy_secrets (
    name TEXT PRIMARY KEY,
    value BYTEA NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS proxy_schema_migrations (
    name TEXT PRIMARY KEY,
    applied_at TEXT NOT NULL
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
ALTER TABLE oauth_states ADD COLUMN IF NOT EXISTS invite_hash TEXT;
ALTER TABLE oauth_states ADD COLUMN IF NOT EXISTS browser_hash TEXT;
ALTER TABLE oauth_states ADD COLUMN IF NOT EXISTS join_organization BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS invitation_attempts (
    bucket TEXT PRIMARY KEY,
    attempts INTEGER NOT NULL,
    window_started_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_organization_invitations (
    session_hash TEXT PRIMARY KEY,
    pending_id TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL,
    invite_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

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
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS access_mode TEXT NOT NULL DEFAULT 'organization';
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'workspaces'::regclass
          AND conname = 'workspaces_access_mode_check'
    ) THEN
        ALTER TABLE workspaces ADD CONSTRAINT workspaces_access_mode_check
            CHECK(access_mode IN ('organization', 'restricted'));
    END IF;
END
$$;
CREATE INDEX IF NOT EXISTS workspaces_organization_id ON workspaces(organization_id);
CREATE INDEX IF NOT EXISTS workspaces_active_organization
    ON workspaces(organization_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS organization_groups (
    group_id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS organization_groups_name
    ON organization_groups(organization_id, lower(name));

CREATE TABLE IF NOT EXISTS organization_group_members (
    group_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    added_at TEXT NOT NULL,
    added_by TEXT NOT NULL,
    PRIMARY KEY(group_id, user_id),
    FOREIGN KEY(group_id) REFERENCES organization_groups(group_id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workspace_user_grants (
    workspace_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner', 'member')),
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    PRIMARY KEY(workspace_id, user_id),
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS workspace_group_grants (
    workspace_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('owner', 'member')),
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    PRIMARY KEY(workspace_id, group_id),
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(group_id) REFERENCES organization_groups(group_id) ON DELETE CASCADE
);

WITH migration AS (
    INSERT INTO proxy_schema_migrations(name, applied_at)
    VALUES('workspace-access-v1', CURRENT_TIMESTAMP::TEXT)
    ON CONFLICT(name) DO NOTHING
    RETURNING 1
)
INSERT INTO workspace_user_grants(workspace_id, user_id, role, created_at, created_by)
SELECT w.workspace_id, w.created_by, 'owner', w.created_at, w.created_by
FROM workspaces w
JOIN organization_members om
  ON om.organization_id = w.organization_id AND om.user_id = w.created_by
WHERE EXISTS(SELECT 1 FROM migration)
ON CONFLICT(workspace_id, user_id) DO NOTHING;

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
-- Immutable attribution for newly written events; do not infer lost history.
-- No FK: workspace deletion must not erase this identifier.
ALTER TABLE organization_audit_events ADD COLUMN IF NOT EXISTS workspace_origin_id TEXT;
CREATE INDEX IF NOT EXISTS organization_audit_events_org_sequence
    ON organization_audit_events(organization_id, sequence DESC);
CREATE INDEX IF NOT EXISTS organization_audit_events_workspace_sequence
    ON organization_audit_events(organization_id, workspace_id, sequence DESC);
CREATE INDEX IF NOT EXISTS organization_audit_events_correlation
    ON organization_audit_events(correlation_id) WHERE correlation_id IS NOT NULL;
-- Audit summary (#191): a typed instant derived from occurred_at so range
-- scans use an index. New rows get it from a trigger (no call site can forget
-- it); old rows are backfilled newest-first by the maintenance worker, which
-- touches only this column. The parse is total: malformed text leaves NULL
-- and is marked so the backfill does not retry it forever.
ALTER TABLE organization_audit_events ADD COLUMN IF NOT EXISTS occurred_at_ts TIMESTAMPTZ;
ALTER TABLE organization_audit_events ADD COLUMN IF NOT EXISTS occurred_at_ts_unparsable BOOLEAN NOT NULL DEFAULT FALSE;
CREATE OR REPLACE FUNCTION treer_audit_parse_instant(value TEXT) RETURNS TIMESTAMPTZ AS $$
BEGIN
    RETURN value::TIMESTAMPTZ;
EXCEPTION WHEN OTHERS THEN
    RETURN NULL;
END;
-- STABLE, not IMMUTABLE: a text to timestamptz cast of a value without an
-- offset depends on the session time zone; recorded values carry one, and the
-- function is not used in an index.
$$ LANGUAGE plpgsql STABLE;
CREATE OR REPLACE FUNCTION treer_audit_fill_instant() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.occurred_at_ts IS NULL THEN
        NEW.occurred_at_ts := treer_audit_parse_instant(NEW.occurred_at);
        NEW.occurred_at_ts_unparsable := NEW.occurred_at_ts IS NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE OR REPLACE TRIGGER organization_audit_events_fill_instant
    BEFORE INSERT ON organization_audit_events FOR EACH ROW
    EXECUTE FUNCTION treer_audit_fill_instant();
CREATE INDEX IF NOT EXISTS organization_audit_events_workspace_time
    ON organization_audit_events((COALESCE(workspace_id, workspace_origin_id)), occurred_at_ts);

-- Shared across replicas. Omitted startup configuration preserves these values.
CREATE TABLE IF NOT EXISTS audit_maintenance_settings (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton),
    retention_days INTEGER NOT NULL DEFAULT 180 CHECK(retention_days BETWEEN 30 AND 3650),
    queue_capacity INTEGER NOT NULL DEFAULT 4096 CHECK(queue_capacity BETWEEN 1 AND 65536),
    pending INTEGER NOT NULL DEFAULT 0 CHECK(pending >= 0),
    overflow_total BIGINT NOT NULL DEFAULT 0 CHECK(overflow_total >= 0)
);
INSERT INTO audit_maintenance_settings(singleton) VALUES(TRUE) ON CONFLICT DO NOTHING;
-- occurred_at_ts backfill watermark (#191): rows with sequence >= the
-- watermark sequence carry occurred_at_ts (or are marked unparsable);
-- complete_since is the instant of the oldest such row; done once every
-- older row was visited.
ALTER TABLE audit_maintenance_settings ADD COLUMN IF NOT EXISTS backfill_watermark_sequence BIGINT;
ALTER TABLE audit_maintenance_settings ADD COLUMN IF NOT EXISTS backfill_complete_since TIMESTAMPTZ;
ALTER TABLE audit_maintenance_settings ADD COLUMN IF NOT EXISTS backfill_done BOOLEAN NOT NULL DEFAULT FALSE;
CREATE TABLE IF NOT EXISTS prompt_audit_outbox (
    event_id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL REFERENCES organizations(organization_id) ON DELETE CASCADE,
    workspace_id TEXT REFERENCES workspaces(workspace_id) ON DELETE SET NULL,
    workspace_origin_id TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    actor_kind TEXT NOT NULL CHECK(actor_kind IN ('user','agent','machine','service','system')),
    actor_id TEXT,
    actor_name TEXT,
    resource_id TEXT NOT NULL
);
CREATE OR REPLACE FUNCTION release_prompt_audit_capacity() RETURNS TRIGGER AS $$
BEGIN
    UPDATE audit_maintenance_settings SET pending=pending-1 WHERE singleton;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
CREATE OR REPLACE TRIGGER prompt_audit_capacity_released
    AFTER DELETE ON prompt_audit_outbox FOR EACH ROW
    EXECUTE FUNCTION release_prompt_audit_capacity();
-- One network round trip for durable admission. Invoker privileges only; this
-- function is not an authorization API and never accepts a prompt body.
CREATE OR REPLACE FUNCTION enqueue_prompt_audit(
    new_event TEXT, workspace TEXT, event_time TEXT, actor TEXT, actor_identifier TEXT, target TEXT
) RETURNS BOOLEAN AS $$
DECLARE full_queue BOOLEAN;
BEGIN
    SELECT pending>=queue_capacity INTO STRICT full_queue
        FROM audit_maintenance_settings WHERE singleton FOR UPDATE;
    IF full_queue THEN
        UPDATE audit_maintenance_settings SET overflow_total=overflow_total+1 WHERE singleton;
        RETURN FALSE;
    END IF;
    INSERT INTO prompt_audit_outbox(event_id,organization_id,workspace_id,workspace_origin_id,occurred_at,actor_kind,actor_id,actor_name,resource_id)
        SELECT new_event,organization_id,workspace_id,workspace_id,event_time,actor,actor_identifier,
            (SELECT preferred_name FROM users WHERE id=actor_identifier),target
        FROM workspaces WHERE workspace_id=workspace;
    IF NOT FOUND THEN RAISE EXCEPTION 'audit workspace unavailable'; END IF;
    UPDATE audit_maintenance_settings SET pending=pending+1 WHERE singleton;
    RETURN TRUE;
END;
$$ LANGUAGE plpgsql;
CREATE TABLE IF NOT EXISTS audit_retention_boundaries (
    organization_id TEXT NOT NULL REFERENCES organizations(organization_id) ON DELETE CASCADE,
    scope_id TEXT NOT NULL,
    deleted_through TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(organization_id,scope_id)
);
-- Date-prefix narrowing is indexable without an unsafe immutable timestamp cast.
CREATE INDEX IF NOT EXISTS organization_audit_events_retention_date
    ON organization_audit_events(left(occurred_at,10),sequence);

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
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS device_id TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS device_name TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS client TEXT;
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

CREATE TABLE IF NOT EXISTS login_attempts (
    bucket TEXT PRIMARY KEY,
    attempts INTEGER NOT NULL,
    window_started_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS login_attempts_window ON login_attempts(window_started_at);

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
ALTER TABLE agent_credentials ADD COLUMN IF NOT EXISTS recovery_enabled BOOLEAN NOT NULL DEFAULT TRUE;

-- Workspace presentation metadata only; these folders have no filesystem effects.
ALTER TABLE workspaces ADD COLUMN IF NOT EXISTS tree_revision BIGINT NOT NULL DEFAULT 0;
CREATE TABLE IF NOT EXISTS workload_folders (
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    folder_id TEXT NOT NULL,
    name TEXT NOT NULL,
    parent_id TEXT,
    PRIMARY KEY (workspace_id, folder_id),
    FOREIGN KEY (workspace_id, parent_id) REFERENCES workload_folders(workspace_id, folder_id)
);
CREATE TABLE IF NOT EXISTS workload_placements (
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agent_credentials(agent_id) ON DELETE CASCADE,
    folder_id TEXT,
    position BIGINT NOT NULL CHECK (position >= 0),
    PRIMARY KEY (workspace_id, agent_id),
    FOREIGN KEY (workspace_id, folder_id) REFERENCES workload_folders(workspace_id, folder_id)
);

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

-- Original human ownership of a new Agent reservation, never inferred from
-- names, client error details or an audit message. Existing identities are not
-- backfilled, and non-human creates do not acquire this cleanup authority.
CREATE TABLE IF NOT EXISTS agent_creation_reservations (
    agent_id TEXT PRIMARY KEY REFERENCES agent_credentials(agent_id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    server_id TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    confirmed_at TIMESTAMPTZ
);
ALTER TABLE agent_creation_reservations ADD COLUMN IF NOT EXISTS confirmed_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS machine_services (
    service_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    server_id TEXT NOT NULL,
    target_agent_id TEXT,
    target_host TEXT NOT NULL,
    target_port BIGINT NOT NULL CHECK(target_port BETWEEN 1 AND 65535),
    protocol TEXT NOT NULL CHECK(protocol IN ('tcp', 'http', 'udp')),
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS public_tcp_services (
    service_id TEXT PRIMARY KEY REFERENCES machine_services(service_id) ON DELETE CASCADE,
    public_port INTEGER NOT NULL UNIQUE CHECK(public_port BETWEEN 1 AND 65535),
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
        WHERE conrelid = 'machine_services'::regclass
          AND conname = 'machine_services_protocol_check'
          AND position('udp' in pg_get_constraintdef(oid)) = 0) THEN
        ALTER TABLE machine_services DROP CONSTRAINT machine_services_protocol_check;
        ALTER TABLE machine_services ADD CONSTRAINT machine_services_protocol_check
            CHECK (protocol IN ('tcp', 'http', 'udp'));
    END IF;
END $$;

ALTER TABLE machine_services
    ADD COLUMN IF NOT EXISTS target_agent_id TEXT;
ALTER TABLE machine_services
    ADD COLUMN IF NOT EXISTS is_primary BOOLEAN NOT NULL DEFAULT FALSE;
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
        WHERE conrelid = 'machine_services'::regclass
          AND conname = 'machine_services_primary_http_check') THEN
        ALTER TABLE machine_services ADD CONSTRAINT machine_services_primary_http_check
            CHECK (NOT is_primary OR (target_agent_id IS NOT NULL AND protocol = 'http'));
    END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS machine_services_agent_primary
    ON machine_services(workspace_id, target_agent_id) WHERE is_primary;
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
DROP INDEX IF EXISTS app_deployments_workspace_name_lower;
ALTER TABLE app_deployments ADD COLUMN IF NOT EXISTS startup_revision BIGINT NOT NULL DEFAULT 0 CHECK(startup_revision >= 0);
-- Browser UI contracts the App declares (e.g. app-tree.v1); presentation only.
ALTER TABLE app_deployments ADD COLUMN IF NOT EXISTS ui_capabilities JSONB NOT NULL DEFAULT '[]'::jsonb;
CREATE INDEX IF NOT EXISTS app_deployments_workspace_name ON app_deployments(workspace_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS app_deployments_workspace_hostname_lower
    ON app_deployments(workspace_id, lower(hostname));
CREATE INDEX IF NOT EXISTS app_deployments_server
    ON app_deployments(workspace_id, server_id);
CREATE INDEX IF NOT EXISTS app_deployments_runtime_agent
    ON app_deployments(workspace_id, runtime_agent_id)
    WHERE runtime_agent_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS workspace_policy_providers (
    workspace_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    service_id TEXT NOT NULL,
    failure_mode TEXT NOT NULL DEFAULT 'fail_closed'
        CHECK(failure_mode IN ('fail_closed', 'fail_open')),
    max_stale_seconds BIGINT NOT NULL DEFAULT 300
        CHECK(max_stale_seconds BETWEEN 0 AND 86400),
    revision_hint BIGINT NOT NULL DEFAULT 0 CHECK(revision_hint >= 0),
    updated_at TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY(app_id) REFERENCES app_deployments(app_id) ON DELETE CASCADE,
    FOREIGN KEY(service_id) REFERENCES machine_services(service_id) ON DELETE CASCADE
);

ALTER TABLE workspace_policy_providers ADD COLUMN IF NOT EXISTS publish_authority TEXT
    NOT NULL DEFAULT 'owner_session'
    CHECK(publish_authority IN ('owner_session', 'provider_operator'));

-- Old Proxy writers must not transfer an explicit authority to another Provider.
CREATE OR REPLACE FUNCTION reset_policy_publish_authority() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.app_id IS DISTINCT FROM OLD.app_id OR NEW.service_id IS DISTINCT FROM OLD.service_id THEN
        NEW.publish_authority := 'owner_session';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS policy_publish_authority_binding ON workspace_policy_providers;
CREATE TRIGGER policy_publish_authority_binding BEFORE UPDATE OF app_id, service_id
    ON workspace_policy_providers FOR EACH ROW EXECUTE FUNCTION reset_policy_publish_authority();

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

-- Separate from legacy unbound codes: mixed-version Proxies fail closed.
CREATE TABLE IF NOT EXISTS ingress_browser_auth_codes (
    code TEXT PRIMARY KEY,
    ingress_id TEXT NOT NULL REFERENCES service_ingresses(ingress_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    return_path TEXT NOT NULL,
    browser_nonce_hash TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT
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
-- Embed sessions (#156): a code minted by the Console for its iframe is bound
-- to one Agent and redeemed on the Agent origin for a partitioned session.
ALTER TABLE ingress_browser_auth_codes ADD COLUMN IF NOT EXISTS embed BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ingress_browser_auth_codes ADD COLUMN IF NOT EXISTS agent_id TEXT;
ALTER TABLE ingress_sessions ADD COLUMN IF NOT EXISTS embed BOOLEAN NOT NULL DEFAULT FALSE;
-- Prewarm (#198 step 3): a code minted for a hover redeems into a 10-minute
-- embed session; the first authorized document request rotates it into a
-- normal session (the old row is kept until its own expiry, marked rotated).
ALTER TABLE ingress_browser_auth_codes ADD COLUMN IF NOT EXISTS prewarm BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ingress_sessions ADD COLUMN IF NOT EXISTS prewarm BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE ingress_sessions ADD COLUMN IF NOT EXISTS rotated BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX IF NOT EXISTS ingress_sessions_user_ingress ON ingress_sessions(user_id, ingress_id);

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
    traffic_class TEXT NOT NULL CHECK(traffic_class IN ('virtual_network', 'service_ingress', 'virtual_host', 'agent_interface', 'direct_network')),
    source_type TEXT NOT NULL CHECK(source_type IN ('client', 'machine', 'internet')),
    source_id TEXT NOT NULL,
    destination_type TEXT NOT NULL CHECK(destination_type IN ('client', 'machine', 'internet')),
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
-- Schema execution is serialized by AuthStore's treer_schema advisory lock.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'traffic_usage_hourly'::regclass
        AND conname = 'traffic_usage_hourly_traffic_class_check'
        AND position('direct_network' in pg_get_constraintdef(oid)) = 0) THEN
        ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_traffic_class_check;
        ALTER TABLE traffic_usage_hourly ADD CHECK (traffic_class IN ('virtual_network', 'service_ingress', 'virtual_host', 'agent_interface', 'direct_network'));
    END IF;
    IF EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'traffic_usage_hourly'::regclass
        AND conname = 'traffic_usage_hourly_source_type_check'
        AND position('internet' in pg_get_constraintdef(oid)) = 0) THEN
        ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_source_type_check;
        ALTER TABLE traffic_usage_hourly ADD CHECK (source_type IN ('client', 'machine', 'internet'));
    END IF;
    IF EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'traffic_usage_hourly'::regclass
        AND conname = 'traffic_usage_hourly_destination_type_check'
        AND position('internet' in pg_get_constraintdef(oid)) = 0) THEN
        ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_destination_type_check;
        ALTER TABLE traffic_usage_hourly ADD CHECK (destination_type IN ('client', 'machine', 'internet'));
    END IF;
END $$;
CREATE TABLE IF NOT EXISTS agent_traffic_usage_hourly (
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    window_start BIGINT NOT NULL,
    traffic_class TEXT NOT NULL CHECK(traffic_class IN ('virtual_network', 'direct_network')),
    source_type TEXT NOT NULL CHECK(source_type IN ('agent', 'machine', 'internet')),
    source_id TEXT NOT NULL,
    destination_type TEXT NOT NULL CHECK(destination_type IN ('agent', 'machine', 'internet')),
    destination_id TEXT NOT NULL,
    payload_bytes BIGINT NOT NULL DEFAULT 0 CHECK(payload_bytes >= 0),
    payload_frames BIGINT NOT NULL DEFAULT 0 CHECK(payload_frames >= 0),
    billable_bytes BIGINT NOT NULL DEFAULT 0 CHECK(billable_bytes >= 0),
    meter_version INTEGER NOT NULL CHECK(meter_version > 0),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(workspace_id, window_start, traffic_class, source_type, source_id,
        destination_type, destination_id, meter_version)
);
CREATE TABLE IF NOT EXISTS network_usage_receipts (
    ticket TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
    server_id TEXT NOT NULL,
    agent_id TEXT,
    destination TEXT NOT NULL,
    sent_bytes BIGINT NOT NULL DEFAULT 0,
    received_bytes BIGINT NOT NULL DEFAULT 0,
    sent_chunks BIGINT NOT NULL DEFAULT 0,
    received_chunks BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    closed_at BIGINT
);
ALTER TABLE network_usage_receipts ADD COLUMN IF NOT EXISTS closed_at BIGINT;
CREATE INDEX IF NOT EXISTS network_usage_receipts_created_at ON network_usage_receipts(created_at);
CREATE INDEX IF NOT EXISTS agent_traffic_usage_hourly_workspace_window
    ON agent_traffic_usage_hourly(workspace_id, window_start DESC);
CREATE INDEX IF NOT EXISTS virtual_network_hosts_service
    ON virtual_network_hosts(workspace_id, service_id);

-- Core App email send: body-free audit, idempotency and rate reservations.
CREATE TABLE IF NOT EXISTS app_email_sends (
 workspace_id TEXT NOT NULL, app_id TEXT NOT NULL, idempotency_key TEXT NOT NULL,
 email_id TEXT UNIQUE NOT NULL, fingerprint TEXT NOT NULL, status TEXT NOT NULL,
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY(workspace_id,app_id,idempotency_key)
);
CREATE TABLE IF NOT EXISTS app_email_attempts (
 attempt_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, app_id TEXT NOT NULL,
 recipient_hash TEXT NOT NULL, status TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS app_email_attempts_rate ON app_email_attempts(workspace_id,created_at);

-- Policy Tree facts are read from a consistent primary-DB snapshot. Notifications
-- are invalidation hints only; a missed notification never authorizes stale paths.
CREATE OR REPLACE FUNCTION treer_tree_revision_changed() RETURNS trigger AS $$
DECLARE wid TEXT;
BEGIN
    IF TG_OP = 'UPDATE' AND OLD IS NOT DISTINCT FROM NEW THEN RETURN NULL; END IF;
    IF TG_OP = 'DELETE' THEN wid := OLD.workspace_id; ELSE wid := NEW.workspace_id; END IF;
    UPDATE workspaces SET tree_revision = tree_revision + 1 WHERE workspace_id = wid;
    PERFORM pg_notify('treer_tree_changed', wid);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE OR REPLACE TRIGGER treer_folders_revision AFTER INSERT OR UPDATE OR DELETE ON workload_folders
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_placements_revision AFTER INSERT OR UPDATE OR DELETE ON workload_placements
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_names_revision AFTER INSERT OR DELETE ON agent_names
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_names_update_revision AFTER UPDATE OF name ON agent_names
    FOR EACH ROW WHEN (OLD.name IS DISTINCT FROM NEW.name) EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_apps_revision AFTER INSERT OR DELETE ON app_deployments
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_apps_update_revision AFTER UPDATE OF name, runtime_agent_id ON app_deployments
    FOR EACH ROW WHEN (OLD.name IS DISTINCT FROM NEW.name OR OLD.runtime_agent_id IS DISTINCT FROM NEW.runtime_agent_id)
    EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_credentials_revision AFTER INSERT OR DELETE ON agent_credentials
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_credentials_update_revision AFTER UPDATE OF revoked_at ON agent_credentials
    FOR EACH ROW WHEN (OLD.revoked_at IS DISTINCT FROM NEW.revoked_at) EXECUTE FUNCTION treer_tree_revision_changed();
CREATE OR REPLACE TRIGGER treer_deleted_agents_revision AFTER INSERT OR DELETE ON deleted_agents
    FOR EACH ROW EXECUTE FUNCTION treer_tree_revision_changed();

-- A Policy Provider service must never be raw-public TCP. Both writers take the
-- same transaction advisory lock before checking the other table, so concurrent
-- publication and selection (on any Proxy replica) cannot both succeed.
CREATE OR REPLACE FUNCTION enforce_private_policy_provider_tcp() RETURNS trigger AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(74839202);
    IF TG_TABLE_NAME = 'public_tcp_services' THEN
        IF NEW.enabled AND EXISTS (SELECT 1 FROM workspace_policy_providers WHERE service_id = NEW.service_id) THEN
            RAISE EXCEPTION 'policy_provider_must_be_private' USING ERRCODE = 'check_violation';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM public_tcp_services WHERE service_id = NEW.service_id AND enabled) THEN
        RAISE EXCEPTION 'policy_provider_must_be_private' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS public_tcp_provider_private ON public_tcp_services;
CREATE TRIGGER public_tcp_provider_private BEFORE INSERT OR UPDATE OF enabled, service_id ON public_tcp_services
    FOR EACH ROW EXECUTE FUNCTION enforce_private_policy_provider_tcp();
DROP TRIGGER IF EXISTS policy_provider_tcp_private ON workspace_policy_providers;
CREATE TRIGGER policy_provider_tcp_private BEFORE INSERT OR UPDATE OF service_id ON workspace_policy_providers
    FOR EACH ROW EXECUTE FUNCTION enforce_private_policy_provider_tcp();

-- #140: workspace-shared Overview pins keyed by stable IDs. Exactly one target
-- column is set; App and folder deletion cascade here, agent deletion revokes
-- its credential row instead of deleting it, so the agent path deletes pins
-- explicitly and reads also filter on an unrevoked credential.
-- Composite targets so a pin row can never reference another workspace's
-- Agent or App at the database level (the write path checks too).
CREATE UNIQUE INDEX IF NOT EXISTS agent_credentials_workspace_agent ON agent_credentials(workspace_id, agent_id);
CREATE UNIQUE INDEX IF NOT EXISTS app_deployments_workspace_app ON app_deployments(workspace_id, app_id);
CREATE TABLE IF NOT EXISTS workspace_pins (
    workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    agent_id TEXT,
    app_id TEXT,
    folder_id TEXT,
    pinned_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    pinned_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    CHECK (num_nonnulls(agent_id, app_id, folder_id) = 1),
    FOREIGN KEY (workspace_id, agent_id) REFERENCES agent_credentials(workspace_id, agent_id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, app_id) REFERENCES app_deployments(workspace_id, app_id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, folder_id) REFERENCES workload_folders(workspace_id, folder_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS workspace_pins_agent ON workspace_pins(workspace_id, agent_id) WHERE agent_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS workspace_pins_app ON workspace_pins(workspace_id, app_id) WHERE app_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS workspace_pins_folder ON workspace_pins(workspace_id, folder_id) WHERE folder_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS workspace_pins_order ON workspace_pins(workspace_id, pinned_at DESC);

-- #146: owner overrides for public ingress protection (NULL = Proxy default).
-- #143 Phase 2: an ingress hostname may front one Agent's interface UI
-- instead of a machine service. Agent rows never carry a service and go away
-- with the Agent's credential row.
ALTER TABLE service_ingresses ALTER COLUMN service_id DROP NOT NULL;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS target_kind TEXT NOT NULL DEFAULT 'service';
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS agent_id TEXT REFERENCES agent_credentials(agent_id) ON DELETE CASCADE;
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'service_ingresses'::regclass
          AND conname = 'service_ingresses_target_check'
    ) THEN
        ALTER TABLE service_ingresses ADD CONSTRAINT service_ingresses_target_check
            CHECK(
                (target_kind = 'service' AND service_id IS NOT NULL AND agent_id IS NULL)
                OR (target_kind = 'agent_interface' AND agent_id IS NOT NULL AND service_id IS NULL AND access = 'workspace')
            );
    END IF;
END
$$;
CREATE UNIQUE INDEX IF NOT EXISTS service_ingresses_agent_id
    ON service_ingresses(agent_id) WHERE agent_id IS NOT NULL;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_rate_per_minute BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_rate_burst BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_concurrency BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_ip_rate_per_minute BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_ip_rate_burst BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_max_body_bytes BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_response_timeout_secs BIGINT;
ALTER TABLE service_ingresses ADD COLUMN IF NOT EXISTS limit_body_timeout_secs BIGINT;

-- Chunked, resumable machine uploads (#166): the authorization binding,
-- progress and receipt of one browser upload; bytes live in the Controller's
-- staging file. Timestamps are fixed-width RFC 3339 UTC text.
CREATE TABLE IF NOT EXISTS machine_uploads (
    upload_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    directory TEXT NOT NULL,
    file_name TEXT NOT NULL,
    overwrite BOOLEAN NOT NULL,
    size BIGINT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL,
    committed_offset BIGINT NOT NULL DEFAULT 0,
    parts JSONB NOT NULL DEFAULT '[]'::jsonb,
    lease_until TEXT,
    created_at TEXT NOT NULL,
    last_part_at TEXT NOT NULL,
    finished_at TEXT,
    result JSONB,
    failure_code TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS machine_uploads_idempotency
    ON machine_uploads(subject_kind, subject_id, workspace_id, server_id, idempotency_key);
CREATE INDEX IF NOT EXISTS machine_uploads_workspace_state ON machine_uploads(workspace_id, state);
CREATE INDEX IF NOT EXISTS machine_uploads_server_state ON machine_uploads(server_id, state);
CREATE INDEX IF NOT EXISTS machine_uploads_subject_state ON machine_uploads(subject_kind, subject_id, state);

-- The former Controller update (controller_update_*) and Console provider CLI
-- install (provider_cli_*) tables are no longer read or written. 0.2.0
-- deliberately leaves them in place; a later release drops them.
