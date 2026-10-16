//! Durable bounded admission; no request-owned background tasks or prompt bodies.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::{sync::oneshot, task::JoinHandle};

const BATCH: i64 = 256;
static UNCONFIRMED: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests;

pub(crate) fn capture_actor() -> MutationActor {
    MUTATION_ACTOR
        .try_with(Clone::clone)
        .unwrap_or(MutationActor {
            kind: "system",
            id: None,
        })
}

/// Success means committed admission; false means a durably counted overflow.
pub(crate) async fn enqueue_prompt(
    pool: &PgPool,
    workspace: &str,
    target: &str,
    actor: MutationActor,
) -> Result<bool, sqlx::Error> {
    let occurred_at = Utc::now().to_rfc3339();
    let event_id = format!("aud_{}", Uuid::new_v4().simple());
    sqlx::query_scalar("SELECT enqueue_prompt_audit($1,$2,$3,$4,$5,$6)")
        .bind(event_id)
        .bind(workspace)
        .bind(occurred_at)
        .bind(actor.kind)
        .bind(actor.id)
        .bind(target)
        .fetch_one(pool)
        .await
}

pub(crate) async fn prompt_best_effort(
    pool: &PgPool,
    workspace: &str,
    target: &str,
    actor: MutationActor,
) {
    match tokio::time::timeout(
        Duration::from_secs(1),
        enqueue_prompt(pool, workspace, target, actor),
    )
    .await
    {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => tracing::warn!(
            code = "audit_queue_overflow",
            "accepted prompt audit dropped; durable overflow counter incremented"
        ),
        // Never emit raw DB errors: they may contain row values. Commit timeout
        // is ambiguous; the entry may be replayed, so do not call it a sure drop.
        _ => {
            let total = UNCONFIRMED
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            tracing::warn!(
                code = "audit_enqueue_unconfirmed",
                unconfirmed_total = total,
                "accepted prompt audit admission unconfirmed (process-local count)"
            );
        }
    }
}

pub(crate) async fn drain_batch(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Same lock order as admission. One short batch at a time across replicas;
    // a deadlock with a concurrent organization cascade rolls back atomically.
    sqlx::query("SELECT singleton FROM audit_maintenance_settings WHERE singleton FOR UPDATE")
        .execute(&mut *tx)
        .await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT event_id FROM prompt_audit_outbox ORDER BY occurred_at,event_id LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(BATCH).fetch_all(&mut *tx).await?;
    sqlx::query("INSERT INTO organization_audit_events(event_id,organization_id,workspace_id,workspace_origin_id,occurred_at,actor_kind,actor_id,actor_name,source,action,outcome,resource_kind,resource_id,payload) \
        SELECT event_id,organization_id,workspace_id,workspace_origin_id,occurred_at,actor_kind,actor_id,actor_name,'control-plane','agent.prompt','succeeded','agent',resource_id,'{}'::JSONB \
        FROM prompt_audit_outbox WHERE event_id=ANY($1) ON CONFLICT(event_id) DO NOTHING")
        .bind(&ids).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM prompt_audit_outbox WHERE event_id=ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ids.len() as u64)
}

pub(crate) async fn configure(pool: &PgPool, days: Option<u16>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let old: i32 = sqlx::query_scalar(
        "SELECT retention_days FROM audit_maintenance_settings WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    let effective = days.map(i32::from).unwrap_or(old);
    if effective != old {
        sqlx::query("UPDATE audit_maintenance_settings SET retention_days=$1 WHERE singleton")
            .bind(effective)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO platform_audit_events(event_id,occurred_at,action,resource_kind,resource_id,payload) VALUES($1,$2,'audit.retention.configure','proxy','audit-retention',$3)")
            .bind(format!("aud_{}",Uuid::new_v4().simple())).bind(Utc::now().to_rfc3339())
            .bind(serde_json::json!({"previous_days":old,"retention_days":effective}))
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    tracing::info!(
        previous_days = old,
        retention_days = effective,
        source = if days.is_some() {
            "explicit"
        } else {
            "database"
        },
        "shared audit retention effective; deletion is irreversible"
    );
    Ok(())
}

/// Call only after the existing audit-read authorization. An organization scope
/// and an exact workspace scope must never be broadened into each other.
pub(crate) async fn metadata(
    pool: &PgPool,
    organization: Option<&str>,
    workspace: Option<&str>,
    statistics: bool,
) -> Result<treer_protocol::AuditMaintenance, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '2s'")
        .execute(&mut *tx)
        .await?;
    let org = match organization {
        Some(org) => org.to_owned(),
        None => sqlx::query_scalar::<_, String>(
            "SELECT organization_id FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL",
        )
        .bind(workspace)
        .fetch_one(&mut *tx)
        .await?,
    };
    let days: i32 =
        sqlx::query_scalar("SELECT retention_days FROM audit_maintenance_settings WHERE singleton")
            .fetch_one(&mut *tx)
            .await?;
    let deleted_through = sqlx::query_scalar::<_,i64>("SELECT (extract(epoch FROM deleted_through)*1000000)::BIGINT FROM audit_retention_boundaries WHERE organization_id=$1 AND scope_id=$2")
        .bind(&org).bind(workspace.unwrap_or("")).fetch_optional(&mut *tx).await?
        .map(timestamp).transpose()?;
    let since = timestamp(sqlx::query_scalar::<_,i64>("SELECT (extract(epoch FROM (date_trunc('day',CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - INTERVAL '29 days'))*1000000)::BIGINT")
        .fetch_one(&mut *tx).await?)?;
    let mut rows_per_day: Option<Vec<treer_protocol::AuditDailyRows>> = if statistics {
        Some(sqlx::query("SELECT to_char(occurred_at::TIMESTAMPTZ AT TIME ZONE 'UTC','YYYY-MM-DD') AS day,action,COUNT(*) AS rows \
            FROM organization_audit_events WHERE organization_id=$1 \
            AND ($2::TEXT IS NULL OR COALESCE(workspace_id,workspace_origin_id)=$2) \
            AND occurred_at::TIMESTAMPTZ >= $3::TEXT::TIMESTAMPTZ AND occurred_at::TIMESTAMPTZ < CURRENT_TIMESTAMP \
            GROUP BY day,action ORDER BY day,action LIMIT 4097")
            .bind(&org).bind(workspace).bind(since.to_rfc3339()).fetch_all(&mut *tx).await?
            .into_iter().map(|row| treer_protocol::AuditDailyRows {day:row.get("day"),action:row.get("action"),rows:row.get("rows")}).collect())
    } else {
        None
    };
    let statistics_truncated = rows_per_day.as_ref().is_some_and(|rows| rows.len() > 4096);
    if let Some(rows) = &mut rows_per_day {
        rows.truncate(4096);
    }
    let oldest_expired_at = if statistics {
        sqlx::query_scalar::<_,Option<i64>>("SELECT (extract(epoch FROM MIN(occurred_at::TIMESTAMPTZ))*1000000)::BIGINT \
            FROM organization_audit_events WHERE organization_id=$1 \
            AND ($2::TEXT IS NULL OR COALESCE(workspace_id,workspace_origin_id)=$2) \
            AND occurred_at::TIMESTAMPTZ < CURRENT_TIMESTAMP-(SELECT retention_days FROM audit_maintenance_settings WHERE singleton)*INTERVAL '1 day'")
            .bind(&org).bind(workspace).fetch_one(&mut *tx).await?.map(timestamp).transpose()?
    } else {
        None
    };
    tx.commit().await?;
    Ok(treer_protocol::AuditMaintenance {
        retention_days: days,
        deleted_through,
        statistics_since: statistics.then_some(since),
        rows_per_day,
        statistics_truncated,
        oldest_expired_at,
    })
}

fn timestamp(micros: i64) -> Result<DateTime<Utc>, sqlx::Error> {
    DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| sqlx::Error::Decode("audit timestamp out of range".into()))
}

pub(crate) async fn cleanup_batch(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // One batch, DB clock, every kind equally. A one-day prefix margin handles
    // historical RFC3339 offsets; the exact timestamp remains authoritative.
    let rows = sqlx::query("DELETE FROM organization_audit_events WHERE sequence IN (\
        SELECT sequence FROM organization_audit_events \
        WHERE left(occurred_at,10) <= to_char((CURRENT_TIMESTAMP-((SELECT retention_days FROM audit_maintenance_settings WHERE singleton)-1)*INTERVAL '1 day') AT TIME ZONE 'UTC','YYYY-MM-DD') \
        AND occurred_at::TIMESTAMPTZ < CURRENT_TIMESTAMP-(SELECT retention_days FROM audit_maintenance_settings WHERE singleton)*INTERVAL '1 day' \
        ORDER BY left(occurred_at,10),sequence LIMIT $1 FOR UPDATE SKIP LOCKED) \
        RETURNING organization_id,COALESCE(workspace_id,workspace_origin_id) AS scope_id,(extract(epoch FROM occurred_at::TIMESTAMPTZ)*1000000)::BIGINT AS retention_micros")
        .bind(BATCH).fetch_all(&mut *tx).await?;
    let mut boundaries = std::collections::BTreeMap::new();
    for row in &rows {
        let org: String = row.get("organization_id");
        let scope: Option<String> = row.get("scope_id");
        let time = timestamp(row.get("retention_micros"))?;
        for scope in std::iter::once("").chain(scope.as_deref()) {
            boundaries
                .entry((org.clone(), scope.to_owned()))
                .and_modify(|current: &mut DateTime<Utc>| *current = (*current).max(time))
                .or_insert(time);
        }
    }
    for ((org, scope), time) in boundaries {
        sqlx::query("INSERT INTO audit_retention_boundaries(organization_id,scope_id,deleted_through) VALUES($1,$2,$3::TIMESTAMPTZ) \
                ON CONFLICT(organization_id,scope_id) DO UPDATE SET deleted_through=GREATEST(audit_retention_boundaries.deleted_through,EXCLUDED.deleted_through)")
                .bind(org).bind(scope).bind(time.to_rfc3339()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(rows.len() as u64)
}

pub(crate) struct Worker {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// Catch up without an unbounded transaction or starving prompt drains. Budget
/// exhaustion schedules another cycle on the next one-second worker tick.
async fn cleanup_cycle(pool: &PgPool) -> Result<bool, sqlx::Error> {
    cleanup_with_budget(pool, Duration::from_secs(1)).await
}

async fn cleanup_with_budget(pool: &PgPool, budget: Duration) -> Result<bool, sqlx::Error> {
    let deadline = tokio::time::Instant::now() + budget;
    for _ in 0..64 {
        match tokio::time::timeout_at(deadline, cleanup_batch(pool)).await {
            Ok(Ok(rows)) if rows < BATCH as u64 => return Ok(false),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(true),
        }
    }
    Ok(true)
}

impl Worker {
    pub(crate) fn start(pool: PgPool) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut cleanup_due = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    _ = tick.tick() => {
                        if drain_batch(&pool).await.is_err() {
                            tracing::warn!(code="audit_drain_failed", "audit outbox drain failed; entries retained");
                        }
                        if tokio::time::Instant::now() >= cleanup_due {
                            let delay = match cleanup_cycle(&pool).await {
                                Ok(false) => 60,
                                Ok(true) => 1,
                                Err(_) => {
                                    tracing::warn!(code="audit_cleanup_failed", "audit retention batch failed");
                                    1
                                }
                            };
                            cleanup_due=tokio::time::Instant::now()+Duration::from_secs(delay);
                        }
                    },
                }
            }
            loop {
                match drain_batch(&pool).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => {
                        tracing::warn!(
                            code = "audit_flush_failed",
                            "audit shutdown flush failed; entries retained"
                        );
                        break;
                    }
                }
            }
        });
        Self { stop, task }
    }

    pub(crate) async fn shutdown(self) {
        let _ = self.stop.send(());
        let mut task = self.task;
        match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => tracing::warn!(
                code = "audit_worker_failed",
                "audit worker exited unexpectedly; committed entries retained"
            ),
            Err(_) => {
                task.abort();
                let _ = task.await;
                tracing::warn!(
                    code = "audit_flush_timeout",
                    "audit shutdown deadline reached; committed entries retained"
                );
            }
        }
    }
}
