use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use treer_protocol::{PolicyProviderFailureMode, PolicyPublishAuthority, WorkspacePolicyProvider};

#[derive(Clone)]
pub struct PolicyProviderStore {
    pool: PgPool,
}

impl PolicyProviderStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspacePolicyProvider>, sqlx::Error> {
        sqlx::query(
            "SELECT workspace_id, app_id, service_id, failure_mode, max_stale_seconds, \
                    revision_hint, updated_at, updated_by, publish_authority \
             FROM workspace_policy_providers WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?
        .map(provider_from_row)
        .transpose()
        .map_err(sqlx::Error::Decode)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn set(
        &self,
        workspace_id: &str,
        app_id: &str,
        service_id: &str,
        failure_mode: PolicyProviderFailureMode,
        max_stale_seconds: u64,
        revision_hint: u64,
        actor: &str,
    ) -> Result<WorkspacePolicyProvider, sqlx::Error> {
        let max_stale_seconds = i64::try_from(max_stale_seconds)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let revision_hint =
            i64::try_from(revision_hint).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let row = sqlx::query(
            "INSERT INTO workspace_policy_providers(\
                workspace_id, app_id, service_id, failure_mode, max_stale_seconds, revision_hint, \
                updated_at, updated_by\
             ) VALUES($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT(workspace_id) DO UPDATE SET \
                app_id = excluded.app_id, service_id = excluded.service_id, \
                failure_mode = excluded.failure_mode, max_stale_seconds = excluded.max_stale_seconds, \
                revision_hint = CASE \
                    WHEN workspace_policy_providers.app_id = excluded.app_id \
                    THEN GREATEST(workspace_policy_providers.revision_hint, excluded.revision_hint) \
                    ELSE excluded.revision_hint END, \
                updated_at = excluded.updated_at, updated_by = excluded.updated_by \
             RETURNING workspace_id, app_id, service_id, failure_mode, max_stale_seconds, \
                       revision_hint, updated_at, updated_by, publish_authority",
        )
        .bind(workspace_id)
        .bind(app_id)
        .bind(service_id)
        .bind(failure_mode.as_str())
        .bind(max_stale_seconds)
        .bind(revision_hint)
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .fetch_one(&self.pool)
        .await?;
        provider_from_row(row).map_err(sqlx::Error::Decode)
    }

    pub async fn clear(&self, workspace_id: &str) -> Result<bool, sqlx::Error> {
        Ok(
            sqlx::query("DELETE FROM workspace_policy_providers WHERE workspace_id = $1")
                .bind(workspace_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// The caller must authenticate a current workspace owner first. Persist the
    /// choice and its audit atomically, fencing against a changed Provider.
    pub async fn set_publish_authority(
        &self,
        workspace: &str,
        expected_app: &str,
        authority: PolicyPublishAuthority,
        actor: &str,
    ) -> Result<Option<WorkspacePolicyProvider>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT publish_authority FROM workspace_policy_providers \
             WHERE workspace_id=$1 AND app_id=$2 FOR UPDATE",
        )
        .bind(workspace)
        .bind(expected_app)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(previous) = previous else {
            return Ok(None);
        };
        let row = sqlx::query(
            "UPDATE workspace_policy_providers SET publish_authority=$3, updated_at=$4, updated_by=$5 \
             WHERE workspace_id=$1 AND app_id=$2 RETURNING workspace_id, app_id, service_id, \
             failure_mode, max_stale_seconds, revision_hint, updated_at, updated_by, publish_authority",
        )
        .bind(workspace)
        .bind(expected_app)
        .bind(authority.as_str())
        .bind(Utc::now().to_rfc3339())
        .bind(actor)
        .fetch_one(&mut *tx)
        .await?;
        let organization: String =
            sqlx::query_scalar("SELECT organization_id FROM workspaces WHERE workspace_id=$1")
                .bind(workspace)
                .fetch_one(&mut *tx)
                .await?;
        crate::audit::insert(
            &mut tx,
            crate::audit::NewAuditEvent {
                organization_id: &organization,
                workspace_id: Some(workspace),
                actor_kind: "user",
                actor_id: Some(actor),
                source: "proxy",
                action: "policy_provider.publish_authority.set",
                resource_kind: "policy_provider",
                resource_id: expected_app,
                resource_name: None,
                payload: serde_json::json!({"previous":previous,"publish_authority":authority}),
            },
        )
        .await?;
        let provider = provider_from_row(row).map_err(sqlx::Error::Decode)?;
        tx.commit().await?;
        Ok(Some(provider))
    }

    pub async fn advance_revision(
        &self,
        workspace_id: &str,
        app_id: &str,
        revision: u64,
    ) -> Result<Option<u64>, sqlx::Error> {
        let revision =
            i64::try_from(revision).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        let value = sqlx::query_scalar::<_, i64>(
            "UPDATE workspace_policy_providers \
             SET revision_hint = GREATEST(revision_hint, $3) \
             WHERE workspace_id = $1 AND app_id = $2 RETURNING revision_hint",
        )
        .bind(workspace_id)
        .bind(app_id)
        .bind(revision)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| u64::try_from(value).map_err(|error| sqlx::Error::Decode(Box::new(error))))
            .transpose()
    }
}

fn provider_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<WorkspacePolicyProvider, Box<dyn std::error::Error + Send + Sync>> {
    let failure_mode = match row.get::<String, _>("failure_mode").as_str() {
        "fail_closed" => PolicyProviderFailureMode::FailClosed,
        "fail_open" => PolicyProviderFailureMode::FailOpen,
        value => return Err(format!("invalid policy provider failure mode {value}").into()),
    };
    let updated_at =
        DateTime::parse_from_rfc3339(&row.get::<String, _>("updated_at"))?.with_timezone(&Utc);
    Ok(WorkspacePolicyProvider {
        workspace_id: row.get("workspace_id"),
        app_id: row.get("app_id"),
        service_id: row.get("service_id"),
        failure_mode,
        max_stale_seconds: u64::try_from(row.get::<i64, _>("max_stale_seconds"))?,
        revision_hint: u64::try_from(row.get::<i64, _>("revision_hint"))?,
        updated_at,
        updated_by: row.get("updated_by"),
        publish_authority: match row.get::<String, _>("publish_authority").as_str() {
            "owner_session" => PolicyPublishAuthority::OwnerSession,
            "provider_operator" => PolicyPublishAuthority::ProviderOperator,
            value => return Err(format!("invalid policy publication authority {value}").into()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;

    #[tokio::test]
    async fn provider_binding_and_revision_hint_are_durable_and_monotonic() {
        let auth = AuthStore::for_test("owner-password").await;
        auth.seed_test_workspace("ws_policy").await;
        let app = auth
            .create_app_deployment(
                "ws_policy",
                "owner",
                "machine".into(),
                treer_protocol::CreateAppDeploymentRequest {
                    folder_id: None,
                    server_id: Some("machine".into()),
                    name: "policy".into(),
                    command: "python3".into(),
                    args: vec![],
                    cwd: ".".into(),
                    port: 8787,
                    hostname: String::new(),
                    public: false,
                    ui_capabilities: Vec::new(),
                },
            )
            .await
            .unwrap();
        let (app_id, service_id) = (app.app_id.as_str(), app.service_id.as_str());
        let store = PolicyProviderStore::new(auth.pool());
        let configured = store
            .set(
                "ws_policy",
                app_id,
                service_id,
                PolicyProviderFailureMode::FailClosed,
                60,
                0,
                "owner",
            )
            .await
            .expect("configure provider");
        assert_eq!(configured.revision_hint, 0);
        assert_eq!(
            store
                .advance_revision("ws_policy", app_id, 9)
                .await
                .expect("advance revision"),
            Some(9)
        );
        assert_eq!(
            store
                .advance_revision("ws_policy", app_id, 4)
                .await
                .expect("reject rollback"),
            Some(9)
        );
        let loaded = store
            .get("ws_policy")
            .await
            .expect("load provider")
            .expect("provider exists");
        assert_eq!(loaded.revision_hint, 9);
        assert_eq!(loaded.max_stale_seconds, 60);
        assert!(store.clear("ws_policy").await.expect("clear provider"));
        assert!(store
            .get("ws_policy")
            .await
            .expect("load cleared")
            .is_none());
    }
}
