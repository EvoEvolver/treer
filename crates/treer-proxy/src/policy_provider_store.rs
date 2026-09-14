use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use treer_protocol::{PolicyProviderFailureMode, WorkspacePolicyProvider};

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
                    revision_hint, updated_at, updated_by \
             FROM workspace_policy_providers WHERE workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?
        .map(provider_from_row)
        .transpose()
        .map_err(sqlx::Error::Decode)
    }

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
                       revision_hint, updated_at, updated_by",
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;

    #[tokio::test]
    async fn provider_binding_and_revision_hint_are_durable_and_monotonic() {
        let auth = AuthStore::for_test("owner-password").await;
        let pool = auth.pool();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO organizations(organization_id, name, created_at, created_by) \
             VALUES('org_policy', 'Policy', $1, 'owner')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert organization");
        sqlx::query(
            "INSERT INTO workspaces(workspace_id, organization_id, name, created_at, created_by) \
             VALUES('ws_policy', 'org_policy', 'Policy', $1, 'owner')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert workspace");
        sqlx::query(
            "INSERT INTO machine_services(service_id, workspace_id, name, server_id, target_host, \
                target_port, protocol, created_at, created_by, updated_at, updated_by) \
             VALUES('svc_policy', 'ws_policy', 'policy', 'machine', '127.0.0.1', 8787, 'http', \
                $1, 'owner', $1, 'owner')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert service");
        sqlx::query(
            "INSERT INTO app_deployments(app_id, workspace_id, name, server_id, command, args, cwd, \
                port, hostname, service_id, created_at, created_by, updated_at, updated_by) \
             VALUES('app_policy', 'ws_policy', 'policy', 'machine', 'python3', '[]'::jsonb, '.', \
                8787, 'policy.internal', 'svc_policy', $1, 'owner', $1, 'owner')",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert app");

        let store = PolicyProviderStore::new(pool);
        let configured = store
            .set(
                "ws_policy",
                "app_policy",
                "svc_policy",
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
                .advance_revision("ws_policy", "app_policy", 9)
                .await
                .expect("advance revision"),
            Some(9)
        );
        assert_eq!(
            store
                .advance_revision("ws_policy", "app_policy", 4)
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
