use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use treer_protocol::{OrganizationAuditEvent, WorkspaceAuditEventsPage, WorkspaceAuditEventsQuery};
use uuid::Uuid;

pub(crate) mod maintenance;

#[cfg(test)]
mod mutation_tests;

/// Set only after authentication, never from an actor supplied in a JSON body.
/// Scoped to the request future (not an OS thread or pooled DB connection).
#[derive(Clone)]
pub(crate) struct MutationActor {
    pub kind: &'static str,
    pub id: Option<String>,
}

tokio::task_local! {
    pub(crate) static MUTATION_ACTOR: MutationActor;
}

pub(crate) async fn insert_workspace_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    workspace: &str,
    action: &str,
    kind: &str,
    id: &str,
) -> Result<(), sqlx::Error> {
    let actor = MUTATION_ACTOR
        .try_with(Clone::clone)
        .unwrap_or(MutationActor {
            kind: "system",
            id: None,
        });
    let organization: String =
        sqlx::query_scalar("SELECT organization_id FROM workspaces WHERE workspace_id=$1")
            .bind(workspace)
            .fetch_one(&mut **transaction)
            .await?;
    insert(
        transaction,
        NewAuditEvent {
            organization_id: &organization,
            workspace_id: Some(workspace),
            actor_kind: actor.kind,
            actor_id: actor.id.as_deref(),
            source: "control-plane",
            action,
            resource_kind: kind,
            resource_id: id,
            resource_name: None,
            payload: serde_json::json!({}),
        },
    )
    .await
}

/// Network mutations span durable rows and in-memory routing/Controller state.
/// Do not report rollback if this follow-up write fails after the mutation.
pub(crate) async fn mutation_best_effort(
    pool: &PgPool,
    workspace: &str,
    action: &str,
    kind: &str,
    id: &str,
) {
    let result = async {
        let mut tx = pool.begin().await?;
        insert_workspace_mutation(&mut tx, workspace, action, kind, id).await?;
        tx.commit().await
    }
    .await;
    if result.is_err() {
        // Database errors may contain row values. Never log them here.
        tracing::warn!(action, "mutation succeeded but audit persistence failed");
    }
}

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
         actor_name, source, action, outcome, resource_kind, resource_id, resource_name, payload, workspace_origin_id) \
         VALUES($1, $2, $3, $4, $5, $6, \
         (SELECT preferred_name FROM users WHERE id = $6), $7, $8, 'succeeded', $9, $10, $11, $12, $3)",
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
        "SELECT sequence, event_id, organization_id, COALESCE(workspace_id,workspace_origin_id) AS workspace_id, occurred_at, actor_kind, \
         actor_id, actor_name, source, action, outcome, resource_kind, resource_id, \
         resource_name, correlation_id, payload \
         FROM organization_audit_events \
         WHERE organization_id = $1 AND ($2::TEXT IS NULL OR COALESCE(workspace_id,workspace_origin_id) = $2 OR COALESCE(workspace_id,workspace_origin_id) IS NULL) \
         AND ($3::BIGINT IS NULL OR sequence < $3) ORDER BY sequence DESC LIMIT $4",
    )
    .bind(organization_id)
    .bind(workspace_id)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    decode_events(rows)
}

fn decode_events(
    rows: Vec<sqlx::postgres::PgRow>,
) -> Result<Vec<OrganizationAuditEvent>, sqlx::Error> {
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

/// Deliberately separate from the organization view: NULL workspace events
/// belong to the organization, never to an arbitrary workspace.
pub(crate) async fn list_workspace(
    pool: &PgPool,
    workspace_id: &str,
    query: &WorkspaceAuditEventsQuery,
) -> Result<WorkspaceAuditEventsPage, sqlx::Error> {
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 100));
    let rows = sqlx::query(
        "SELECT e.sequence, left(e.event_id,128) AS event_id, \
         left(e.organization_id,128) AS organization_id, left(e.workspace_id,128) AS workspace_id, e.occurred_at, \
         left(e.actor_kind,32) AS actor_kind, left(e.actor_id,128) AS actor_id, NULL::TEXT AS actor_name, \
         left(e.source,32) AS source, left(e.action,128) AS action, left(e.outcome,32) AS outcome, \
         left(e.resource_kind,64) AS resource_kind, left(e.resource_id,256) AS resource_id, \
         NULL::TEXT AS resource_name, NULL::TEXT AS correlation_id, \
         COALESCE((SELECT jsonb_object_agg(k, e.payload->k) \
           FROM unnest(ARRAY['server_id','startup_revision','exit_code','timed_out','truncated','timeout_ms','rate_limit_per_minute','rate_burst','concurrency_limit','ip_rate_limit_per_minute','ip_rate_burst','max_body_bytes','response_timeout_secs','body_timeout_secs']) AS k \
           WHERE (jsonb_typeof(e.payload->k) IN ('number','boolean','null') AND octet_length((e.payload->k)::TEXT) <= 64) \
           OR (k = 'server_id' AND jsonb_typeof(e.payload->k) = 'string' AND octet_length((e.payload->k)::TEXT) <= 130)), \
           '{}'::JSONB) AS payload \
         FROM organization_audit_events e JOIN workspaces w \
         ON w.workspace_id = e.workspace_id AND w.organization_id = e.organization_id \
         WHERE w.workspace_id = $1 AND w.deleted_at IS NULL \
         AND ($2::BIGINT IS NULL OR e.sequence < $2) \
         AND ($3::TEXT IS NULL OR e.occurred_at::TIMESTAMPTZ >= $3::TIMESTAMPTZ) \
         AND ($4::TEXT IS NULL OR e.occurred_at::TIMESTAMPTZ < $4::TIMESTAMPTZ) \
         AND ($5::TEXT IS NULL OR e.actor_id = $5) \
         AND ($6::TEXT IS NULL OR e.action = $6) \
         AND ($7::TEXT IS NULL OR e.resource_kind = $7) \
         ORDER BY e.sequence DESC LIMIT $8",
    )
    .bind(workspace_id)
    .bind(query.before)
    .bind(query.since.map(|v| v.to_rfc3339()))
    .bind(query.until.map(|v| v.to_rfc3339()))
    .bind(&query.actor_id)
    .bind(&query.action)
    .bind(&query.resource_kind)
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;
    let mut events = decode_events(rows)?;
    let has_more = events.len() > limit as usize;
    events.truncate(limit as usize);
    let next_cursor = has_more.then(|| events.last().expect("positive page limit").sequence);
    for event in &mut events {
        // Stored display names and arbitrary payloads are not trusted schema.
        // The org endpoint intentionally retains its existing projection.
        event.actor_name = None;
        event.resource_name = None;
        event.correlation_id = None;
        event.payload = workspace_payload(&event.action, &event.payload);
    }
    Ok(WorkspaceAuditEventsPage {
        events,
        next_cursor,
        maintenance: Some(
            maintenance::metadata(pool, None, Some(workspace_id), query.include_statistics).await?,
        ),
    })
}

fn workspace_payload(action: &str, payload: &Value) -> Value {
    // Per-action allowlists fail closed for new events. Never project file
    // paths/hashes, command arguments, configuration, bodies or unknown fields.
    let fields: &[&str] = match action {
        "agent.create"
        | "agent.rename"
        | "agent.delete"
        | "agent.start"
        | "agent.stop"
        | "agent.restart"
        | "agent.abort"
        | "agent.startup.set"
        | "agent.startup.clear"
        | "agent.startup.manage"
        | "agent.startup.restart"
        | "app.create"
        | "app.start"
        | "app.stop"
        | "app.restart"
        | "app.delete"
        | "app.update"
        | "app.startup.update" => &["server_id", "startup_revision"],
        "machine.exec" => &["exit_code", "timed_out", "truncated", "timeout_ms"],
        "ingress.limits.update" => &[
            "rate_limit_per_minute",
            "rate_burst",
            "concurrency_limit",
            "ip_rate_limit_per_minute",
            "ip_rate_burst",
            "max_body_bytes",
            "response_timeout_secs",
            "body_timeout_secs",
        ],
        "workspace.audit.read"
        | "tree.folder.create"
        | "tree.folder.update"
        | "tree.folder.delete"
        | "tree.placement.create"
        | "tree.placement.update"
        | "tree.placement.delete"
        | "tree.pin.create"
        | "tree.pin.delete"
        | "tree.workload.rename"
        | "service.create"
        | "service.update"
        | "service.delete"
        | "virtual_host.create"
        | "virtual_host.delete"
        | "ingress.http.create"
        | "ingress.http.update"
        | "ingress.http.delete"
        | "ingress.tcp.publish"
        | "ingress.tcp.unpublish"
        | "machine.enroll"
        | "machine.reenroll"
        | "agent.prompt" => &[],
        _ => &[],
    };
    let mut result = serde_json::Map::new();
    for key in fields {
        if let Some(value) = payload.get(key) {
            // Strings only for stable machine IDs; no arbitrary nested text.
            let safe = if *key == "server_id" {
                value.as_str().is_some_and(|id| {
                    id.len() <= 128
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                })
            } else {
                value.is_boolean() || value.is_number() || value.is_null()
            };
            if safe {
                result.insert((*key).to_string(), value.clone());
            }
        }
    }
    Value::Object(result)
}

pub(crate) struct NewPlatformAuditEvent<'a> {
    pub action: &'a str,
    pub resource_kind: &'a str,
    pub resource_id: &'a str,
    pub resource_name: Option<&'a str>,
    pub payload: Value,
}

pub(crate) async fn record_platform(
    pool: &PgPool,
    event: NewPlatformAuditEvent<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO platform_audit_events(\
         event_id, occurred_at, action, resource_kind, resource_id, resource_name, payload) \
         VALUES($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(format!("aud_{}", Uuid::new_v4().simple()))
    .bind(Utc::now().to_rfc3339())
    .bind(event.action)
    .bind(event.resource_kind)
    .bind(event.resource_id)
    .bind(event.resource_name)
    .bind(event.payload)
    .execute(pool)
    .await?;
    Ok(())
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
