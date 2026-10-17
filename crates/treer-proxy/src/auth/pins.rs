//! Workspace-shared Overview pins (#140). One bounded list per workspace,
//! keyed by stable agent/App/folder IDs, serialised with the other Tree
//! mutations through the workspace row lock. Audit rows are written only when
//! the list actually changes and carry nothing but the kind and the stable ID.
use chrono::{DateTime, Utc};
use sqlx::Row;
use treer_protocol::{WorkspacePin, WorkspacePinKind, WORKSPACE_PINS_LIMIT};

use super::{AuthFailure, AuthStore};
use crate::audit;

/// Pins whose target still exists in the workspace, newest first. Agents
/// count as existing while their credential is unrevoked; Apps and folders
/// while their row exists (their deletion cascades anyway). Read-only.
const LIST_SQL: &str = "SELECT p.agent_id, p.app_id, p.folder_id, p.pinned_by, (EXTRACT(EPOCH FROM p.pinned_at)*1000)::BIGINT AS pinned_at_ms \
    FROM workspace_pins p \
    LEFT JOIN agent_credentials a ON a.agent_id = p.agent_id AND a.workspace_id = p.workspace_id AND a.revoked_at IS NULL \
    LEFT JOIN app_deployments d ON d.app_id = p.app_id AND d.workspace_id = p.workspace_id \
    LEFT JOIN workload_folders f ON f.folder_id = p.folder_id AND f.workspace_id = p.workspace_id \
    WHERE p.workspace_id = $1 AND (a.agent_id IS NOT NULL OR d.app_id IS NOT NULL OR f.folder_id IS NOT NULL) \
    ORDER BY p.pinned_at DESC, p.agent_id, p.app_id, p.folder_id";

fn item_not_found() -> AuthFailure {
    AuthFailure::not_found("pin_item_not_found", "The item is not in this workspace")
}

fn column_for(kind: WorkspacePinKind) -> &'static str {
    match kind {
        WorkspacePinKind::Agent => "agent_id",
        WorkspacePinKind::App => "app_id",
        WorkspacePinKind::Folder => "folder_id",
    }
}

fn pin_from_row(row: &sqlx::postgres::PgRow) -> Result<WorkspacePin, AuthFailure> {
    let agent: Option<String> = row.try_get("agent_id").map_err(AuthFailure::database)?;
    let app: Option<String> = row.try_get("app_id").map_err(AuthFailure::database)?;
    let folder: Option<String> = row.try_get("folder_id").map_err(AuthFailure::database)?;
    let (kind, id) = match (agent, app, folder) {
        (Some(id), None, None) => (WorkspacePinKind::Agent, id),
        (None, Some(id), None) => (WorkspacePinKind::App, id),
        (None, None, Some(id)) => (WorkspacePinKind::Folder, id),
        _ => {
            return Err(AuthFailure::internal(
                "pin_corrupt",
                "pin row does not name exactly one item".to_string(),
            ))
        }
    };
    let millis: i64 = row.try_get("pinned_at_ms").map_err(AuthFailure::database)?;
    let pinned_at = DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| AuthFailure::internal("pin_corrupt", "pin time out of range".to_string()))?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    Ok(WorkspacePin {
        kind,
        id,
        pinned_at,
        pinned_by: row.try_get("pinned_by").map_err(AuthFailure::database)?,
    })
}

impl AuthStore {
    pub(crate) async fn workspace_pins(
        &self,
        workspace: &str,
    ) -> Result<Vec<WorkspacePin>, AuthFailure> {
        sqlx::query(LIST_SQL)
            .bind(workspace)
            .fetch_all(&self.pool)
            .await
            .map_err(AuthFailure::database)?
            .iter()
            .map(pin_from_row)
            .collect()
    }

    /// Pins an item. Returns the pin and whether it was created now (an
    /// existing pin is returned unchanged with `false`: no audit, no event).
    /// Existence is decided before the limit, so a re-pin at the limit still
    /// succeeds; the 51st distinct item is refused.
    pub(crate) async fn pin_workspace_item(
        &self,
        workspace: &str,
        kind: WorkspacePinKind,
        id: &str,
        user_id: &str,
    ) -> Result<(WorkspacePin, bool), AuthFailure> {
        let column = column_for(kind);
        let mut tx = self.lock_workload_tree(workspace).await?;
        let existing = sqlx::query(&format!(
            "SELECT agent_id, app_id, folder_id, pinned_by, (EXTRACT(EPOCH FROM pinned_at)*1000)::BIGINT AS pinned_at_ms FROM workspace_pins WHERE workspace_id=$1 AND {column}=$2"
        ))
        .bind(workspace)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(row) = existing {
            return Ok((pin_from_row(&row)?, false));
        }
        let exists_sql = match kind {
            WorkspacePinKind::Agent => "SELECT EXISTS(SELECT 1 FROM agent_credentials WHERE workspace_id=$1 AND agent_id=$2 AND revoked_at IS NULL)",
            WorkspacePinKind::App => "SELECT EXISTS(SELECT 1 FROM app_deployments WHERE workspace_id=$1 AND app_id=$2)",
            WorkspacePinKind::Folder => "SELECT EXISTS(SELECT 1 FROM workload_folders WHERE workspace_id=$1 AND folder_id=$2)",
        };
        let exists: bool = sqlx::query_scalar(exists_sql)
            .bind(workspace)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        if !exists {
            return Err(item_not_found());
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspace_pins WHERE workspace_id=$1")
                .bind(workspace)
                .fetch_one(&mut *tx)
                .await
                .map_err(AuthFailure::database)?;
        if count >= WORKSPACE_PINS_LIMIT as i64 {
            return Err(AuthFailure::conflict(
                "pins_limit_reached",
                "This workspace already has the maximum number of pinned items",
            ));
        }
        let row = sqlx::query(&format!(
            "INSERT INTO workspace_pins(workspace_id, {column}, pinned_by) VALUES($1,$2,$3) RETURNING agent_id, app_id, folder_id, pinned_by, (EXTRACT(EPOCH FROM pinned_at)*1000)::BIGINT AS pinned_at_ms"
        ))
        .bind(workspace)
        .bind(id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        let pin = pin_from_row(&row)?;
        audit::insert_workspace_mutation(&mut tx, workspace, "tree.pin.create", kind.as_str(), id)
            .await
            .map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok((pin, true))
    }

    /// Unpins an item; `Ok(false)` when nothing was pinned (no audit).
    pub(crate) async fn unpin_workspace_item(
        &self,
        workspace: &str,
        kind: WorkspacePinKind,
        id: &str,
    ) -> Result<bool, AuthFailure> {
        let column = column_for(kind);
        let mut tx = self.lock_workload_tree(workspace).await?;
        let removed = sqlx::query(&format!(
            "DELETE FROM workspace_pins WHERE workspace_id=$1 AND {column}=$2"
        ))
        .bind(workspace)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AuthFailure::database)?
        .rows_affected()
            > 0;
        if removed {
            audit::insert_workspace_mutation(
                &mut tx,
                workspace,
                "tree.pin.delete",
                kind.as_str(),
                id,
            )
            .await
            .map_err(AuthFailure::database)?;
        }
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(removed)
    }
}
