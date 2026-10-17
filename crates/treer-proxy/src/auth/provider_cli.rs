//! Provider CLI installation bookkeeping (#123): hashed single-use confirmation
//! tokens and machine-scoped operation records. Output never enters PostgreSQL;
//! the Controller keeps it. Every mutation here runs in one transaction with
//! its audit row so a prepare, acceptance or completion is either fully
//! recorded or absent.
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use treer_protocol::{
    ProviderCliAction, ProviderCliConsent, ProviderCliObservationSummary, ProviderCliObservedState,
    ProviderCliOperationStatus, ProviderCliRecipeRef, ProviderCliVersionRelation,
    PROVIDER_CLI_CONTROLLER_GRACE_SECONDS, PROVIDER_CLI_MAX_LIVE_TOKENS,
    PROVIDER_CLI_PREPARES_PER_MINUTE, PROVIDER_CLI_RETENTION_SECONDS,
    PROVIDER_CLI_TOKEN_TTL_SECONDS,
};
use uuid::Uuid;

use super::{AuthFailure, AuthStore};
use crate::audit::{self, NewAuditEvent};

pub(crate) const PROVIDER_CLI_TOKEN_PREFIX: &str = "pcl_";

/// Scope every provider CLI route shares after Policy passed.
#[derive(Debug, Clone)]
pub(crate) struct ProviderCliScope<'a> {
    pub workspace_id: &'a str,
    pub server_id: &'a str,
    pub user_id: &'a str,
    pub session_token: &'a str,
}

impl ProviderCliScope<'_> {
    fn session_hash(&self) -> String {
        sha256_hex(self.session_token)
    }
}

/// The consent a token binds (#134): the action and relation the human saw
/// and the path-free summary of the observation it was decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderCliObservedConsent {
    pub consent: ProviderCliConsent,
    pub state: ProviderCliObservedState,
    pub current_version: Option<String>,
    pub found_in_prefix: bool,
}

/// What a Controller report contributes to a terminal transition.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProviderCliFinishDetails<'a> {
    pub exit_code: Option<i32>,
    pub argv_hash: Option<&'a str>,
    pub observed_before: Option<&'a ProviderCliObservationSummary>,
}

/// Binding recorded with a token and copied to the operation it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderCliBinding {
    pub recipe: ProviderCliRecipeRef,
    pub table_hash: String,
    pub controller_source_sha: String,
    /// Hash of the displayed plan's command steps (see `plan_argv_hash`).
    pub argv_hash: String,
    /// Always present on tokens minted since #134; `None` only on operation
    /// rows written before observation existed.
    pub observed: Option<ProviderCliObservedConsent>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderCliOperationRecord {
    pub operation_id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub created_by: String,
    pub binding: ProviderCliBinding,
    pub status: ProviderCliOperationStatus,
    pub error_code: Option<String>,
    pub exit_code: Option<i32>,
    pub argv_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub reported_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl ProviderCliOperationRecord {
    pub(crate) fn expired(&self, now: DateTime<Utc>) -> bool {
        (now - self.created_at).num_seconds() > PROVIDER_CLI_RETENTION_SECONDS
    }

    /// A non-terminal operation whose Controller has been silent past the
    /// grace period is ambiguous. Silence is measured from the last report,
    /// else from dispatch, else from acceptance (an accepted but never
    /// dispatched orphan expires too); nothing is assumed finished or dead.
    pub(crate) fn controller_silent_past_grace(&self, now: DateTime<Utc>) -> bool {
        let baseline = self
            .reported_at
            .or(self.dispatched_at)
            .unwrap_or(self.created_at);
        !self.status.is_terminal()
            && (now - baseline).num_seconds() > PROVIDER_CLI_CONTROLLER_GRACE_SECONDS
    }
}

pub(crate) fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn new_confirmation_token() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{PROVIDER_CLI_TOKEN_PREFIX}{hex}")
}

fn token_shape_ok(token: &str) -> bool {
    token
        .strip_prefix(PROVIDER_CLI_TOKEN_PREFIX)
        .is_some_and(|rest| rest.len() == 64 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn invalid_token() -> AuthFailure {
    // Expired, consumed, foreign and malformed tokens are indistinguishable.
    AuthFailure::conflict(
        "provider_cli_token_invalid",
        "Confirmation is no longer valid; review the recipe again and confirm anew",
    )
}

pub(crate) fn machine_busy() -> AuthFailure {
    AuthFailure::conflict(
        "provider_cli_machine_busy",
        "Another provider CLI installation is active on this machine",
    )
}

fn acknowledgement_required() -> AuthFailure {
    AuthFailure::bad_request(
        "provider_cli_acknowledgement_required",
        "This installation replaces an existing CLI with a downgrade or unknown version; acknowledge the shown relation to proceed",
    )
}

fn acknowledgement_mismatch() -> AuthFailure {
    AuthFailure::bad_request(
        "provider_cli_acknowledgement_mismatch",
        "The acknowledged relation does not match the prepared installation",
    )
}

/// The install request must acknowledge exactly the bound relation when that
/// relation needs it, and must not carry one otherwise.
pub(crate) fn acknowledgement_ok(
    bound: Option<ProviderCliVersionRelation>,
    acknowledged: Option<ProviderCliVersionRelation>,
) -> Result<(), AuthFailure> {
    match (bound, acknowledged) {
        (Some(relation), None) if relation.requires_acknowledgement() => {
            Err(acknowledgement_required())
        }
        (Some(relation), Some(given)) if relation.requires_acknowledgement() => {
            if given == relation {
                Ok(())
            } else {
                Err(acknowledgement_mismatch())
            }
        }
        (_, Some(_)) => Err(acknowledgement_mismatch()),
        (_, None) => Ok(()),
    }
}

/// Audit payload: scope, hashes and the path-free observation summary.
fn binding_payload(binding: &ProviderCliBinding) -> Value {
    let mut payload = json!({
        "recipe_id": binding.recipe.recipe_id,
        "recipe_version": binding.recipe.recipe_version,
        "recipe_hash": binding.recipe.recipe_hash,
        "table_hash": binding.table_hash,
        "controller_source_sha": binding.controller_source_sha,
        "argv_hash": binding.argv_hash,
    });
    if let Some(observed) = &binding.observed {
        payload["action"] = Value::String(observed.consent.action.as_str().to_owned());
        payload["version_relation"] = observed
            .consent
            .version_relation
            .map(|relation| Value::String(relation.as_str().to_owned()))
            .unwrap_or(Value::Null);
        payload["observation_hash"] = Value::String(observed.consent.observation_hash.clone());
        payload["observed_state"] = Value::String(observed.state.as_str().to_owned());
        payload["observed_version"] = observed
            .current_version
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        payload["observed_in_prefix"] = Value::Bool(observed.found_in_prefix);
    }
    payload
}

/// Reads the consent columns shared by token and operation rows.
fn observed_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<Option<ProviderCliObservedConsent>, AuthFailure> {
    let action: Option<String> = row.try_get("action").map_err(AuthFailure::database)?;
    let Some(action) = action else {
        return Ok(None);
    };
    let corrupt = || {
        AuthFailure::internal(
            "provider_cli_operation_corrupt",
            "consent columns hold an unknown value".to_string(),
        )
    };
    let action = ProviderCliAction::parse(&action).ok_or_else(corrupt)?;
    let relation: Option<String> = row
        .try_get("version_relation")
        .map_err(AuthFailure::database)?;
    let version_relation = match relation {
        Some(text) => Some(ProviderCliVersionRelation::parse(&text).ok_or_else(corrupt)?),
        None => None,
    };
    let observation_hash: Option<String> = row
        .try_get("observation_hash")
        .map_err(AuthFailure::database)?;
    let state: Option<String> = row
        .try_get("observed_state")
        .map_err(AuthFailure::database)?;
    let state = ProviderCliObservedState::parse(state.as_deref().unwrap_or_default())
        .ok_or_else(corrupt)?;
    let found_in_prefix: Option<bool> = row
        .try_get("observed_in_prefix")
        .map_err(AuthFailure::database)?;
    Ok(Some(ProviderCliObservedConsent {
        consent: ProviderCliConsent {
            action,
            version_relation,
            observation_hash: observation_hash.ok_or_else(corrupt)?,
        },
        state,
        current_version: row
            .try_get("observed_version")
            .map_err(AuthFailure::database)?,
        found_in_prefix: found_in_prefix.unwrap_or(false),
    }))
}

fn version_i64(version: u64) -> Result<i64, AuthFailure> {
    i64::try_from(version).map_err(|_| {
        AuthFailure::bad_request("provider_cli_recipe_invalid", "recipe_version out of range")
    })
}

fn row_to_record(row: &sqlx::postgres::PgRow) -> Result<ProviderCliOperationRecord, AuthFailure> {
    let status: String = row.try_get("status").map_err(AuthFailure::database)?;
    let status = ProviderCliOperationStatus::parse(&status).ok_or_else(|| {
        AuthFailure::internal(
            "provider_cli_operation_corrupt",
            "operation record has an unknown status".to_string(),
        )
    })?;
    let recipe_version: i64 = row
        .try_get("recipe_version")
        .map_err(AuthFailure::database)?;
    Ok(ProviderCliOperationRecord {
        operation_id: row.try_get("operation_id").map_err(AuthFailure::database)?,
        workspace_id: row.try_get("workspace_id").map_err(AuthFailure::database)?,
        server_id: row.try_get("server_id").map_err(AuthFailure::database)?,
        created_by: row.try_get("created_by").map_err(AuthFailure::database)?,
        binding: ProviderCliBinding {
            recipe: ProviderCliRecipeRef {
                recipe_id: row.try_get("recipe_id").map_err(AuthFailure::database)?,
                recipe_version: u64::try_from(recipe_version).unwrap_or_default(),
                recipe_hash: row.try_get("recipe_hash").map_err(AuthFailure::database)?,
            },
            table_hash: row.try_get("table_hash").map_err(AuthFailure::database)?,
            controller_source_sha: row
                .try_get("controller_source_sha")
                .map_err(AuthFailure::database)?,
            argv_hash: row
                .try_get::<Option<String>, _>("argv_hash")
                .map_err(AuthFailure::database)?
                .unwrap_or_default(),
            observed: observed_from_row(row)?,
        },
        status,
        error_code: row.try_get("error_code").map_err(AuthFailure::database)?,
        exit_code: row.try_get("exit_code").map_err(AuthFailure::database)?,
        argv_hash: row.try_get("argv_hash").map_err(AuthFailure::database)?,
        created_at: timestamp(row, "created_at_ms")?.ok_or_else(|| {
            AuthFailure::internal(
                "provider_cli_operation_corrupt",
                "operation record lacks a creation time".to_string(),
            )
        })?,
        dispatched_at: timestamp(row, "dispatched_at_ms")?,
        reported_at: timestamp(row, "reported_at_ms")?,
        finished_at: timestamp(row, "finished_at_ms")?,
    })
}

// Timestamps travel as epoch milliseconds: sqlx is built without chrono.
const OPERATION_COLUMNS: &str = "operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, status, error_code, exit_code, argv_hash, action, version_relation, observation_hash, observed_state, observed_version, observed_in_prefix, (EXTRACT(EPOCH FROM created_at)*1000)::BIGINT AS created_at_ms, (EXTRACT(EPOCH FROM dispatched_at)*1000)::BIGINT AS dispatched_at_ms, (EXTRACT(EPOCH FROM reported_at)*1000)::BIGINT AS reported_at_ms, (EXTRACT(EPOCH FROM finished_at)*1000)::BIGINT AS finished_at_ms";

fn timestamp(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> Result<Option<DateTime<Utc>>, AuthFailure> {
    let millis: Option<i64> = row.try_get(column).map_err(AuthFailure::database)?;
    Ok(millis.and_then(DateTime::from_timestamp_millis))
}

fn tokens_exhausted() -> AuthFailure {
    AuthFailure::too_many_requests(
        "provider_cli_tokens_exhausted",
        "Too many unconfirmed installations; wait for them to expire",
    )
}

impl AuthStore {
    /// Admits one prepare before the Controller observes anything: at most
    /// ten attempts per minute and four live tokens per user and machine, by
    /// database time, counting every attempt whatever its outcome (noop,
    /// unobservable, refused recipe) so a session cannot make the machine run
    /// npm and the probe without bound.
    pub(crate) async fn provider_cli_prepare_begin(
        &self,
        scope: &ProviderCliScope<'_>,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!(
                "provider_cli:{}:{}:{}",
                scope.workspace_id, scope.server_id, scope.user_id
            ))
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM provider_cli_prepare_attempts WHERE attempt_id IN (SELECT attempt_id FROM provider_cli_prepare_attempts WHERE attempted_at < CURRENT_TIMESTAMP - INTERVAL '1 hour' LIMIT 100)")
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        let recent: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_cli_prepare_attempts WHERE workspace_id=$1 AND server_id=$2 AND user_id=$3 AND attempted_at > CURRENT_TIMESTAMP - INTERVAL '1 minute'",
        )
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if recent >= PROVIDER_CLI_PREPARES_PER_MINUTE {
            return Err(AuthFailure::too_many_requests(
                "provider_cli_rate_limited",
                "Too many installation requests; retry in a minute",
            ));
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_cli_install_tokens WHERE workspace_id=$1 AND server_id=$2 AND user_id=$3 AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP",
        )
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if live >= PROVIDER_CLI_MAX_LIVE_TOKENS {
            return Err(tokens_exhausted());
        }
        sqlx::query("INSERT INTO provider_cli_prepare_attempts(workspace_id, server_id, user_id) VALUES($1,$2,$3)")
            .bind(scope.workspace_id)
            .bind(scope.server_id)
            .bind(scope.user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    /// Mints a single-use confirmation token after the caller displayed the
    /// exact recipe and observation. `provider_cli_prepare_begin` already
    /// admitted the attempt; the live-token ceiling is restated here under the
    /// same lock so concurrent replicas cannot overshoot it.
    pub(crate) async fn provider_cli_prepare(
        &self,
        scope: &ProviderCliScope<'_>,
        binding: &ProviderCliBinding,
    ) -> Result<(String, DateTime<Utc>), AuthFailure> {
        let recipe_version = version_i64(binding.recipe.recipe_version)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!(
                "provider_cli:{}:{}:{}",
                scope.workspace_id, scope.server_id, scope.user_id
            ))
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        // Bounded cleanup of long-expired tokens; never a full-table sweep.
        sqlx::query("DELETE FROM provider_cli_install_tokens WHERE token_hash IN (SELECT token_hash FROM provider_cli_install_tokens WHERE expires_at < CURRENT_TIMESTAMP - INTERVAL '1 hour' LIMIT 100)")
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_cli_install_tokens WHERE workspace_id=$1 AND server_id=$2 AND user_id=$3 AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP",
        )
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if live >= PROVIDER_CLI_MAX_LIVE_TOKENS {
            return Err(tokens_exhausted());
        }
        let observed = binding.observed.as_ref().ok_or_else(|| {
            AuthFailure::internal(
                "provider_cli_consent_missing",
                "a token requires an observed consent".to_string(),
            )
        })?;
        if observed.consent.action == ProviderCliAction::Noop {
            return Err(AuthFailure::internal(
                "provider_cli_consent_missing",
                "a noop never receives a token".to_string(),
            ));
        }
        let token = new_confirmation_token();
        let expires_at_ms: i64 = sqlx::query_scalar(
            "INSERT INTO provider_cli_install_tokens(token_hash, workspace_id, server_id, user_id, session_hash, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, action, version_relation, observation_hash, observed_state, observed_version, observed_in_prefix, expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$12,$13,$14,$15,$16,$17,$18, CURRENT_TIMESTAMP + make_interval(secs => $11)) RETURNING (EXTRACT(EPOCH FROM expires_at)*1000)::BIGINT",
        )
        .bind(sha256_hex(&token))
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .bind(scope.session_hash())
        .bind(&binding.recipe.recipe_id)
        .bind(recipe_version)
        .bind(&binding.recipe.recipe_hash)
        .bind(&binding.table_hash)
        .bind(&binding.controller_source_sha)
        .bind(PROVIDER_CLI_TOKEN_TTL_SECONDS as f64)
        .bind(&binding.argv_hash)
        .bind(observed.consent.action.as_str())
        .bind(observed.consent.version_relation.map(|relation| relation.as_str()))
        .bind(&observed.consent.observation_hash)
        .bind(observed.state.as_str())
        .bind(&observed.current_version)
        .bind(observed.found_in_prefix)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        self.provider_cli_audit(
            &mut transaction,
            scope,
            "provider_cli.prepare",
            binding_payload(binding),
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let expires_at = DateTime::from_timestamp_millis(expires_at_ms).ok_or_else(|| {
            AuthFailure::internal(
                "provider_cli_token_corrupt",
                "token expiry out of range".to_string(),
            )
        })?;
        Ok((token, expires_at))
    }

    /// Durable reconciliation independent of polling: an active row whose
    /// Controller has been silent past the grace (last report, else dispatch,
    /// else acceptance) becomes `unknown`, audited once, and stops holding the
    /// machine slot. Nothing is assumed finished; the Controller still refuses a
    /// second job while its own is active. Returns the number of rows changed.
    pub(crate) async fn provider_cli_reconcile_stale(
        &self,
        scope: &ProviderCliScope<'_>,
    ) -> Result<usize, AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let changed = self.reconcile_stale_in(&mut transaction, scope).await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(changed)
    }

    async fn reconcile_stale_in(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &ProviderCliScope<'_>,
    ) -> Result<usize, AuthFailure> {
        let rows = sqlx::query(&format!(
            "UPDATE provider_cli_operations SET status='unknown', error_code='controller_unreachable', finished_at=CURRENT_TIMESTAMP WHERE workspace_id=$1 AND server_id=$2 AND status IN ('queued','running') AND COALESCE(reported_at, dispatched_at, created_at) < CURRENT_TIMESTAMP - make_interval(secs => $3) RETURNING {OPERATION_COLUMNS}"
        ))
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(PROVIDER_CLI_CONTROLLER_GRACE_SECONDS as f64)
        .fetch_all(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
        for row in &rows {
            let record = row_to_record(row)?;
            let mut payload = binding_payload(&record.binding);
            payload["operation_id"] = Value::String(record.operation_id.clone());
            payload["status"] = Value::String("unknown".into());
            payload["error_code"] = Value::String("controller_unreachable".into());
            payload["argv_hash"] = record.argv_hash.map(Value::String).unwrap_or(Value::Null);
            // The row may belong to another user: the control plane, not the
            // requester, is the actor of this reconciliation.
            payload["created_by"] = Value::String(record.created_by.clone());
            payload["reconciled_during_request_by"] = Value::String(scope.user_id.to_owned());
            self.provider_cli_audit_as(
                transaction,
                scope.workspace_id,
                scope.server_id,
                ("system", None),
                "provider_cli.install.finished",
                payload,
            )
            .await?;
        }
        Ok(rows.len())
    }

    /// Consumes the token and inserts the queued operation atomically. A busy
    /// machine is refused before consumption so the confirmation stays usable.
    /// Expired, consumed, foreign or mismatched tokens are one uniform 409.
    pub(crate) async fn provider_cli_accept(
        &self,
        scope: &ProviderCliScope<'_>,
        confirmation_token: &str,
        acknowledged_relation: Option<ProviderCliVersionRelation>,
    ) -> Result<ProviderCliOperationRecord, AuthFailure> {
        if !token_shape_ok(confirmation_token) {
            return Err(invalid_token());
        }
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        // A stale active row must not hold the slot forever (no poll required):
        // reconcile in this same transaction, before the busy check.
        self.reconcile_stale_in(&mut transaction, scope).await?;
        let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_cli_operations WHERE workspace_id=$1 AND server_id=$2 AND status IN ('queued','running'))")
            .bind(scope.workspace_id)
            .bind(scope.server_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        if busy {
            return Err(machine_busy());
        }
        let consumed = sqlx::query(
            "UPDATE provider_cli_install_tokens SET consumed_at=CURRENT_TIMESTAMP WHERE token_hash=$1 AND workspace_id=$2 AND server_id=$3 AND user_id=$4 AND session_hash=$5 AND consumed_at IS NULL AND expires_at > CURRENT_TIMESTAMP RETURNING recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, action, version_relation, observation_hash, observed_state, observed_version, observed_in_prefix",
        )
        .bind(sha256_hex(confirmation_token))
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .bind(scope.session_hash())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_token)?;
        let recipe_version: i64 = consumed
            .try_get("recipe_version")
            .map_err(AuthFailure::database)?;
        let binding = ProviderCliBinding {
            recipe: ProviderCliRecipeRef {
                recipe_id: consumed
                    .try_get("recipe_id")
                    .map_err(AuthFailure::database)?,
                recipe_version: u64::try_from(recipe_version).unwrap_or_default(),
                recipe_hash: consumed
                    .try_get("recipe_hash")
                    .map_err(AuthFailure::database)?,
            },
            table_hash: consumed
                .try_get("table_hash")
                .map_err(AuthFailure::database)?,
            controller_source_sha: consumed
                .try_get("controller_source_sha")
                .map_err(AuthFailure::database)?,
            argv_hash: consumed
                .try_get("argv_hash")
                .map_err(AuthFailure::database)?,
            observed: observed_from_row(&consumed)?,
        };
        // A token without consent predates observation: never installable.
        // Any refusal below rolls the transaction back, so the token stays
        // unconsumed for a corrected request.
        let observed = binding.observed.clone().ok_or_else(invalid_token)?;
        if observed.consent.action == ProviderCliAction::Noop {
            return Err(invalid_token());
        }
        acknowledgement_ok(observed.consent.version_relation, acknowledged_relation)?;
        let operation_id = format!("pco_{}", Uuid::new_v4().simple());
        let inserted = sqlx::query(&format!(
            "INSERT INTO provider_cli_operations(operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, status, action, version_relation, observation_hash, observed_state, observed_version, observed_in_prefix) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'queued',$11,$12,$13,$14,$15,$16) RETURNING {OPERATION_COLUMNS}"
        ))
        .bind(&operation_id)
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .bind(&binding.recipe.recipe_id)
        .bind(recipe_version)
        .bind(&binding.recipe.recipe_hash)
        .bind(&binding.table_hash)
        .bind(&binding.controller_source_sha)
        .bind(&binding.argv_hash)
        .bind(observed.consent.action.as_str())
        .bind(observed.consent.version_relation.map(|relation| relation.as_str()))
        .bind(&observed.consent.observation_hash)
        .bind(observed.state.as_str())
        .bind(&observed.current_version)
        .bind(observed.found_in_prefix)
        .fetch_one(&mut *transaction)
        .await;
        let row = match inserted {
            Ok(row) => row,
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                // Concurrent accept on another replica won; the whole
                // transaction rolls back, so this token is not consumed.
                return Err(machine_busy());
            }
            Err(error) => return Err(AuthFailure::database(error)),
        };
        let record = row_to_record(&row)?;
        let mut payload = binding_payload(&binding);
        payload["operation_id"] = Value::String(operation_id.clone());
        self.provider_cli_audit(
            &mut transaction,
            scope,
            "provider_cli.install.accepted",
            payload,
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(record)
    }

    /// Records the Controller's acknowledgement: the job is durable and running.
    pub(crate) async fn provider_cli_mark_dispatched(
        &self,
        operation_id: &str,
        argv_hash: &str,
    ) -> Result<(), AuthFailure> {
        sqlx::query("UPDATE provider_cli_operations SET status='running', argv_hash=$2, dispatched_at=CURRENT_TIMESTAMP, reported_at=CURRENT_TIMESTAMP WHERE operation_id=$1 AND status='queued'")
            .bind(operation_id)
            .bind(argv_hash)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    /// Terminal transition from a non-terminal state, with its audit row. An
    /// `unknown` record may still be resolved by the machine's own later
    /// observation; any other terminal state is final and audited once.
    pub(crate) async fn provider_cli_finish(
        &self,
        scope: &ProviderCliScope<'_>,
        operation_id: &str,
        status: ProviderCliOperationStatus,
        error_code: Option<&str>,
        reported: ProviderCliFinishDetails<'_>,
    ) -> Result<bool, AuthFailure> {
        debug_assert!(status.is_terminal());
        let ProviderCliFinishDetails {
            exit_code,
            argv_hash,
            observed_before,
        } = reported;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let updated = sqlx::query(&format!(
            "UPDATE provider_cli_operations SET status=$2, error_code=$3, exit_code=$4, argv_hash=COALESCE($5, argv_hash), reported_at=CURRENT_TIMESTAMP, finished_at=CURRENT_TIMESTAMP WHERE operation_id=$1 AND workspace_id=$6 AND server_id=$7 AND status IN ('queued','running','unknown') AND status <> $2 RETURNING {OPERATION_COLUMNS}"
        ))
        .bind(operation_id)
        .bind(status.as_str())
        .bind(error_code)
        .bind(exit_code)
        .bind(argv_hash)
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = updated else {
            return Ok(false);
        };
        let record = row_to_record(&row)?;
        let mut payload = binding_payload(&record.binding);
        payload["operation_id"] = Value::String(operation_id.to_owned());
        payload["status"] = Value::String(status.as_str().to_owned());
        payload["error_code"] = error_code
            .map(|code| Value::String(code.to_owned()))
            .unwrap_or(Value::Null);
        payload["exit_code"] = exit_code.map(Value::from).unwrap_or(Value::Null);
        payload["argv_hash"] = record
            .argv_hash
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        if let Some(observed_before) = observed_before {
            // Path-free by construction (see `ProviderCliObservationSummary`).
            payload["observed_before"] = json!({
                "state": observed_before.state.as_str(),
                "current_version": observed_before.current_version,
                "found_in_prefix": observed_before.found_in_prefix,
                "observation_hash": observed_before.observation_hash,
            });
        }
        self.provider_cli_audit(
            &mut transaction,
            scope,
            "provider_cli.install.finished",
            payload,
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(true)
    }

    /// Marks a still-running record as touched by a non-terminal Controller
    /// report so the grace-period rule only fires for silent Controllers.
    pub(crate) async fn provider_cli_touch(&self, operation_id: &str) -> Result<(), AuthFailure> {
        sqlx::query("UPDATE provider_cli_operations SET reported_at=CURRENT_TIMESTAMP, status='running' WHERE operation_id=$1 AND status IN ('queued','running')")
            .bind(operation_id)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    /// Creator-only lookup. Other users, machines, workspaces and unknown IDs
    /// receive the same uninformative not-found error.
    pub(crate) async fn provider_cli_operation(
        &self,
        scope: &ProviderCliScope<'_>,
        operation_id: &str,
    ) -> Result<ProviderCliOperationRecord, AuthFailure> {
        let row = sqlx::query(&format!(
            "SELECT {OPERATION_COLUMNS} FROM provider_cli_operations WHERE operation_id=$1 AND workspace_id=$2 AND server_id=$3 AND created_by=$4"
        ))
        .bind(operation_id)
        .bind(scope.workspace_id)
        .bind(scope.server_id)
        .bind(scope.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| {
            AuthFailure::not_found(
                "provider_cli_operation_not_found",
                "Installation operation not available",
            )
        })?;
        row_to_record(&row)
    }

    /// Records an outcome-only audit row (no output, no path) for a version
    /// check or a prepare that issued no token (`noop`, unobservable).
    pub(crate) async fn provider_cli_audit_outcome(
        &self,
        scope: &ProviderCliScope<'_>,
        event: &str,
        recipe: &ProviderCliRecipeRef,
        payload: Value,
    ) -> Result<(), AuthFailure> {
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let mut payload = payload;
        payload["recipe_id"] = Value::String(recipe.recipe_id.clone());
        payload["recipe_version"] = Value::from(recipe.recipe_version);
        payload["recipe_hash"] = Value::String(recipe.recipe_hash.clone());
        self.provider_cli_audit(&mut transaction, scope, event, payload)
            .await?;
        transaction.commit().await.map_err(AuthFailure::database)
    }

    async fn provider_cli_audit(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &ProviderCliScope<'_>,
        action: &str,
        payload: Value,
    ) -> Result<(), AuthFailure> {
        self.provider_cli_audit_as(
            transaction,
            scope.workspace_id,
            scope.server_id,
            ("user", Some(scope.user_id)),
            action,
            payload,
        )
        .await
    }

    async fn provider_cli_audit_as(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        workspace_id: &str,
        server_id: &str,
        (actor_kind, actor_id): (&str, Option<&str>),
        action: &str,
        payload: Value,
    ) -> Result<(), AuthFailure> {
        let organization_id: String =
            sqlx::query_scalar("SELECT organization_id FROM workspaces WHERE workspace_id = $1")
                .bind(workspace_id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(AuthFailure::database)?;
        audit::insert(
            transaction,
            NewAuditEvent {
                organization_id: &organization_id,
                workspace_id: Some(workspace_id),
                actor_kind,
                actor_id,
                source: "runtime",
                action,
                resource_kind: "machine",
                resource_id: server_id,
                resource_name: None,
                payload,
            },
        )
        .await
        .map_err(AuthFailure::database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_must_match_the_bound_relation_exactly() {
        use ProviderCliVersionRelation::*;
        assert!(acknowledgement_ok(None, None).is_ok());
        assert!(acknowledgement_ok(Some(Upgrade), None).is_ok());
        assert_eq!(
            acknowledgement_ok(Some(Upgrade), Some(Upgrade))
                .unwrap_err()
                .code(),
            "provider_cli_acknowledgement_mismatch"
        );
        assert_eq!(
            acknowledgement_ok(None, Some(Downgrade))
                .unwrap_err()
                .code(),
            "provider_cli_acknowledgement_mismatch"
        );
        for relation in [Downgrade, Incomparable, Unknown] {
            assert_eq!(
                acknowledgement_ok(Some(relation), None).unwrap_err().code(),
                "provider_cli_acknowledgement_required"
            );
            assert!(acknowledgement_ok(Some(relation), Some(relation)).is_ok());
            let other = if relation == Downgrade {
                Unknown
            } else {
                Downgrade
            };
            assert_eq!(
                acknowledgement_ok(Some(relation), Some(other))
                    .unwrap_err()
                    .code(),
                "provider_cli_acknowledgement_mismatch"
            );
        }
    }

    #[test]
    fn confirmation_tokens_have_a_fixed_shape_and_hash_once() {
        let token = new_confirmation_token();
        assert!(token_shape_ok(&token));
        assert!(!token_shape_ok("pcl_short"));
        assert!(!token_shape_ok(&token.replace("pcl_", "ses_")));
        assert_eq!(sha256_hex(&token).len(), 64);
        assert_ne!(new_confirmation_token(), token);
    }
}
