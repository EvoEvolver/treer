use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Extension, Path as AxumPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use treer_protocol::{
    format_machine_enrollment_key, parse_machine_enrollment_key, AgentServerSnapshot, ApiError,
    CreateVirtualNetworkHostRequest, ProtocolError, ServerInfo, VirtualNetworkHost, WorkspaceInfo,
};
use url::Url;
use uuid::Uuid;

const SESSION_COOKIE: &str = "treer_session";
const SESSION_TTL_DAYS: i64 = 30;
const MACHINE_ENROLLMENT_TTL_MINUTES: i64 = 10;
const DEFAULT_ORGANIZATION_ID: &str = "org_default";
const DEFAULT_ORGANIZATION_NAME: &str = "Default organization";

#[derive(Clone)]
pub struct AuthStore {
    pool: SqlitePool,
    admin_password: Arc<str>,
    public_url: Url,
    disabled: bool,
}

#[derive(Clone, Debug)]
pub struct CurrentSession {
    pub token: String,
    pub username: String,
    pub is_admin: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrganizationInfo {
    pub organization_id: String,
    pub name: String,
    pub role: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OrganizationMember {
    pub username: String,
    pub role: String,
    pub joined_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineSession {
    pub server_id: Option<String>,
    pub workspace_id: Option<String>,
}

impl MachineSession {
    pub fn allows_workspace(&self, workspace_id: &str) -> bool {
        self.workspace_id
            .as_ref()
            .is_none_or(|expected| expected == workspace_id)
    }

    pub fn allows_server(&self, workspace_id: &str, server_id: &str) -> bool {
        self.allows_workspace(workspace_id)
            && self
                .server_id
                .as_ref()
                .is_none_or(|expected| expected == server_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineEnrollmentClaim {
    pub workspace_id: String,
    pub server_id: String,
    pub machine_token: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    invite: String,
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrganizationRequest {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemberRoleRequest {
    role: String,
}

impl AuthStore {
    pub async fn open(
        path: &Path,
        admin_password: String,
        public_url: Url,
        disabled: bool,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Self {
            pool,
            admin_password: admin_password.into(),
            public_url,
            disabled,
        };
        store.migrate().await?;
        Ok(store)
    }

    #[cfg(test)]
    async fn in_memory(admin_password: &str) -> Self {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database");
        let store = Self {
            pool,
            admin_password: admin_password.to_string().into(),
            public_url: Url::parse("https://treer.example/").expect("valid URL"),
            disabled: false,
        };
        store.migrate().await.expect("database migration");
        store
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS users (\
             id TEXT PRIMARY KEY, \
             username TEXT NOT NULL COLLATE NOCASE UNIQUE, \
             password_hash TEXT NOT NULL, \
             created_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS invitations (\
             token TEXT PRIMARY KEY, \
             created_at TEXT NOT NULL, \
             created_by TEXT NOT NULL, \
             used_at TEXT, \
             used_by TEXT)",
        )
        .execute(&self.pool)
        .await?;
        self.ensure_column("invitations", "organization_id", "TEXT")
            .await?;
        self.ensure_column("invitations", "role", "TEXT").await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sessions (\
             token TEXT PRIMARY KEY, \
             username TEXT NOT NULL, \
             is_admin INTEGER NOT NULL, \
             created_at TEXT NOT NULL, \
             expires_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS sessions_expires_at ON sessions(expires_at)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS machine_enrollments (\
             enrollment_id TEXT PRIMARY KEY, \
             workspace_id TEXT NOT NULL, \
             secret_hash TEXT NOT NULL, \
             created_at TEXT NOT NULL, \
             expires_at TEXT NOT NULL, \
             created_by TEXT NOT NULL, \
             used_at TEXT, \
             server_id TEXT)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS machines (\
             server_id TEXT PRIMARY KEY, \
             workspace_id TEXT NOT NULL, \
             secret_hash TEXT NOT NULL, \
             created_at TEXT NOT NULL, \
             enrolled_by TEXT NOT NULL, \
             revoked_at TEXT)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS machine_names (\
             server_id TEXT PRIMARY KEY, \
             workspace_id TEXT NOT NULL, \
             name TEXT NOT NULL, \
             updated_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS agent_names (\
             agent_id TEXT PRIMARY KEY, \
             workspace_id TEXT NOT NULL, \
             name TEXT NOT NULL, \
             updated_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS agent_names_workspace_id \
             ON agent_names(workspace_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS deleted_agents (\
             agent_id TEXT PRIMARY KEY, \
             workspace_id TEXT NOT NULL, \
             deleted_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS deleted_agents_workspace_id \
             ON deleted_agents(workspace_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS organizations (\
             organization_id TEXT PRIMARY KEY, \
             name TEXT NOT NULL, \
             created_at TEXT NOT NULL, \
             created_by TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS organization_members (\
             organization_id TEXT NOT NULL, \
             username TEXT NOT NULL COLLATE NOCASE, \
             role TEXT NOT NULL CHECK(role IN ('owner', 'admin', 'member')), \
             joined_at TEXT NOT NULL, \
             PRIMARY KEY(organization_id, username), \
             FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS organization_members_username \
             ON organization_members(username COLLATE NOCASE)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workspaces (\
             workspace_id TEXT PRIMARY KEY, \
             organization_id TEXT NOT NULL, \
             name TEXT NOT NULL, \
             created_at TEXT NOT NULL, \
             created_by TEXT NOT NULL, \
             FOREIGN KEY(organization_id) REFERENCES organizations(organization_id) ON DELETE CASCADE)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
             name TEXT PRIMARY KEY, \
             applied_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS workspaces_organization_id \
             ON workspaces(organization_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS virtual_network_hosts (\
             workspace_id TEXT NOT NULL, \
             hostname TEXT NOT NULL COLLATE NOCASE, \
             destination_server_id TEXT NOT NULL, \
             target_host TEXT NOT NULL, \
             target_port INTEGER, \
             created_at TEXT NOT NULL, \
             created_by TEXT NOT NULL, \
             PRIMARY KEY(workspace_id, hostname), \
             FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id) ON DELETE CASCADE)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS virtual_network_hosts_destination \
             ON virtual_network_hosts(workspace_id, destination_server_id)",
        )
        .execute(&self.pool)
        .await?;

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO organizations(organization_id, name, created_at, created_by) \
             VALUES(?, ?, ?, 'admin')",
        )
        .bind(DEFAULT_ORGANIZATION_ID)
        .bind(DEFAULT_ORGANIZATION_NAME)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO organization_members(organization_id, username, role, joined_at) \
             VALUES(?, 'admin', 'owner', ?)",
        )
        .bind(DEFAULT_ORGANIZATION_ID)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        let legacy_members_migrated = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM schema_migrations WHERE name = 'organization_members_v1'",
        )
        .fetch_one(&self.pool)
        .await?
            != 0;
        if !legacy_members_migrated {
            let mut transaction = self.pool.begin().await?;
            sqlx::query(
                "INSERT OR IGNORE INTO organization_members(organization_id, username, role, joined_at) \
                 SELECT ?, username, 'member', ? FROM users",
            )
            .bind(DEFAULT_ORGANIZATION_ID)
            .bind(&now)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO schema_migrations(name, applied_at) \
                 VALUES('organization_members_v1', ?)",
            )
            .bind(&now)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES('default', ?, 'Default', ?, 'admin')",
        )
        .bind(DEFAULT_ORGANIZATION_ID)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             SELECT workspace_id, ?, workspace_id, ?, 'migration' FROM (\
               SELECT workspace_id FROM machines \
               UNION SELECT workspace_id FROM machine_enrollments \
               UNION SELECT workspace_id FROM machine_names \
               UNION SELECT workspace_id FROM agent_names\
             )",
        )
        .bind(DEFAULT_ORGANIZATION_ID)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE invitations SET organization_id = ?, role = 'member' \
             WHERE organization_id IS NULL OR role IS NULL",
        )
        .bind(DEFAULT_ORGANIZATION_ID)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn ensure_column(
        &self,
        table: &str,
        column: &str,
        definition: &str,
    ) -> anyhow::Result<()> {
        let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&self.pool)
            .await?;
        if !rows
            .iter()
            .any(|row| row.get::<String, _>("name") == column)
        {
            sqlx::query(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn all_workspaces(&self) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces ORDER BY workspace_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn list_organizations(
        &self,
        username: &str,
    ) -> Result<Vec<OrganizationInfo>, AuthFailure> {
        let rows = if self.disabled {
            sqlx::query(
                "SELECT organization_id, name, created_at, 'owner' AS role \
                 FROM organizations ORDER BY name COLLATE NOCASE, organization_id",
            )
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT o.organization_id, o.name, o.created_at, m.role \
                 FROM organizations o \
                 JOIN organization_members m ON m.organization_id = o.organization_id \
                 WHERE m.username = ? COLLATE NOCASE \
                 ORDER BY o.name COLLATE NOCASE, o.organization_id",
            )
            .bind(username)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationInfo {
                organization_id: row.get("organization_id"),
                name: row.get("name"),
                role: row.get("role"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    pub async fn create_organization(
        &self,
        username: &str,
        name: &str,
    ) -> Result<OrganizationInfo, AuthFailure> {
        let name = validate_resource_name(name, "organization")?;
        let organization_id = format!("org_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES(?, ?, ?, ?)",
        )
        .bind(&organization_id)
        .bind(&name)
        .bind(&now)
        .bind(username)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO organization_members(organization_id, username, role, joined_at) \
             VALUES(?, ?, 'owner', ?)",
        )
        .bind(&organization_id)
        .bind(username)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(OrganizationInfo {
            organization_id,
            name,
            role: "owner".to_string(),
            created_at: now,
        })
    }

    pub async fn require_organization_member(
        &self,
        organization_id: &str,
        username: &str,
    ) -> Result<String, AuthFailure> {
        self.membership_role(organization_id, username)
            .await?
            .ok_or_else(|| {
                AuthFailure::forbidden(
                    "organization_access_denied",
                    "you are not a member of this organization",
                )
            })
    }

    pub async fn require_workspace_member(
        &self,
        workspace_id: &str,
        username: &str,
    ) -> Result<(), AuthFailure> {
        let organization_id = sqlx::query_scalar::<_, String>(
            "SELECT organization_id FROM workspaces WHERE workspace_id = ?",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("workspace_not_found", "workspace does not exist"))?;
        self.require_organization_member(&organization_id, username)
            .await?;
        Ok(())
    }

    pub async fn list_members(
        &self,
        organization_id: &str,
        username: &str,
    ) -> Result<Vec<OrganizationMember>, AuthFailure> {
        self.require_organization_member(organization_id, username)
            .await?;
        let rows = sqlx::query(
            "SELECT username, role, joined_at FROM organization_members \
             WHERE organization_id = ? \
             ORDER BY CASE role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, \
             username COLLATE NOCASE",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(rows
            .into_iter()
            .map(|row| OrganizationMember {
                username: row.get("username"),
                role: row.get("role"),
                joined_at: row.get("joined_at"),
            })
            .collect())
    }

    pub async fn list_workspaces(
        &self,
        organization_id: &str,
        username: &str,
    ) -> Result<Vec<WorkspaceInfo>, AuthFailure> {
        self.require_organization_member(organization_id, username)
            .await?;
        let rows = sqlx::query(
            "SELECT workspace_id, name, created_at FROM workspaces \
             WHERE organization_id = ? ORDER BY name COLLATE NOCASE, workspace_id",
        )
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter().map(workspace_from_row).collect()
    }

    pub async fn create_workspace(
        &self,
        organization_id: &str,
        workspace_id: &str,
        name: &str,
        username: &str,
    ) -> Result<WorkspaceInfo, AuthFailure> {
        self.require_organization_member(organization_id, username)
            .await?;
        let name = validate_resource_name(name, "workspace")?;
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(workspace_id)
        .bind(organization_id)
        .bind(&name)
        .bind(now.to_rfc3339())
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("workspace_exists", "workspace already exists")
            } else {
                AuthFailure::database(error)
            }
        })?;
        Ok(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name,
            created_at: now,
        })
    }

    pub async fn list_virtual_network_hosts(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<VirtualNetworkHost>, AuthFailure> {
        let rows = sqlx::query(
            "SELECT workspace_id, hostname, destination_server_id, target_host, target_port, \
             created_at, created_by FROM virtual_network_hosts \
             WHERE workspace_id = ? ORDER BY hostname COLLATE NOCASE",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        rows.into_iter()
            .map(virtual_network_host_from_row)
            .collect()
    }

    pub async fn resolve_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<Option<VirtualNetworkHost>, AuthFailure> {
        let hostname = match normalize_virtual_hostname(hostname) {
            Ok(hostname) => hostname,
            Err(_) => return Ok(None),
        };
        sqlx::query(
            "SELECT workspace_id, hostname, destination_server_id, target_host, target_port, \
             created_at, created_by FROM virtual_network_hosts \
             WHERE workspace_id = ? AND hostname = ? COLLATE NOCASE",
        )
        .bind(workspace_id)
        .bind(hostname)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .map(virtual_network_host_from_row)
        .transpose()
    }

    pub async fn create_virtual_network_host(
        &self,
        workspace_id: &str,
        username: &str,
        request: CreateVirtualNetworkHostRequest,
    ) -> Result<VirtualNetworkHost, AuthFailure> {
        let hostname = normalize_virtual_hostname(&request.hostname)?;
        let target_host = request.target_host.trim();
        if target_host.is_empty() || target_host.len() > 253 {
            return Err(AuthFailure::bad_request(
                "invalid_virtual_host",
                "target_host must be a non-empty hostname or address",
            ));
        }
        if request.target_port == Some(0) {
            return Err(AuthFailure::bad_request(
                "invalid_virtual_host",
                "target_port must be between 1 and 65535",
            ));
        }
        let record = VirtualNetworkHost {
            workspace_id: workspace_id.to_string(),
            hostname,
            destination_server_id: request.destination_server_id,
            target_host: target_host.to_string(),
            target_port: request.target_port,
            created_at: Utc::now(),
            created_by: username.to_string(),
        };
        let result = sqlx::query(
            "INSERT INTO virtual_network_hosts(\
             workspace_id, hostname, destination_server_id, target_host, target_port, \
             created_at, created_by) VALUES(?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&record.workspace_id)
        .bind(&record.hostname)
        .bind(&record.destination_server_id)
        .bind(&record.target_host)
        .bind(record.target_port.map(i64::from))
        .bind(record.created_at.to_rfc3339())
        .bind(&record.created_by)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(record),
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                Err(AuthFailure::conflict(
                    "virtual_host_exists",
                    "virtual hostname already exists in this workspace",
                ))
            }
            Err(error) => Err(AuthFailure::database(error)),
        }
    }

    pub async fn delete_virtual_network_host(
        &self,
        workspace_id: &str,
        hostname: &str,
    ) -> Result<(), AuthFailure> {
        let hostname = normalize_virtual_hostname(hostname)?;
        let result = sqlx::query(
            "DELETE FROM virtual_network_hosts WHERE workspace_id = ? AND hostname = ? COLLATE NOCASE",
        )
        .bind(workspace_id)
        .bind(hostname)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() == 0 {
            return Err(AuthFailure::not_found(
                "virtual_host_not_found",
                "virtual host does not exist",
            ));
        }
        Ok(())
    }

    async fn membership_role(
        &self,
        organization_id: &str,
        username: &str,
    ) -> Result<Option<String>, AuthFailure> {
        if self.disabled {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM organizations WHERE organization_id = ?",
            )
            .bind(organization_id)
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            return Ok((exists != 0).then(|| "owner".to_string()));
        }
        sqlx::query_scalar(
            "SELECT role FROM organization_members \
             WHERE organization_id = ? AND username = ? COLLATE NOCASE",
        )
        .bind(organization_id)
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)
    }

    async fn require_manager(
        &self,
        organization_id: &str,
        username: &str,
    ) -> Result<String, AuthFailure> {
        let role = self
            .require_organization_member(organization_id, username)
            .await?;
        if matches!(role.as_str(), "owner" | "admin") {
            Ok(role)
        } else {
            Err(AuthFailure::forbidden(
                "organization_manager_required",
                "organization owner or administrator access required",
            ))
        }
    }

    pub async fn set_machine_name(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES(?, ?, ?, ?) \
             ON CONFLICT(server_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(server_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn set_agent_name(
        &self,
        workspace_id: &str,
        agent_id: &str,
        name: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query(
            "INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) \
             VALUES(?, ?, ?, ?) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, name = excluded.name, \
             updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn apply_server_name(&self, server: &mut ServerInfo) -> Result<(), AuthFailure> {
        if server.name.trim().is_empty() {
            server.name.clone_from(&server.hostname);
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM machine_names WHERE server_id = ? AND workspace_id = ?",
        )
        .bind(&server.server_id)
        .bind(&server.workspace_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(name) = name {
            server.name = name;
        }
        Ok(())
    }

    pub async fn apply_agent_names(
        &self,
        snapshot: &mut AgentServerSnapshot,
    ) -> Result<Vec<String>, AuthFailure> {
        let rows = sqlx::query("SELECT agent_id, name FROM agent_names WHERE workspace_id = ?")
            .bind(&snapshot.server.workspace_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        let names: HashMap<String, String> = rows
            .into_iter()
            .map(|row| (row.get("agent_id"), row.get("name")))
            .collect();
        for agent in &mut snapshot.agents {
            if let Some(name) = names.get(&agent.agent_id) {
                agent.name.clone_from(name);
            }
        }
        let deleted = sqlx::query_scalar::<_, String>(
            "SELECT agent_id FROM deleted_agents WHERE workspace_id = ?",
        )
        .bind(&snapshot.server.workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let deleted_set: std::collections::HashSet<_> =
            deleted.iter().map(String::as_str).collect();
        snapshot
            .agents
            .retain(|agent| !deleted_set.contains(agent.agent_id.as_str()));
        Ok(deleted)
    }

    pub async fn delete_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO deleted_agents(agent_id, workspace_id, deleted_at) \
             VALUES(?, ?, ?) \
             ON CONFLICT(agent_id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, deleted_at = excluded.deleted_at",
        )
        .bind(agent_id)
        .bind(workspace_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM agent_names WHERE agent_id = ? AND workspace_id = ?")
            .bind(agent_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn delete_machine(
        &self,
        workspace_id: &str,
        server_id: &str,
        agent_ids: &[String],
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        if !self.disabled {
            let update = sqlx::query(
                "UPDATE machines SET revoked_at = ? \
                 WHERE server_id = ? AND workspace_id = ? AND revoked_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            if update.rows_affected() != 1 {
                return Err(AuthFailure::conflict(
                    "machine_already_deleted",
                    "machine credential is already revoked",
                ));
            }
        }
        sqlx::query("DELETE FROM machine_names WHERE server_id = ? AND workspace_id = ?")
            .bind(server_id)
            .bind(workspace_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM virtual_network_hosts WHERE workspace_id = ? AND destination_server_id = ?",
        )
        .bind(workspace_id)
        .bind(server_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        for agent_id in agent_ids {
            sqlx::query("DELETE FROM agent_names WHERE agent_id = ? AND workspace_id = ?")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
            sqlx::query("DELETE FROM deleted_agents WHERE agent_id = ? AND workspace_id = ?")
                .bind(agent_id)
                .bind(workspace_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
        }
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn create_machine_enrollment(
        &self,
        workspace_id: &str,
        created_by: &str,
    ) -> Result<String, AuthFailure> {
        let enrollment_id = Uuid::new_v4().simple().to_string();
        let secret = random_secret();
        let enrollment = format_machine_enrollment_key(workspace_id, &enrollment_id, &secret)
            .map_err(|error| AuthFailure::internal("machine_enrollment_error", error.message))?;
        let identifier = enrollment
            .split_once('.')
            .map(|(identifier, _)| identifier)
            .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash = hash_password(&secret)?;
        let now = Utc::now();
        let expires_at = now + Duration::minutes(MACHINE_ENROLLMENT_TTL_MINUTES);
        sqlx::query(
            "INSERT INTO machine_enrollments(\
             enrollment_id, workspace_id, secret_hash, created_at, expires_at, created_by) \
             VALUES(?, ?, ?, ?, ?, ?)",
        )
        .bind(identifier)
        .bind(workspace_id)
        .bind(secret_hash)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .bind(created_by)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(enrollment)
    }

    pub async fn claim_machine_enrollment(
        &self,
        token: &str,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let enrollment =
            parse_machine_enrollment_key(token).map_err(|_| invalid_machine_enrollment())?;
        let now = Utc::now();
        let row = sqlx::query(
            "SELECT workspace_id, secret_hash, created_by \
             FROM machine_enrollments \
             WHERE enrollment_id = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_machine_enrollment)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(&enrollment.secret, &secret_hash) {
            return Err(invalid_machine_enrollment());
        }
        let workspace_id: String = row.get("workspace_id");
        if workspace_id != enrollment.workspace_id {
            return Err(invalid_machine_enrollment());
        }
        let created_by: String = row.get("created_by");
        let server_id = format!("srv_{}", Uuid::new_v4().simple());
        let machine_secret = random_secret();
        let machine_secret_hash = hash_password(&machine_secret)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let update = sqlx::query(
            "UPDATE machine_enrollments SET used_at = ?, server_id = ? \
             WHERE enrollment_id = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(now.to_rfc3339())
        .bind(&server_id)
        .bind(&enrollment.identifier)
        .bind(now.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if update.rows_affected() != 1 {
            return Err(invalid_machine_enrollment());
        }
        sqlx::query(
            "INSERT INTO machines(\
             server_id, workspace_id, secret_hash, created_at, enrolled_by) \
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(&server_id)
        .bind(&workspace_id)
        .bind(machine_secret_hash)
        .bind(now.to_rfc3339())
        .bind(created_by)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(MachineEnrollmentClaim {
            workspace_id,
            machine_token: format!("{server_id}.{machine_secret}"),
            server_id,
        })
    }

    pub async fn claim_machine_enrollment_from_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<MachineEnrollmentClaim, AuthFailure> {
        let token = bearer_token(headers).ok_or_else(invalid_machine_enrollment)?;
        self.claim_machine_enrollment(token).await
    }

    pub async fn authenticate_machine(
        &self,
        headers: &HeaderMap,
    ) -> Result<MachineSession, AuthFailure> {
        if self.disabled {
            return Ok(MachineSession {
                server_id: None,
                workspace_id: None,
            });
        }
        let token = bearer_token(headers).ok_or_else(machine_auth_required)?;
        let (server_id, secret) = parse_credential(token, "srv_")?;
        let row = sqlx::query(
            "SELECT workspace_id, secret_hash FROM machines \
             WHERE server_id = ? AND revoked_at IS NULL",
        )
        .bind(server_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(machine_auth_required)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(secret, &secret_hash) {
            return Err(machine_auth_required());
        }
        Ok(MachineSession {
            server_id: Some(server_id.to_string()),
            workspace_id: Some(row.get("workspace_id")),
        })
    }

    async fn login(&self, username: &str, password: &str) -> Result<CurrentSession, AuthFailure> {
        let username = username.trim();
        let is_admin = username.eq_ignore_ascii_case("admin");
        let valid = if is_admin {
            password == self.admin_password.as_ref()
        } else {
            let hash = sqlx::query_scalar::<_, String>(
                "SELECT password_hash FROM users WHERE username = ? COLLATE NOCASE",
            )
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
            hash.is_some_and(|hash| verify_password(password, &hash))
        };
        if !valid {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid username or password",
            ));
        }
        self.create_session(username, is_admin).await
    }

    async fn create_session(
        &self,
        username: &str,
        is_admin: bool,
    ) -> Result<CurrentSession, AuthFailure> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO sessions(token, username, is_admin, created_at, expires_at) \
             VALUES(?, ?, ?, ?, ?)",
        )
        .bind(&token)
        .bind(username)
        .bind(is_admin)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token,
            username: username.to_string(),
            is_admin,
        })
    }

    async fn session(&self, token: &str) -> Result<Option<CurrentSession>, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            "SELECT username, is_admin FROM sessions WHERE token = ? AND expires_at > ?",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(row.map(|row| CurrentSession {
            token: token.to_string(),
            username: row.get("username"),
            is_admin: row.get::<i64, _>("is_admin") != 0,
        }))
    }

    async fn logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn create_invitation(
        &self,
        organization_id: &str,
        created_by: &str,
    ) -> Result<(String, Url), AuthFailure> {
        self.require_manager(organization_id, created_by).await?;
        let token = format!("inv_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO invitations(\
             token, created_at, created_by, organization_id, role) \
             VALUES(?, ?, ?, ?, 'member')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .bind(created_by)
        .bind(organization_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut url = self.public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    pub async fn update_member_role(
        &self,
        organization_id: &str,
        actor: &str,
        target: &str,
        role: &str,
    ) -> Result<(), AuthFailure> {
        let actor_role = self
            .require_organization_member(organization_id, actor)
            .await?;
        if actor_role != "owner" {
            return Err(AuthFailure::forbidden(
                "organization_owner_required",
                "organization owner access required",
            ));
        }
        if !matches!(role, "admin" | "member") {
            return Err(AuthFailure::bad_request(
                "invalid_member_role",
                "member role must be admin or member",
            ));
        }
        let result = sqlx::query(
            "UPDATE organization_members SET role = ? \
             WHERE organization_id = ? AND username = ? COLLATE NOCASE AND role != 'owner'",
        )
        .bind(role)
        .bind(organization_id)
        .bind(target)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "member_not_found",
                "member does not exist or is the organization owner",
            ));
        }
        Ok(())
    }

    pub async fn remove_member(
        &self,
        organization_id: &str,
        actor: &str,
        target: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let target_role = self
            .membership_role(organization_id, target)
            .await?
            .ok_or_else(|| AuthFailure::not_found("member_not_found", "member does not exist"))?;
        if target_role == "owner" {
            return Err(AuthFailure::conflict(
                "owner_cannot_be_removed",
                "the organization owner cannot be removed",
            ));
        }
        sqlx::query(
            "DELETE FROM organization_members \
             WHERE organization_id = ? AND username = ? COLLATE NOCASE",
        )
        .bind(organization_id)
        .bind(target)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    async fn register(
        &self,
        invite: &str,
        username: &str,
        password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let username = validate_username(username)?;
        if password.len() < 8 {
            return Err(AuthFailure::bad_request(
                "invalid_password",
                "password must contain at least 8 characters",
            ));
        }
        let invitation = sqlx::query(
            "SELECT organization_id, role FROM invitations WHERE token = ? AND used_at IS NULL",
        )
        .bind(invite)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::bad_request(
                "invalid_invitation",
                "invitation is invalid or already used",
            )
        })?;
        let organization_id: String = invitation.get("organization_id");
        let role: String = invitation.get("role");
        let password_hash = hash_password(password)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let user_id = format!("usr_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO users(id, username, password_hash, created_at) VALUES(?, ?, ?, ?)",
        )
        .bind(&user_id)
        .bind(&username)
        .bind(password_hash)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("username_exists", "username is already registered")
            } else {
                AuthFailure::database(error)
            }
        })?;
        let result = sqlx::query(
            "UPDATE invitations SET used_at = ?, used_by = ? WHERE token = ? AND used_at IS NULL",
        )
        .bind(&now)
        .bind(&user_id)
        .bind(invite)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::bad_request(
                "invalid_invitation",
                "invitation is invalid or already used",
            ));
        }
        sqlx::query(
            "INSERT INTO organization_members(organization_id, username, role, joined_at) \
             VALUES(?, ?, ?, ?)",
        )
        .bind(organization_id)
        .bind(&username)
        .bind(role)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        self.create_session(&username, false).await
    }
}

pub async fn require_user(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate_request(&auth, request.headers()).await {
        Ok(session) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

pub async fn require_workspace_access(
    State(auth): State<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    request: Request,
    next: Next,
) -> Response {
    let Some(workspace_id) = workspace_id_from_api_path(request.uri().path()) else {
        return next.run(request).await;
    };
    match auth
        .require_workspace_member(&workspace_id, &session.username)
        .await
    {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

pub async fn require_machine(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match auth.authenticate_machine(request.headers()).await {
        Ok(session) if machine_workspace_matches(&session, request.uri().path()) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Ok(_) => AuthFailure::forbidden(
            "machine_workspace_mismatch",
            "machine credentials do not grant access to this workspace",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn authenticate_request(
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<CurrentSession, AuthFailure> {
    if auth.disabled {
        return Ok(CurrentSession {
            token: "local".to_string(),
            username: "local".to_string(),
            is_admin: true,
        });
    }
    let token = cookie_value(headers, SESSION_COOKIE).ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "authentication required")
    })?;
    auth.session(&token).await?.ok_or_else(|| {
        AuthFailure::unauthorized("authentication_required", "authentication required")
    })
}

pub async fn login(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth.login(&request.username, &request.password).await?;
    Ok(session_response(&auth, &session))
}

pub async fn register(
    Extension(auth): Extension<AuthStore>,
    Json(request): Json<RegisterRequest>,
) -> Result<Response, AuthFailure> {
    let session = auth
        .register(&request.invite, &request.username, &request.password)
        .await?;
    Ok(session_response(&auth, &session))
}

pub async fn me(Extension(session): Extension<CurrentSession>) -> Json<Value> {
    Json(json!({ "username": session.username, "is_admin": session.is_admin }))
}

pub async fn organizations(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organizations": auth.list_organizations(&session.username).await?
    })))
}

pub async fn create_organization_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<CreateOrganizationRequest>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "organization": auth.create_organization(&session.username, &request.name).await?
    })))
}

pub async fn members(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    let role = auth
        .require_organization_member(&organization_id, &session.username)
        .await?;
    Ok(Json(json!({
        "members": auth.list_members(&organization_id, &session.username).await?,
        "current_role": role
    })))
}

pub async fn logout(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Response, AuthFailure> {
    auth.logout(&session.token).await?;
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_cookie_suffix(&auth)
    );
    Ok((
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(AuthFailure::header)?,
        )],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

pub async fn create_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath(organization_id): AxumPath<String>,
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth
        .create_invitation(&organization_id, &session.username)
        .await?;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
}

pub async fn update_member_role_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, username)): AxumPath<(String, String)>,
    Json(request): Json<UpdateMemberRoleRequest>,
) -> Result<Json<Value>, AuthFailure> {
    auth.update_member_role(
        &organization_id,
        &session.username,
        &username,
        &request.role,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_member_handler(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    AxumPath((organization_id, username)): AxumPath<(String, String)>,
) -> Result<Json<Value>, AuthFailure> {
    auth.remove_member(&organization_id, &session.username, &username)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

fn session_response(auth: &AuthStore, session: &CurrentSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        session.token,
        SESSION_TTL_DAYS * 24 * 60 * 60,
        secure_cookie_suffix(auth)
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "username": session.username, "is_admin": session.is_admin })),
    )
        .into_response()
}

fn secure_cookie_suffix(auth: &AuthStore) -> &'static str {
    if auth.public_url.scheme() == "https" {
        "; Secure"
    } else {
        ""
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
}

fn machine_workspace_matches(session: &MachineSession, path: &str) -> bool {
    let Some(encoded_workspace) = path
        .strip_prefix("/agent/workspaces/")
        .and_then(|rest| rest.split('/').next())
    else {
        return false;
    };
    percent_encoding::percent_decode_str(encoded_workspace)
        .decode_utf8()
        .is_ok_and(|workspace| session.allows_workspace(&workspace))
}

fn workspace_id_from_api_path(path: &str) -> Option<String> {
    let encoded = path.strip_prefix("/api/workspaces/")?.split('/').next()?;
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(|workspace| workspace.into_owned())
}

fn random_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn parse_credential<'a>(
    token: &'a str,
    expected_prefix: &str,
) -> Result<(&'a str, &'a str), AuthFailure> {
    let (identifier, secret) = token.split_once('.').ok_or_else(machine_auth_required)?;
    if !identifier.starts_with(expected_prefix) || secret.len() != 64 {
        return Err(machine_auth_required());
    }
    Ok((identifier, secret))
}

fn machine_auth_required() -> AuthFailure {
    AuthFailure::unauthorized(
        "machine_authentication_required",
        "valid machine credentials are required",
    )
}

fn invalid_machine_enrollment() -> AuthFailure {
    AuthFailure::unauthorized(
        "invalid_machine_enrollment",
        "machine enrollment token is invalid, expired, or already used",
    )
}

fn validate_username(username: &str) -> Result<String, AuthFailure> {
    let username = username.trim();
    if !(3..=32).contains(&username.len())
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || username.eq_ignore_ascii_case("admin")
    {
        return Err(AuthFailure::bad_request(
            "invalid_username",
            "username must be 3-32 letters, numbers, dots, dashes, or underscores",
        ));
    }
    Ok(username.to_string())
}

fn validate_resource_name(value: &str, resource: &str) -> Result<String, AuthFailure> {
    let value = value.trim();
    if value.is_empty() || value.len() > 80 || value.chars().any(|character| character.is_control())
    {
        return Err(AuthFailure::bad_request(
            "invalid_name",
            &format!("{resource} name must be 1-80 printable characters"),
        ));
    }
    Ok(value.to_string())
}

fn workspace_from_row(row: sqlx::sqlite::SqliteRow) -> Result<WorkspaceInfo, AuthFailure> {
    let created_at: String = row.get("created_at");
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            tracing::error!(%error, "invalid workspace timestamp in database");
            AuthFailure::internal(
                "database_error",
                "workspace timestamp is invalid".to_string(),
            )
        })?;
    Ok(WorkspaceInfo {
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        created_at,
    })
}

fn normalize_virtual_hostname(value: &str) -> Result<String, AuthFailure> {
    let hostname = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels_valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    let conflicts_with_direct_route = hostname
        .strip_suffix(".treer")
        .is_some_and(|route| route.contains(".via."));
    if !labels_valid || conflicts_with_direct_route {
        return Err(AuthFailure::bad_request(
            "invalid_virtual_hostname",
            "hostname must contain valid DNS labels and must not conflict with a Treer via route",
        ));
    }
    Ok(hostname)
}

fn virtual_network_host_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> Result<VirtualNetworkHost, AuthFailure> {
    let created_at = row
        .get::<String, _>("created_at")
        .parse()
        .map_err(|error| {
            AuthFailure::internal(
                "database_error",
                format!("virtual network host has invalid created_at: {error}"),
            )
        })?;
    let target_port = row
        .get::<Option<i64>, _>("target_port")
        .map(u16::try_from)
        .transpose()
        .map_err(|_| {
            AuthFailure::internal(
                "database_error",
                "virtual network host has invalid target_port".to_string(),
            )
        })?;
    Ok(VirtualNetworkHost {
        workspace_id: row.get("workspace_id"),
        hostname: row.get("hostname"),
        destination_server_id: row.get("destination_server_id"),
        target_host: row.get("target_host"),
        target_port,
        created_at,
        created_by: row.get("created_by"),
    })
}

fn hash_password(password: &str) -> Result<String, AuthFailure> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AuthFailure::internal("password_hash_error", error.to_string()))
}

fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[derive(Debug)]
pub struct AuthFailure {
    status: StatusCode,
    error: ProtocolError,
}

impl AuthFailure {
    fn not_found(code: &str, message: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    fn unauthorized(code: &str, message: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    fn forbidden(code: &str, message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    fn bad_request(code: &str, message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn conflict(code: &str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    fn internal(code: &str, message: String) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    fn database(error: sqlx::Error) -> Self {
        tracing::error!(%error, "authentication database error");
        Self::internal("database_error", "database operation failed".to_string())
    }

    fn header(error: axum::http::header::InvalidHeaderValue) -> Self {
        tracing::error!(%error, "failed to encode session cookie");
        Self::internal("session_error", "failed to create session".to_string())
    }

    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            error: ProtocolError::new(code, message),
        }
    }

    pub fn into_parts(self) -> (StatusCode, ProtocolError) {
        (self.status, self.error)
    }
}

impl IntoResponse for AuthFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_protocol::{AgentInfo, AgentStatus, CreateVirtualNetworkHostRequest, ServerStatus};

    #[tokio::test]
    async fn virtual_network_hosts_are_normalized_resolved_and_cleaned_up() {
        let mut store = AuthStore::in_memory("owner-password").await;
        let record = store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "API.Dev.Example.".to_string(),
                    destination_server_id: "destination".to_string(),
                    target_host: "127.0.0.1".to_string(),
                    target_port: Some(8080),
                },
            )
            .await
            .expect("create virtual host");
        assert_eq!(record.hostname, "api.dev.example");
        assert_eq!(
            store
                .resolve_virtual_network_host("default", "API.DEV.EXAMPLE")
                .await
                .expect("resolve virtual host"),
            Some(record.clone())
        );
        assert!(store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "api.dev.example".to_string(),
                    destination_server_id: "destination".to_string(),
                    target_host: "localhost".to_string(),
                    target_port: None,
                },
            )
            .await
            .is_err());
        assert!(store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "host.via.machine.treer".to_string(),
                    destination_server_id: "destination".to_string(),
                    target_host: "localhost".to_string(),
                    target_port: None,
                },
            )
            .await
            .is_err());
        store
            .create_virtual_network_host(
                "default",
                "admin",
                CreateVirtualNetworkHostRequest {
                    hostname: "git.via.example".to_string(),
                    destination_server_id: "destination".to_string(),
                    target_host: "localhost".to_string(),
                    target_port: None,
                },
            )
            .await
            .expect("via label outside a Treer direct route is valid");
        store.disabled = true;
        store
            .delete_machine("default", "destination", &[])
            .await
            .expect("delete destination machine");
        assert!(store
            .list_virtual_network_hosts("default")
            .await
            .expect("list virtual hosts")
            .is_empty());
    }

    #[tokio::test]
    async fn invitation_registration_and_login_round_trip() {
        let store = AuthStore::in_memory("owner-password").await;
        let admin = store
            .login("admin", "owner-password")
            .await
            .expect("admin login");
        assert!(admin.is_admin);
        let (invite, url) = store
            .create_invitation(DEFAULT_ORGANIZATION_ID, &admin.username)
            .await
            .expect("invitation");
        assert!(url.as_str().contains(&invite));

        let registered = store
            .register(&invite, "alice", "password123")
            .await
            .expect("registration");
        assert_eq!(registered.username, "alice");
        assert!(!registered.is_admin);
        assert!(store.register(&invite, "bob", "password123").await.is_err());

        let login = store
            .login("ALICE", "password123")
            .await
            .expect("case-insensitive login");
        assert_eq!(login.username, "ALICE");
        assert!(!login.is_admin);
    }

    #[tokio::test]
    async fn organization_roles_control_members_and_share_workspaces() {
        let store = AuthStore::in_memory("owner-password").await;
        let owner = store
            .login("admin", "owner-password")
            .await
            .expect("owner login");
        let organization = store
            .create_organization(&owner.username, "Engineering")
            .await
            .expect("create organization");
        store
            .create_workspace(
                &organization.organization_id,
                "ws_engineering",
                "Engineering",
                &owner.username,
            )
            .await
            .expect("create workspace");
        let (alice_invite, _) = store
            .create_invitation(&organization.organization_id, &owner.username)
            .await
            .expect("invite alice");
        let alice = store
            .register(&alice_invite, "alice", "password123")
            .await
            .expect("register alice");

        let workspaces = store
            .list_workspaces(&organization.organization_id, &alice.username)
            .await
            .expect("member workspaces");
        assert_eq!(workspaces[0].workspace_id, "ws_engineering");
        store
            .create_workspace(
                &organization.organization_id,
                "ws_product",
                "Product",
                &alice.username,
            )
            .await
            .expect("members may create workspaces");
        assert!(store
            .create_invitation(&organization.organization_id, &alice.username)
            .await
            .is_err());

        store
            .update_member_role(
                &organization.organization_id,
                &owner.username,
                &alice.username,
                "admin",
            )
            .await
            .expect("promote alice");
        let (bob_invite, _) = store
            .create_invitation(&organization.organization_id, &alice.username)
            .await
            .expect("admin invite");
        store
            .register(&bob_invite, "bob", "password123")
            .await
            .expect("register bob");
        store
            .remove_member(&organization.organization_id, &alice.username, "bob")
            .await
            .expect("admin removes member");
        assert!(store
            .remove_member(
                &organization.organization_id,
                &alice.username,
                &owner.username
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn workspace_access_is_limited_to_organization_members() {
        let store = AuthStore::in_memory("owner-password").await;
        let owner = store
            .login("admin", "owner-password")
            .await
            .expect("owner login");
        let (invite, _) = store
            .create_invitation(DEFAULT_ORGANIZATION_ID, &owner.username)
            .await
            .expect("default organization invite");
        let alice = store
            .register(&invite, "alice", "password123")
            .await
            .expect("register alice");
        let private = store
            .create_organization(&owner.username, "Private")
            .await
            .expect("create private organization");
        store
            .create_workspace(
                &private.organization_id,
                "ws_private",
                "Private",
                &owner.username,
            )
            .await
            .expect("create private workspace");

        let error = store
            .require_workspace_member("ws_private", &alice.username)
            .await
            .expect_err("cross-organization access must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn restarting_does_not_restore_removed_legacy_members() {
        let store = AuthStore::in_memory("owner-password").await;
        let owner = store
            .login("admin", "owner-password")
            .await
            .expect("owner login");
        let (invite, _) = store
            .create_invitation(DEFAULT_ORGANIZATION_ID, &owner.username)
            .await
            .expect("invite alice");
        store
            .register(&invite, "alice", "password123")
            .await
            .expect("register alice");
        store
            .remove_member(DEFAULT_ORGANIZATION_ID, &owner.username, "alice")
            .await
            .expect("remove alice");

        store.migrate().await.expect("repeat migration");

        assert!(store
            .membership_role(DEFAULT_ORGANIZATION_ID, "alice")
            .await
            .expect("membership lookup")
            .is_none());
    }

    #[tokio::test]
    async fn logout_invalidates_the_session() {
        let store = AuthStore::in_memory("owner-password").await;
        let session = store
            .login("admin", "owner-password")
            .await
            .expect("admin login");
        assert!(store
            .session(&session.token)
            .await
            .expect("session lookup")
            .is_some());
        store.logout(&session.token).await.expect("logout");
        assert!(store
            .session(&session.token)
            .await
            .expect("session lookup")
            .is_none());
    }

    #[test]
    fn cookie_parser_handles_multiple_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; treer_session=abc123; other=value"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("abc123")
        );
    }

    #[tokio::test]
    async fn disabled_auth_injects_a_local_administrator() {
        let mut store = AuthStore::in_memory("owner-password").await;
        store.disabled = true;
        let session = authenticate_request(&store, &HeaderMap::new())
            .await
            .expect("local session");
        assert_eq!(session.username, "local");
        assert!(session.is_admin);
    }

    #[tokio::test]
    async fn machine_enrollment_is_single_use_and_binds_identity() {
        let store = AuthStore::in_memory("owner-password").await;
        let enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create enrollment");
        assert_eq!(
            parse_machine_enrollment_key(&enrollment)
                .expect("parse enrollment")
                .workspace_id,
            "workspace-a"
        );
        let claim = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim enrollment");
        assert_eq!(claim.workspace_id, "workspace-a");
        assert!(claim.server_id.starts_with("srv_"));
        assert!(store.claim_machine_enrollment(&enrollment).await.is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
                .expect("authorization header"),
        );
        let machine = store
            .authenticate_machine(&headers)
            .await
            .expect("authenticate machine");
        assert!(machine.allows_server("workspace-a", &claim.server_id));
        assert!(!machine.allows_server("workspace-b", &claim.server_id));
        assert!(!machine.allows_server("workspace-a", "srv_other"));
    }

    #[tokio::test]
    async fn machine_authentication_rejects_missing_credentials() {
        let store = AuthStore::in_memory("owner-password").await;
        assert!(store.authenticate_machine(&HeaderMap::new()).await.is_err());
    }

    #[tokio::test]
    async fn deleting_machine_revokes_credential_and_cleans_names() {
        let store = AuthStore::in_memory("owner-password").await;
        let enrollment = store
            .create_machine_enrollment("workspace-a", "admin")
            .await
            .expect("create enrollment");
        let claim = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim enrollment");
        store
            .set_machine_name("workspace-a", &claim.server_id, "builder")
            .await
            .expect("store machine name");
        store
            .set_agent_name("workspace-a", "agent-a", "reviewer")
            .await
            .expect("store agent name");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
                .expect("authorization header"),
        );

        store
            .delete_machine("workspace-a", &claim.server_id, &["agent-a".to_string()])
            .await
            .expect("delete machine");

        assert!(store.authenticate_machine(&headers).await.is_err());
        let machine_name =
            sqlx::query_scalar::<_, String>("SELECT name FROM machine_names WHERE server_id = ?")
                .bind(&claim.server_id)
                .fetch_optional(&store.pool)
                .await
                .expect("query machine name");
        let agent_name =
            sqlx::query_scalar::<_, String>("SELECT name FROM agent_names WHERE agent_id = ?")
                .bind("agent-a")
                .fetch_optional(&store.pool)
                .await
                .expect("query agent name");
        assert!(machine_name.is_none());
        assert!(agent_name.is_none());
    }

    #[tokio::test]
    async fn persisted_names_are_applied_to_controller_snapshots() {
        let store = AuthStore::in_memory("owner-password").await;
        store
            .set_machine_name("workspace-a", "server-a", "build-machine")
            .await
            .expect("store machine name");
        store
            .set_agent_name("workspace-a", "agent-a", "reviewer")
            .await
            .expect("store agent name");
        let now = Utc::now();
        let mut snapshot = AgentServerSnapshot {
            server: ServerInfo {
                server_id: "server-a".to_string(),
                workspace_id: "workspace-a".to_string(),
                name: String::new(),
                hostname: "original-host".to_string(),
                root: "/workspace".to_string(),
                labels: Default::default(),
                status: ServerStatus::Online,
                connected_at: now,
                last_seen_at: now,
            },
            agents: vec![AgentInfo {
                agent_id: "agent-a".to_string(),
                workspace_id: "workspace-a".to_string(),
                server_id: "server-a".to_string(),
                kind: "codex".to_string(),
                name: "original-agent".to_string(),
                cwd: ".".to_string(),
                status: AgentStatus::Idle,
                pid: None,
                started_at: now,
                updated_at: now,
                exited_at: None,
                exit_code: None,
                output_revision: 0,
            }],
        };

        store
            .apply_server_name(&mut snapshot.server)
            .await
            .expect("apply machine name");
        store
            .apply_agent_names(&mut snapshot)
            .await
            .expect("apply agent names");

        assert_eq!(snapshot.server.name, "build-machine");
        assert_eq!(snapshot.agents[0].name, "reviewer");

        store
            .delete_agent("workspace-a", "agent-a")
            .await
            .expect("persist deletion");
        snapshot.agents[0].name = "original-agent".to_string();
        let deleted = store
            .apply_agent_names(&mut snapshot)
            .await
            .expect("apply deletion");
        assert_eq!(deleted, ["agent-a"]);
        assert!(snapshot.agents.is_empty());
    }

    #[test]
    fn machine_route_is_limited_to_credential_workspace() {
        let machine = MachineSession {
            server_id: Some("srv_test".to_string()),
            workspace_id: Some("team one".to_string()),
        };
        assert!(machine_workspace_matches(
            &machine,
            "/agent/workspaces/team%20one/agents"
        ));
        assert!(!machine_workspace_matches(
            &machine,
            "/agent/workspaces/other/agents"
        ));
    }
}
