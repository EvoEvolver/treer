use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use treer_protocol::OrganizationAuditEvent;
use uuid::Uuid;

pub(crate) struct NewAuditEvent<'a> {
    pub organization_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub actor_kind: &'a str,
    pub actor_id: Option<&'a str>,
    pub source: &'a str,
    pub action: &'a str,
    pub resource_kind: &'a str,
    pub resource_id: &'a str,
    pub resource_name: Option<&'a str>,
    pub payload: Value,
}

pub(crate) struct NewWorkspaceAuditEvent<'a> {
    pub workspace_id: &'a str,
    pub actor_kind: &'a str,
    pub actor_id: Option<&'a str>,
    pub action: &'a str,
    pub resource_kind: &'a str,
    pub resource_id: &'a str,
    pub resource_name: Option<&'a str>,
    pub payload: Value,
}

pub(crate) async fn insert(
    transaction: &mut Transaction<'_, Postgres>,
    event: NewAuditEvent<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO organization_audit_events(\
         event_id, organization_id, workspace_id, occurred_at, actor_kind, actor_id, \
         actor_name, source, action, outcome, resource_kind, resource_id, resource_name, payload) \
         VALUES($1, $2, $3, $4, $5, $6, \
         (SELECT preferred_name FROM users WHERE id = $6), $7, $8, 'succeeded', $9, $10, $11, $12)",
    )
    .bind(format!("aud_{}", Uuid::new_v4().simple()))
    .bind(event.organization_id)
    .bind(event.workspace_id)
    .bind(Utc::now().to_rfc3339())
    .bind(event.actor_kind)
    .bind(event.actor_id)
    .bind(event.source)
    .bind(event.action)
    .bind(event.resource_kind)
    .bind(event.resource_id)
    .bind(event.resource_name)
    .bind(event.payload)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) async fn list(
    pool: &PgPool,
    organization_id: &str,
    workspace_id: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<OrganizationAuditEvent>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT sequence, event_id, organization_id, workspace_id, occurred_at, actor_kind, \
         actor_id, actor_name, source, action, outcome, resource_kind, resource_id, \
         resource_name, correlation_id, payload \
         FROM organization_audit_events \
         WHERE organization_id = $1 AND ($2::TEXT IS NULL OR workspace_id = $2 OR workspace_id IS NULL) \
         AND ($3::BIGINT IS NULL OR sequence < $3) ORDER BY sequence DESC LIMIT $4",
    )
    .bind(organization_id)
    .bind(workspace_id)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let occurred_at = row
                .get::<String, _>("occurred_at")
                .parse::<DateTime<Utc>>()
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
            Ok(OrganizationAuditEvent {
                sequence: row.get("sequence"),
                event_id: row.get("event_id"),
                organization_id: row.get("organization_id"),
                workspace_id: row.get("workspace_id"),
                occurred_at,
                actor_kind: row.get("actor_kind"),
                actor_id: row.get("actor_id"),
                actor_name: row.get("actor_name"),
                source: row.get("source"),
                action: row.get("action"),
                outcome: row.get("outcome"),
                resource_kind: row.get("resource_kind"),
                resource_id: row.get("resource_id"),
                resource_name: row.get("resource_name"),
                correlation_id: row.get("correlation_id"),
                payload: row.get("payload"),
            })
        })
        .collect()
}

pub(crate) async fn record_workspace(
    pool: &PgPool,
    event: NewWorkspaceAuditEvent<'_>,
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let organization_id = sqlx::query_scalar::<_, String>(
        "SELECT organization_id FROM workspaces WHERE workspace_id = $1",
    )
    .bind(event.workspace_id)
    .fetch_one(&mut *transaction)
    .await?;
    insert(
        &mut transaction,
        NewAuditEvent {
            organization_id: &organization_id,
            workspace_id: Some(event.workspace_id),
            actor_kind: event.actor_kind,
            actor_id: event.actor_id,
            source: "runtime",
            action: event.action,
            resource_kind: event.resource_kind,
            resource_id: event.resource_id,
            resource_name: event.resource_name,
            payload: event.payload,
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}
