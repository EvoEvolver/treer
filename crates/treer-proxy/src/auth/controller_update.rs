//! Durable #222 intent, single-use confirmation and read-only reconciliation.
//! A committed acceptance authorizes exactly one send by that caller. Neither
//! reconnect nor a consumed-token lookup acquires dispatch authority again.
use super::{sha256_hex, AuthFailure, AuthStore, ProviderCliScope};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::{Postgres, Row, Transaction};
use treer_protocol::controller_update::{self as wire, Operation, State};
use treer_protocol::controller_update_runtime::DispatchBinding;

pub(crate) type ControllerUpdateScope<'a> = ProviderCliScope<'a>;

pub(crate) struct UpdateAcceptance {
    pub operation: Operation,
    /// True only for the transaction which consumed the unconsumed token.
    pub dispatch: Option<DispatchBinding>,
}

fn refusal(code: &str) -> AuthFailure {
    AuthFailure::conflict(code, "Controller update could not be accepted")
}
fn corrupt() -> AuthFailure {
    AuthFailure::internal(
        "controller_update_record_invalid",
        "Stored update evidence is invalid".into(),
    )
}
pub(crate) fn update_not_found() -> AuthFailure {
    AuthFailure::not_found(
        "controller_update_operation_not_found",
        "Update operation not found",
    )
}
fn value<T: serde::Serialize>(v: &T) -> Result<Value, AuthFailure> {
    serde_json::to_value(v).map_err(|_| corrupt())
}
fn operation(row: &sqlx::postgres::PgRow) -> Result<Operation, AuthFailure> {
    let op: Operation =
        serde_json::from_value(row.try_get("operation").map_err(AuthFailure::database)?)
            .map_err(|_| corrupt())?;
    op.validate().map_err(|_| corrupt())?;
    Ok(op)
}
fn bound(row: &sqlx::postgres::PgRow) -> Result<DispatchBinding, AuthFailure> {
    let binding: DispatchBinding =
        serde_json::from_value(row.try_get("binding").map_err(AuthFailure::database)?)
            .map_err(|_| corrupt())?;
    if !binding.validate() {
        return Err(corrupt());
    }
    Ok(binding)
}
/// All installer/update acceptances use this same transaction lock. Hash
/// collisions only serialize unrelated machines; they never grant authority.
pub(crate) async fn lock_machine_mutation(
    tx: &mut Transaction<'_, Postgres>,
    ws: &str,
    machine: &str,
) -> Result<(), AuthFailure> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("machine-mutation:{ws}:{machine}"))
        .execute(&mut **tx)
        .await
        .map_err(AuthFailure::database)?;
    Ok(())
}

impl AuthStore {
    pub(crate) async fn controller_update_prepare_begin(
        &self,
        s: &ControllerUpdateScope<'_>,
    ) -> Result<(), AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        lock_machine_mutation(&mut tx, s.workspace_id, s.server_id).await?;
        sqlx::query("DELETE FROM controller_update_previews WHERE token_hash IN (SELECT token_hash FROM controller_update_previews WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND created_at < CURRENT_TIMESTAMP - INTERVAL '420 seconds' LIMIT 100)")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM controller_update_attempts WHERE ctid IN (SELECT ctid FROM controller_update_attempts WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND attempted_at < CURRENT_TIMESTAMP - INTERVAL '1 hour' LIMIT 100)")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND NOT holds_slot AND finished_at < CURRENT_TIMESTAMP - INTERVAL '7 days'")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        let (attempts, live): (i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM controller_update_attempts WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND attempted_at > CURRENT_TIMESTAMP - INTERVAL '1 minute'), (SELECT COUNT(*) FROM controller_update_previews WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP)")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
        if attempts >= wire::PREPARE_ATTEMPTS_PER_MINUTE as i64
            || live >= wire::MAX_LIVE_TOKENS as i64
        {
            return Err(AuthFailure::too_many_requests(
                "controller_update_rate_limited",
                "Too many update previews",
            ));
        }
        sqlx::query("INSERT INTO controller_update_attempts(workspace_id,server_id,created_by) VALUES($1,$2,$3)")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)
    }

    /// Called only with a fresh validated current identity and a target from
    /// the trusted Proxy catalog, after admission and Policy authorization.
    pub(crate) async fn controller_update_preview(
        &self,
        s: &ControllerUpdateScope<'_>,
        binding: &DispatchBinding,
    ) -> Result<wire::Preview, AuthFailure> {
        if !binding.validate()
            || binding.workspace_id != s.workspace_id
            || binding.server_id != s.server_id
        {
            return Err(refusal("controller_update_preview_changed"));
        }
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        lock_machine_mutation(&mut tx, s.workspace_id, s.server_id).await?;
        self.controller_update_require_idle(&mut tx, s).await?;
        let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_previews WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
        if live >= wire::MAX_LIVE_TOKENS as i64 {
            return Err(refusal("controller_update_capacity_exhausted"));
        }
        // Random 256-bit value; only its hash enters storage or audit.
        use rand::RngCore;
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let expiry: i64 = sqlx::query_scalar("INSERT INTO controller_update_previews(token_hash,operation_id,workspace_id,server_id,created_by,session_hash,binding,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,CURRENT_TIMESTAMP + INTERVAL '120 seconds') RETURNING (EXTRACT(EPOCH FROM expires_at)*1000)::BIGINT")
            .bind(sha256_hex(&token)).bind(&binding.operation_id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).bind(sha256_hex(s.session_token)).bind(value(binding)?)
            .fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
        self.provider_cli_audit_as(
            &mut tx,
            s.workspace_id,
            s.server_id,
            ("user", Some(s.user_id)),
            "controller_update.prepare",
            value(binding)?,
        )
        .await?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(wire::Preview {
            operation_id: binding.operation_id.clone(),
            current: binding.current.clone(),
            target: binding.target.clone(),
            plan_digest: binding.plan_digest.clone(),
            steps: wire::PLAN_STEPS.to_vec(),
            confirmation_token: token,
            expires_at: DateTime::from_timestamp_millis(expiry).ok_or_else(corrupt)?,
        })
    }

    async fn controller_update_require_idle(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        s: &ControllerUpdateScope<'_>,
    ) -> Result<(), AuthFailure> {
        let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND (holds_slot OR (operation->>'state' IN ('failed','rolled_back') AND finished_at > CURRENT_TIMESTAMP - INTERVAL '60 seconds'))) OR EXISTS(SELECT 1 FROM provider_cli_operations WHERE workspace_id=$1 AND server_id=$2 AND status IN ('queued','running','unknown'))")
            .bind(s.workspace_id).bind(s.server_id).fetch_one(&mut **tx).await.map_err(AuthFailure::database)?;
        if busy {
            return Err(refusal("controller_update_busy"));
        }
        Ok(())
    }

    /// Read a candidate binding for revalidation; this confers no send right.
    pub(crate) async fn controller_update_confirmation(
        &self,
        s: &ControllerUpdateScope<'_>,
        token: &str,
    ) -> Result<DispatchBinding, AuthFailure> {
        let row = sqlx::query("SELECT binding FROM controller_update_previews WHERE token_hash=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 AND session_hash=$5 AND created_at > CURRENT_TIMESTAMP - INTERVAL '420 seconds' AND (consumed_at IS NOT NULL OR expires_at > CURRENT_TIMESTAMP)")
            .bind(sha256_hex(token)).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).bind(sha256_hex(s.session_token)).fetch_optional(&self.pool).await.map_err(AuthFailure::database)?.ok_or_else(||refusal("controller_update_confirmation_invalid"))?;
        bound(&row)
    }

    pub(crate) async fn controller_update_accept(
        &self,
        s: &ControllerUpdateScope<'_>,
        token: &str,
        expected: &DispatchBinding,
    ) -> Result<UpdateAcceptance, AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        lock_machine_mutation(&mut tx, s.workspace_id, s.server_id).await?;
        let row = sqlx::query("SELECT binding,consumed_at IS NOT NULL AS consumed,expires_at > CURRENT_TIMESTAMP AS valid FROM controller_update_previews WHERE token_hash=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 AND session_hash=$5 AND created_at > CURRENT_TIMESTAMP - INTERVAL '420 seconds' FOR UPDATE")
            .bind(sha256_hex(token)).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).bind(sha256_hex(s.session_token)).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?.ok_or_else(||refusal("controller_update_confirmation_invalid"))?;
        let binding = bound(&row)?;
        if &binding != expected {
            return Err(refusal("controller_update_preview_changed"));
        }
        if row
            .try_get::<bool, _>("consumed")
            .map_err(AuthFailure::database)?
        {
            let row = sqlx::query("SELECT operation FROM controller_update_operations WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4")
                .bind(&binding.operation_id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?.ok_or_else(update_not_found)?;
            return Ok(UpdateAcceptance {
                operation: operation(&row)?,
                dispatch: None,
            });
        }
        if !row
            .try_get::<bool, _>("valid")
            .map_err(AuthFailure::database)?
        {
            return Err(refusal("controller_update_confirmation_invalid"));
        }
        self.controller_update_require_idle(&mut tx, s).await?;
        let now_ms: i64 =
            sqlx::query_scalar("SELECT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)*1000)::BIGINT")
                .fetch_one(&mut *tx)
                .await
                .map_err(AuthFailure::database)?;
        let now = DateTime::from_timestamp_millis(now_ms).ok_or_else(corrupt)?;
        let op = Operation {
            operation_id: binding.operation_id.clone(),
            current: binding.current.clone(),
            target: binding.target.clone(),
            state: State::Accepted,
            phase: None,
            observation: wire::Observation::WaitingForReconnect,
            observed_identity: None,
            restored_identity: None,
            health: wire::Health::Unknown,
            receipt_revision: None,
            created_at: binding.created_at,
            updated_at: now.max(binding.created_at),
            observed_at: None,
            finished_at: None,
            error_code: None,
        };
        op.validate().map_err(|_| corrupt())?;
        // Admission remains bounded even when a caller never opens discovery.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND NOT holds_slot AND created_at > CURRENT_TIMESTAMP - INTERVAL '7 days'")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_one(&mut *tx).await.map_err(AuthFailure::database)?;
        if count >= wire::MAX_RESOLVED_OPERATIONS as i64 {
            return Err(refusal("controller_update_capacity_exhausted"));
        }
        sqlx::query("INSERT INTO controller_update_operations(operation_id,workspace_id,server_id,created_by,binding,operation,created_at) VALUES($1,$2,$3,$4,$5,$6,to_timestamp($7::double precision/1000))")
            .bind(&op.operation_id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).bind(value(&binding)?).bind(value(&op)?).bind(op.created_at.timestamp_millis() as f64).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        sqlx::query("UPDATE controller_update_previews SET consumed_at=CURRENT_TIMESTAMP WHERE token_hash=$1").bind(sha256_hex(token)).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        self.provider_cli_audit_as(
            &mut tx,
            s.workspace_id,
            s.server_id,
            ("user", Some(s.user_id)),
            "controller_update.accepted",
            value(&binding)?,
        )
        .await?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(UpdateAcceptance {
            operation: op,
            dispatch: Some(binding),
        })
    }

    pub(crate) async fn controller_update_operation(
        &self,
        s: &ControllerUpdateScope<'_>,
        id: &str,
    ) -> Result<Operation, AuthFailure> {
        let row = sqlx::query("SELECT operation FROM controller_update_operations WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 AND (holds_slot OR finished_at > CURRENT_TIMESTAMP - INTERVAL '7 days')")
            .bind(id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_optional(&self.pool).await.map_err(AuthFailure::database)?.ok_or_else(update_not_found)?;
        operation(&row)
    }

    pub(crate) async fn controller_update_discovery(
        &self,
        s: &ControllerUpdateScope<'_>,
    ) -> Result<wire::Discovery, AuthFailure> {
        // Reserve one place for the unresolved row before selecting recent
        // resolved rows. Unresolved can never disappear beneath newer history.
        let rows = sqlx::query("SELECT operation FROM (SELECT operation,created_at,operation_id FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND holds_slot UNION ALL SELECT operation,created_at,operation_id FROM (SELECT operation,created_at,operation_id FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND created_by=$3 AND NOT holds_slot AND finished_at > CURRENT_TIMESTAMP - INTERVAL '7 days' ORDER BY created_at DESC,operation_id DESC LIMIT 15) recent) selected ORDER BY created_at DESC,operation_id DESC")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_all(&self.pool).await.map_err(AuthFailure::database)?;
        let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND holds_slot AND created_by<>$3) OR EXISTS(SELECT 1 FROM provider_cli_operations WHERE workspace_id=$1 AND server_id=$2 AND status IN ('queued','running','unknown'))")
            .bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_one(&self.pool).await.map_err(AuthFailure::database)?;
        let discovery = wire::Discovery {
            operations: rows.iter().map(operation).collect::<Result<_, _>>()?,
            blocked_by_other_operation: blocked,
        };
        discovery.validate().map_err(|_| corrupt())?;
        Ok(discovery)
    }

    /// DB lease bounds read-only native queries across tabs and Proxy replicas.
    /// Lease expiry only permits a query; it never releases the mutation slot.
    pub(crate) async fn controller_update_query_lease(
        &self,
        s: &ControllerUpdateScope<'_>,
        id: &str,
    ) -> Result<bool, AuthFailure> {
        let changed = sqlx::query("UPDATE controller_update_operations SET queried_at=CURRENT_TIMESTAMP,query_lease_until=CURRENT_TIMESTAMP + INTERVAL '15 seconds' WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 AND holds_slot AND (queried_at IS NULL OR queried_at < CURRENT_TIMESTAMP - INTERVAL '2 seconds') AND (query_lease_until IS NULL OR query_lease_until < CURRENT_TIMESTAMP)")
            .bind(id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).execute(&self.pool).await.map_err(AuthFailure::database)?.rows_affected();
        Ok(changed == 1)
    }

    pub(crate) async fn controller_update_observe(
        &self,
        s: &ControllerUpdateScope<'_>,
        incoming: &Operation,
    ) -> Result<Operation, AuthFailure> {
        incoming
            .validate()
            .map_err(|_| refusal("controller_update_receipt_invalid"))?;
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        lock_machine_mutation(&mut tx, s.workspace_id, s.server_id).await?;
        let row=sqlx::query("SELECT operation,binding FROM controller_update_operations WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 FOR UPDATE")
            .bind(&incoming.operation_id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?.ok_or_else(update_not_found)?;
        let old = operation(&row)?;
        if !valid_transition(&old, incoming, Utc::now()) {
            return Err(refusal("controller_update_receipt_invalid"));
        }
        let resolved = matches!(
            incoming.state,
            State::Succeeded | State::Failed | State::RolledBack
        );
        sqlx::query("UPDATE controller_update_operations SET operation=$2,holds_slot=$3,finished_at=CASE WHEN $3 THEN NULL ELSE COALESCE(finished_at,CURRENT_TIMESTAMP) END,query_lease_until=NULL WHERE operation_id=$1")
            .bind(&incoming.operation_id).bind(value(incoming)?).bind(!resolved).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        if incoming != &old {
            self.provider_cli_audit_as(&mut tx,s.workspace_id,s.server_id,("system",None),"controller_update.observed",json!({"operation_id":incoming.operation_id,"state":incoming.state,"phase":incoming.phase,"receipt_revision":incoming.receipt_revision})).await?;
        }
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(incoming.clone())
    }
}

fn valid_transition(old: &Operation, new: &Operation, now: DateTime<Utc>) -> bool {
    if old.operation_id != new.operation_id
        || old.current != new.current
        || old.target != new.target
        || old.created_at != new.created_at
        || new.updated_at < old.updated_at
        || new.updated_at > now + chrono::Duration::seconds(30)
        || new
            .observed_at
            .is_some_and(|t| t > now + chrono::Duration::seconds(30))
    {
        return false;
    }
    if matches!(
        old.state,
        State::Succeeded
            | State::Failed
            | State::RolledBack
            | State::Partial
            | State::RollbackFailed
    ) {
        return old == new;
    }
    if let (Some(before), Some(after)) = (old.phase, new.phase) {
        use wire::Phase::*;
        let progresses = match before {
            Preparing => true,
            Prepared => !matches!(after, Preparing),
            Activating => !matches!(after, Preparing | Prepared | Failed),
            CoreHealthy => matches!(
                after,
                CoreHealthy
                    | LauncherInstalling
                    | RollingBack
                    | Complete
                    | RolledBack
                    | RollbackFailed
                    | Partial
            ),
            LauncherInstalling => matches!(after, LauncherInstalling | Complete | Partial),
            RollingBack => matches!(after, RollingBack | RolledBack | RollbackFailed),
            Complete | Failed | RolledBack | RollbackFailed | Partial => before == after,
        };
        if !progresses {
            return false;
        }
    }
    match (old.receipt_revision, new.receipt_revision) {
        (Some(a), Some(b)) if b < a => false,
        (Some(a), Some(b)) if a == b => old == new,
        (Some(_), None) => false,
        _ => true,
    }
}

impl AuthStore {
    /// The database is the locally provisioned release catalog, not a remote
    /// "latest" endpoint. Persist the highest revision read before serving it.
    pub(crate) async fn controller_update_targets(
        &self,
        platform: wire::Platform,
        current: &wire::Identity,
    ) -> Result<(u64, Vec<wire::Target>), AuthFailure> {
        let key = serde_json::to_value(platform)
            .map_err(|_| corrupt())?
            .as_str()
            .ok_or_else(corrupt)?
            .to_owned();
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("controller-update-catalog:{key}"))
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
        let revision: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(revision) FROM controller_update_catalog_entries WHERE platform=$1",
        )
        .bind(&key)
        .fetch_one(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        let revision = revision.ok_or_else(|| refusal("controller_update_catalog_unavailable"))?;
        if revision <= 0 || revision as u64 > (1 << 53) - 1 {
            return Err(corrupt());
        }
        let floor: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM controller_update_catalog_floor WHERE platform=$1",
        )
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AuthFailure::database)?;
        if floor.is_some_and(|f| f > revision) {
            return Err(refusal("controller_update_catalog_unavailable"));
        }
        let rows=sqlx::query("SELECT release_sequence,target FROM controller_update_catalog_entries WHERE platform=$1 AND revision=$2 ORDER BY release_sequence LIMIT 18")
            .bind(&key).bind(revision).fetch_all(&mut *tx).await.map_err(AuthFailure::database)?;
        if rows.len() > wire::MAX_TARGETS + 1 {
            return Err(corrupt());
        }
        let entries = rows
            .iter()
            .map(|row| {
                let sequence: i64 = row
                    .try_get("release_sequence")
                    .map_err(AuthFailure::database)?;
                let target: wire::Target =
                    serde_json::from_value(row.try_get("target").map_err(AuthFailure::database)?)
                        .map_err(|_| corrupt())?;
                target.validate().map_err(|_| corrupt())?;
                if target.platform != platform {
                    return Err(corrupt());
                }
                Ok((sequence, target))
            })
            .collect::<Result<Vec<_>, AuthFailure>>()?;
        let known = entries
            .iter()
            .find(|(_, t)| &t.identity == current)
            .map(|(seq, _)| *seq)
            .ok_or_else(|| refusal("controller_update_target_not_upgrade"))?;
        let targets = entries
            .into_iter()
            .filter(|(seq, _)| *seq > known)
            .map(|(_, t)| t)
            .collect();
        sqlx::query("INSERT INTO controller_update_catalog_floor(platform,revision) VALUES($1,$2) ON CONFLICT(platform) DO UPDATE SET revision=GREATEST(controller_update_catalog_floor.revision,EXCLUDED.revision)")
            .bind(key).bind(revision).execute(&mut *tx).await.map_err(AuthFailure::database)?;
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok((revision as u64, targets))
    }
}

impl AuthStore {
    /// Absence is not a terminal result. This changes only the presentation
    /// classification and retains the frozen binding, receipt and machine slot.
    pub(crate) async fn controller_update_note_silence(
        &self,
        s: &ControllerUpdateScope<'_>,
        id: &str,
    ) -> Result<Operation, AuthFailure> {
        let mut tx = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row=sqlx::query("SELECT operation FROM controller_update_operations WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4 FOR UPDATE")
            .bind(id).bind(s.workspace_id).bind(s.server_id).bind(s.user_id).fetch_optional(&mut *tx).await.map_err(AuthFailure::database)?.ok_or_else(update_not_found)?;
        let mut op = operation(&row)?;
        if matches!(op.state, State::Accepted | State::Updating)
            && Utc::now() - op.updated_at
                > chrono::Duration::seconds(wire::REPORT_GRACE_SECONDS as i64)
        {
            op.state = State::Unconfirmed;
            op.error_code = Some(wire::UpdateError::ResultUnconfirmed);
            op.health = wire::Health::Unknown;
            op.observation = wire::Observation::Unavailable;
            op.validate().map_err(|_| corrupt())?;
            sqlx::query(
                "UPDATE controller_update_operations SET operation=$2 WHERE operation_id=$1",
            )
            .bind(id)
            .bind(value(&op)?)
            .execute(&mut *tx)
            .await
            .map_err(AuthFailure::database)?;
            self.provider_cli_audit_as(
                &mut tx,
                s.workspace_id,
                s.server_id,
                ("system", None),
                "controller_update.unconfirmed",
                json!({"operation_id":id,"state":op.state}),
            )
            .await?;
        }
        tx.commit().await.map_err(AuthFailure::database)?;
        Ok(op)
    }
}

impl AuthStore {
    pub(crate) async fn controller_update_busy(
        &self,
        s: &ControllerUpdateScope<'_>,
    ) -> Result<bool, AuthFailure> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM controller_update_operations WHERE workspace_id=$1 AND server_id=$2 AND (holds_slot OR (operation->>'state' IN ('failed','rolled_back') AND finished_at > CURRENT_TIMESTAMP - INTERVAL '60 seconds'))) OR EXISTS(SELECT 1 FROM provider_cli_operations WHERE workspace_id=$1 AND server_id=$2 AND status IN ('queued','running','unknown'))")
            .bind(s.workspace_id).bind(s.server_id).fetch_one(&self.pool).await.map_err(AuthFailure::database)
    }
}
