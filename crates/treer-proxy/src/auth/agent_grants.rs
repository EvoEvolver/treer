//! Organization-level Agent grants. Organization-scope administration by an
//! Agent needs one of these in addition to an allow rule naming the Agent in
//! its own workspace Policy, so a member who owns a workspace (and therefore
//! its Policy) cannot hand its Agent organization powers. Only human
//! organization owners/admins write grants; a grant authorizes nothing once
//! its creator is no longer an owner/admin.
use super::*;
use treer_protocol::{
    ActorSummary, AgentGrant, AgentGrantList, OrganizationRole, PutAgentGrantRequest,
    ORG_AGENT_GRANTABLE_ACTIONS,
};

fn invalid_grant(message: &str) -> AuthFailure {
    AuthFailure::bad_request("invalid_agent_grant", message)
}

fn grant_from_row(row: sqlx::postgres::PgRow) -> AgentGrant {
    let created_by: String = row.get("created_by");
    let role: String = row.get("created_by_role");
    AgentGrant {
        agent_id: row.get("agent_id"),
        agent_name: row.get("agent_name"),
        workspace_id: row.get("workspace_id"),
        workspace_name: row.get("workspace_name"),
        actions: serde_json::from_value(row.get("actions")).unwrap_or_default(),
        created_by: ActorSummary {
            name: row
                .get::<Option<String>, _>("creator_name")
                .unwrap_or_else(|| created_by.clone()),
            id: created_by,
            kind: "user".into(),
        },
        created_by_role: OrganizationRole::parse(&role).unwrap_or(OrganizationRole::Admin),
        created_at: row
            .get::<Option<i64>, _>("created_ms")
            .and_then(chrono::DateTime::from_timestamp_millis)
            .unwrap_or_default(),
        active: row.get("active"),
    }
}

const GRANT_SELECT: &str = "SELECT g.agent_id, g.workspace_id, g.actions, g.created_by, \
     g.created_by_role, (extract(epoch FROM g.created_at) * 1000)::bigint AS created_ms, \
     (SELECT name FROM agent_names a WHERE a.agent_id = g.agent_id) AS agent_name, \
     w.name AS workspace_name, u.preferred_name AS creator_name, \
     (w.workspace_id IS NOT NULL AND EXISTS(SELECT 1 FROM organization_members m \
        WHERE m.organization_id = g.organization_id AND m.user_id = g.created_by \
        AND m.role IN ('owner', 'admin'))) AS active \
     FROM organization_agent_grants g \
     LEFT JOIN workspaces w ON w.workspace_id = g.workspace_id \
       AND w.organization_id = g.organization_id AND w.deleted_at IS NULL \
     LEFT JOIN users u ON u.id = g.created_by \
     WHERE g.organization_id = $1";

fn validate_agent_id(agent_id: &str) -> Result<(), AuthFailure> {
    if agent_id.is_empty()
        || agent_id.len() > 128
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(invalid_grant("agent_id must be a plain Agent ID"));
    }
    Ok(())
}

fn validate_actions(actions: &[String]) -> Result<Vec<String>, AuthFailure> {
    let mut actions = actions
        .iter()
        .map(|action| action.trim().to_string())
        .collect::<Vec<_>>();
    actions.sort();
    actions.dedup();
    if actions.is_empty() {
        return Err(invalid_grant(
            "actions must not be empty; delete the grant instead",
        ));
    }
    if let Some(action) = actions
        .iter()
        .find(|action| !ORG_AGENT_GRANTABLE_ACTIONS.contains(&action.as_str()))
    {
        return Err(invalid_grant(&format!(
            "{action} cannot be granted to an Agent at organization level"
        )));
    }
    Ok(actions)
}

impl AuthStore {
    pub(crate) async fn list_agent_grants(
        &self,
        organization_id: &str,
        user_id: &str,
    ) -> Result<AgentGrantList, AuthFailure> {
        self.require_manager(organization_id, user_id).await?;
        let rows = sqlx::query(&format!(
            "{GRANT_SELECT} ORDER BY g.created_at, g.agent_id LIMIT 500"
        ))
        .bind(organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(AgentGrantList {
            grants: rows.into_iter().map(grant_from_row).collect(),
            grantable_actions: ORG_AGENT_GRANTABLE_ACTIONS
                .iter()
                .map(|action| action.to_string())
                .collect(),
        })
    }

    /// Create or replace an Agent's organization grant. Every grantable
    /// action is within an admin's own powers once the Agent caps apply
    /// (Agents are treated like admins for owner-affecting checks), so owners
    /// and admins may grant any of them.
    pub(crate) async fn put_agent_grant(
        &self,
        organization_id: &str,
        user_id: &str,
        agent_id: &str,
        request: PutAgentGrantRequest,
    ) -> Result<AgentGrant, AuthFailure> {
        let role = self.require_manager(organization_id, user_id).await?;
        validate_agent_id(agent_id)?;
        let actions = validate_actions(&request.actions)?;
        let workspace_active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE workspace_id = $1 \
             AND organization_id = $2 AND deleted_at IS NULL)",
        )
        .bind(&request.workspace_id)
        .bind(organization_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if !workspace_active {
            return Err(AuthFailure::not_found(
                "workspace_not_found",
                "workspace is not an active workspace of this organization",
            ));
        }
        let named_elsewhere: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agent_names WHERE agent_id = $1 AND workspace_id <> $2)",
        )
        .bind(agent_id)
        .bind(&request.workspace_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        if named_elsewhere {
            return Err(invalid_grant("the Agent belongs to another workspace"));
        }
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let previous: Option<Value> = sqlx::query_scalar(
            "SELECT actions FROM organization_agent_grants \
             WHERE organization_id = $1 AND agent_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let previous: Vec<String> = previous
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO organization_agent_grants(organization_id, agent_id, workspace_id, \
             actions, created_by, created_by_role, created_at) VALUES($1, $2, $3, $4, $5, $6, now()) \
             ON CONFLICT(organization_id, agent_id) DO UPDATE SET workspace_id = EXCLUDED.workspace_id, \
             actions = EXCLUDED.actions, created_by = EXCLUDED.created_by, \
             created_by_role = EXCLUDED.created_by_role, created_at = EXCLUDED.created_at",
        )
        .bind(organization_id)
        .bind(agent_id)
        .bind(&request.workspace_id)
        .bind(json!(actions))
        .bind(user_id)
        .bind(&role)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if !actions.iter().any(|action| action == "org.invite.manage") {
            super::org_admin::revoke_agent_invitations(&mut transaction, organization_id, agent_id)
                .await?;
        }
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.agent_grant.updated",
                resource_kind: "agent",
                resource_id: agent_id,
                resource_name: None,
                payload: json!({
                    "workspace_id": request.workspace_id,
                    "actions": actions,
                    "previous_actions": previous,
                    "created_by_role": role,
                }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(&format!("{GRANT_SELECT} AND g.agent_id = $2"))
            .bind(organization_id)
            .bind(agent_id)
            .fetch_one(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(grant_from_row(row))
    }

    /// Remove an Agent's organization grant and revoke the pending
    /// invitations it created.
    pub(crate) async fn delete_agent_grant(
        &self,
        organization_id: &str,
        user_id: &str,
        agent_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, user_id).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let removed: Option<Value> = sqlx::query_scalar(
            "DELETE FROM organization_agent_grants WHERE organization_id = $1 AND agent_id = $2 \
             RETURNING actions",
        )
        .bind(organization_id)
        .bind(agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let Some(actions) = removed else {
            return Err(AuthFailure::not_found(
                "agent_grant_not_found",
                "this Agent has no organization grant",
            ));
        };
        super::org_admin::revoke_agent_invitations(&mut transaction, organization_id, agent_id)
            .await?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(user_id),
                source: "api",
                action: "organization.agent_grant.removed",
                resource_kind: "agent",
                resource_id: agent_id,
                resource_name: None,
                payload: json!({ "previous_actions": actions }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    /// The organization-grant half of organization-scope Agent
    /// administration: `action` must be in the Agent's active grant.
    pub(crate) async fn require_agent_org_grant(
        &self,
        organization_id: &str,
        workspace_id: &str,
        agent_id: &str,
        action: &str,
    ) -> Result<(), AuthFailure> {
        let granted = super::org_admin::active_agent_grant(
            &self.pool,
            organization_id,
            workspace_id,
            agent_id,
        )
        .await?
        .is_some_and(|actions| actions.iter().any(|granted| granted == action));
        if granted {
            Ok(())
        } else {
            Err(AuthFailure::forbidden(
                "agent_org_grant_required",
                "organization administration by an Agent also needs an organization grant from an owner or admin (Organization settings → Agent permissions)",
            ))
        }
    }
}
