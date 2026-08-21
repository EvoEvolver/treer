use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use sqlx::Row;
use treer_protocol::{
    MessagePrincipal, MessagePrincipalKind, PluginHumanSession, PluginOAuthExchangeResponse,
};
use uuid::Uuid;

use crate::auth::AppOAuthGrant;

const PLUGIN_OAUTH_STATE_TTL_MINUTES: i64 = 10;
const PLUGIN_HUMAN_SESSION_TTL_HOURS: i64 = 12;

#[derive(Clone)]
pub struct PluginSessionStore {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct ConsumedPluginOAuthState {
    pub workspace_id: String,
    pub plugin_id: String,
    pub service_id: String,
    pub bridge_agent_id: String,
    pub redirect_uri: String,
    pub verifier: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginStoreError {
    #[error("{message}")]
    Contract { code: &'static str, message: String },
    #[error("plugin session storage operation failed")]
    Database(#[source] sqlx::Error),
    #[error("stored plugin session data is invalid")]
    Corrupt,
}

impl PluginStoreError {
    fn contract(code: &'static str, message: impl Into<String>) -> Self {
        Self::Contract {
            code,
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for PluginStoreError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}

impl PluginSessionStore {
    pub async fn open(pool: PgPool) -> Result<Self, PluginStoreError> {
        let store = Self { pool };
        for statement in SCHEMA {
            sqlx::query(statement).execute(&store.pool).await?;
        }
        Ok(store)
    }

    pub async fn create_oauth_state(
        &self,
        workspace_id: &str,
        plugin_id: &str,
        service_id: &str,
        bridge_agent_id: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<(String, DateTime<Utc>), PluginStoreError> {
        validate_plugin_id(plugin_id)?;
        if workspace_id.is_empty()
            || service_id.is_empty()
            || bridge_agent_id.is_empty()
            || redirect_uri.is_empty()
            || redirect_uri.len() > 4_096
            || verifier.len() < 43
            || verifier.len() > 128
        {
            return Err(PluginStoreError::contract(
                "plugin_oauth_request_invalid",
                "plugin OAuth request fields are missing or exceed their bounds",
            ));
        }
        let state = format!("pos_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::minutes(PLUGIN_OAUTH_STATE_TTL_MINUTES);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM core_plugin_oauth_states WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO core_plugin_oauth_states(\
                state_hash, workspace_id, plugin_id, service_id, bridge_agent_id, redirect_uri, \
                verifier, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(secret_hash(&state))
        .bind(workspace_id)
        .bind(plugin_id)
        .bind(service_id)
        .bind(bridge_agent_id)
        .bind(redirect_uri)
        .bind(verifier)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok((state, expires_at))
    }

    pub async fn consume_oauth_state(
        &self,
        state: &str,
        workspace_id: &str,
        plugin_id: &str,
        service_id: &str,
        bridge_agent_id: &str,
    ) -> Result<ConsumedPluginOAuthState, PluginStoreError> {
        let row = sqlx::query(
            "DELETE FROM core_plugin_oauth_states WHERE state_hash = $1 AND workspace_id = $2 \
             AND plugin_id = $3 AND service_id = $4 AND bridge_agent_id = $5 AND expires_at > $6 \
             RETURNING workspace_id, plugin_id, service_id, bridge_agent_id, redirect_uri, verifier",
        )
        .bind(secret_hash(state))
        .bind(workspace_id)
        .bind(plugin_id)
        .bind(service_id)
        .bind(bridge_agent_id)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            PluginStoreError::contract(
                "plugin_oauth_state_invalid",
                "plugin OAuth state is invalid, expired, or already used",
            )
        })?;
        Ok(ConsumedPluginOAuthState {
            workspace_id: row.get("workspace_id"),
            plugin_id: row.get("plugin_id"),
            service_id: row.get("service_id"),
            bridge_agent_id: row.get("bridge_agent_id"),
            redirect_uri: row.get("redirect_uri"),
            verifier: row.get("verifier"),
        })
    }

    pub async fn create_human_session(
        &self,
        plugin_id: &str,
        bridge_agent_id: &str,
        grant: &AppOAuthGrant,
    ) -> Result<PluginOAuthExchangeResponse, PluginStoreError> {
        validate_plugin_id(plugin_id)?;
        validate_bridge_agent_id(bridge_agent_id)?;
        let session_id = format!("phs_{}", Uuid::new_v4().simple());
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let capability = format!("{session_id}.{secret}");
        let now = Utc::now();
        let expires_at = now + Duration::hours(PLUGIN_HUMAN_SESSION_TTL_HOURS);
        sqlx::query(
            "INSERT INTO core_plugin_human_sessions(\
                session_id, token_hash, workspace_id, plugin_id, service_id, bridge_agent_id, \
                user_id, preferred_name, role, created_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&session_id)
        .bind(secret_hash(&capability))
        .bind(&grant.workspace_id)
        .bind(plugin_id)
        .bind(&grant.service_id)
        .bind(bridge_agent_id)
        .bind(&grant.user_id)
        .bind(&grant.preferred_name)
        .bind(&grant.role)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(PluginOAuthExchangeResponse {
            session_capability: capability,
            session: PluginHumanSession {
                plugin_id: plugin_id.to_string(),
                workspace_id: grant.workspace_id.clone(),
                service_id: grant.service_id.clone(),
                principal: MessagePrincipal {
                    kind: MessagePrincipalKind::Human,
                    id: grant.user_id.clone(),
                    name: grant.preferred_name.clone(),
                    role: Some(grant.role.clone()),
                },
                expires_at,
            },
        })
    }

    pub async fn authenticate(
        &self,
        workspace_id: &str,
        plugin_id: &str,
        bridge_agent_id: &str,
        capability: &str,
    ) -> Result<PluginHumanSession, PluginStoreError> {
        validate_plugin_id(plugin_id)?;
        validate_bridge_agent_id(bridge_agent_id)?;
        let (session_id, secret) = capability.split_once('.').ok_or_else(|| {
            PluginStoreError::contract(
                "plugin_session_invalid",
                "plugin human session is invalid or expired",
            )
        })?;
        if !session_id.starts_with("phs_") || secret.len() != 64 {
            return Err(PluginStoreError::contract(
                "plugin_session_invalid",
                "plugin human session is invalid or expired",
            ));
        }
        let row = sqlx::query(
            "SELECT workspace_id, plugin_id, service_id, user_id, preferred_name, role, expires_at \
             FROM core_plugin_human_sessions WHERE session_id = $1 AND token_hash = $2 \
             AND workspace_id = $3 AND plugin_id = $4 AND bridge_agent_id = $5 \
             AND revoked_at IS NULL AND expires_at > $6",
        )
        .bind(session_id)
        .bind(secret_hash(capability))
        .bind(workspace_id)
        .bind(plugin_id)
        .bind(bridge_agent_id)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            PluginStoreError::contract(
                "plugin_session_invalid",
                "plugin human session is invalid or expired",
            )
        })?;
        Ok(PluginHumanSession {
            plugin_id: row.get("plugin_id"),
            workspace_id: row.get("workspace_id"),
            service_id: row.get("service_id"),
            principal: MessagePrincipal {
                kind: MessagePrincipalKind::Human,
                id: row.get("user_id"),
                name: row.get("preferred_name"),
                role: Some(row.get("role")),
            },
            expires_at: parse_timestamp(row.get("expires_at"))?,
        })
    }

    pub async fn revoke(
        &self,
        workspace_id: &str,
        plugin_id: &str,
        bridge_agent_id: &str,
        capability: &str,
    ) -> Result<bool, PluginStoreError> {
        validate_plugin_id(plugin_id)?;
        validate_bridge_agent_id(bridge_agent_id)?;
        let session_id = capability
            .split_once('.')
            .map(|(id, _)| id)
            .ok_or_else(|| {
                PluginStoreError::contract(
                    "plugin_session_invalid",
                    "plugin human session is invalid",
                )
            })?;
        let result = sqlx::query(
            "UPDATE core_plugin_human_sessions SET revoked_at = $1 WHERE session_id = $2 \
             AND token_hash = $3 AND workspace_id = $4 AND plugin_id = $5 \
             AND bridge_agent_id = $6 AND revoked_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(session_id)
        .bind(secret_hash(capability))
        .bind(workspace_id)
        .bind(plugin_id)
        .bind(bridge_agent_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn revoke_plugin(
        &self,
        workspace_id: &str,
        plugin_id: &str,
        bridge_agent_id: &str,
    ) -> Result<u64, PluginStoreError> {
        validate_plugin_id(plugin_id)?;
        validate_bridge_agent_id(bridge_agent_id)?;
        let result = sqlx::query(
            "UPDATE core_plugin_human_sessions SET revoked_at = $1 WHERE workspace_id = $2 \
             AND plugin_id = $3 AND bridge_agent_id = $4 AND revoked_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(workspace_id)
        .bind(plugin_id)
        .bind(bridge_agent_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

fn validate_bridge_agent_id(agent_id: &str) -> Result<(), PluginStoreError> {
    if agent_id.is_empty() || agent_id.len() > 256 || agent_id.chars().any(char::is_control) {
        Err(PluginStoreError::contract(
            "plugin_bridge_agent_invalid",
            "plugin bridge Agent ID is invalid",
        ))
    } else {
        Ok(())
    }
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), PluginStoreError> {
    let valid = !plugin_id.is_empty()
        && plugin_id.len() <= 63
        && plugin_id.as_bytes()[0].is_ascii_lowercase()
        && plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(PluginStoreError::contract(
            "plugin_id_invalid",
            "plugin ID is invalid",
        ))
    }
}

fn secret_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn parse_timestamp(value: String) -> Result<DateTime<Utc>, PluginStoreError> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| PluginStoreError::Corrupt)
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS core_plugin_oauth_states (\
        state_hash TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, plugin_id TEXT NOT NULL, \
        service_id TEXT NOT NULL, bridge_agent_id TEXT NOT NULL, redirect_uri TEXT NOT NULL, \
        verifier TEXT NOT NULL, created_at TEXT NOT NULL, expires_at TEXT NOT NULL)",
    "CREATE INDEX IF NOT EXISTS core_plugin_oauth_states_expiry \
        ON core_plugin_oauth_states(expires_at)",
    "CREATE TABLE IF NOT EXISTS core_plugin_human_sessions (\
        session_id TEXT PRIMARY KEY, token_hash TEXT UNIQUE NOT NULL, workspace_id TEXT NOT NULL, \
        plugin_id TEXT NOT NULL, service_id TEXT NOT NULL, bridge_agent_id TEXT NOT NULL, \
        user_id TEXT NOT NULL, preferred_name TEXT NOT NULL, role TEXT NOT NULL, \
        created_at TEXT NOT NULL, expires_at TEXT NOT NULL, revoked_at TEXT)",
    "ALTER TABLE core_plugin_human_sessions ADD COLUMN IF NOT EXISTS bridge_agent_id \
        TEXT NOT NULL DEFAULT ''",
    "CREATE INDEX IF NOT EXISTS core_plugin_human_sessions_lookup \
        ON core_plugin_human_sessions(workspace_id, plugin_id, user_id, expires_at) \
        WHERE revoked_at IS NULL",
];

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    async fn store() -> PluginSessionStore {
        let database_url = std::env::var("TREER_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
        let schema = format!("plugin_test_{}", Uuid::new_v4().simple());
        let setup = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect to test PostgreSQL");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&setup)
            .await
            .expect("create test schema");
        setup.close().await;
        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect to test schema");
        PluginSessionStore::open(pool).await.expect("plugin store")
    }

    #[tokio::test]
    async fn oauth_state_is_single_use_and_human_session_is_plugin_bound() {
        let store = store().await;
        let verifier = "v".repeat(64);
        let (state, _) = store
            .create_oauth_state(
                "workspace-a",
                "mail",
                "svc-mail",
                "agent-bridge",
                "https://mail.example/api/auth/callback",
                &verifier,
            )
            .await
            .expect("create state");
        let consumed = store
            .consume_oauth_state(&state, "workspace-a", "mail", "svc-mail", "agent-bridge")
            .await
            .expect("consume state");
        assert_eq!(consumed.verifier, verifier);
        assert!(store
            .consume_oauth_state(&state, "workspace-a", "mail", "svc-mail", "agent-bridge",)
            .await
            .is_err());

        let response = store
            .create_human_session(
                "mail",
                "agent-bridge",
                &AppOAuthGrant {
                    workspace_id: "workspace-a".to_string(),
                    service_id: "svc-mail".to_string(),
                    user_id: "user-a".to_string(),
                    preferred_name: "Owner".to_string(),
                    role: "owner".to_string(),
                },
            )
            .await
            .expect("create human session");
        assert!(store
            .authenticate(
                "workspace-a",
                "telegram",
                "agent-bridge",
                &response.session_capability,
            )
            .await
            .is_err());
        assert!(store
            .authenticate(
                "workspace-a",
                "mail",
                "another-agent",
                &response.session_capability,
            )
            .await
            .is_err());
        assert_eq!(
            store
                .authenticate(
                    "workspace-a",
                    "mail",
                    "agent-bridge",
                    &response.session_capability,
                )
                .await
                .expect("authenticate session")
                .principal
                .id,
            "user-a"
        );
        assert!(store
            .revoke(
                "workspace-a",
                "mail",
                "agent-bridge",
                &response.session_capability,
            )
            .await
            .expect("revoke"));
        assert!(store
            .authenticate(
                "workspace-a",
                "mail",
                "agent-bridge",
                &response.session_capability,
            )
            .await
            .is_err());
    }
}
