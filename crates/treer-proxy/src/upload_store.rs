//! Durable rows for chunked machine uploads (#166): authorization binding
//! (subject, workspace, machine, destination), declared size, committed
//! offset and parts, a lease so one part is in flight per upload across
//! Proxy instances, terminal state with the receipt, and the quotas that are
//! counted here so they hold across instances. Bytes never touch this store.
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use treer_protocol::{MACHINE_UPLOAD_MAX_BYTES, MACHINE_UPLOAD_RECEIPT_RETENTION_SECONDS};

pub const UPLOADS_PER_USER: i64 = 2;
pub const UPLOADS_PER_WORKSPACE: i64 = 8;
pub const UPLOADS_PER_MACHINE: i64 = 8;
pub const BEGINS_PER_MINUTE: i64 = 10;
pub const DECLARED_BYTES_PER_MACHINE: i64 = 8 * MACHINE_UPLOAD_MAX_BYTES as i64;
/// How long one part may hold the upload before the lease lapses on its own.
pub const LEASE_SECONDS: i64 = 150;
/// A `committing` row older than this is recovered by the expiry job.
pub const COMMITTING_RECOVERY_SECONDS: i64 = 120;
/// A `pending` row (Controller begin never answered) older than this is dropped.
pub const PENDING_RECOVERY_SECONDS: i64 = 300;

pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRecord {
    pub offset: u64,
    pub length: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadReceipt {
    pub path: String,
    pub bytes_written: u64,
    pub sha256: Option<String>,
    pub committed_at: String,
    #[serde(default)]
    pub cleanup_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    Pending,
    Active,
    Committing,
    Committed,
    Aborted,
    Expired,
    Failed,
}

impl UploadState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Committing => "committing",
            Self::Committed => "committed",
            Self::Aborted => "aborted",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }
    fn parse(text: &str) -> Self {
        match text {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "committing" => Self::Committing,
            "committed" => Self::Committed,
            "aborted" => Self::Aborted,
            "expired" => Self::Expired,
            _ => Self::Failed,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::Aborted | Self::Expired | Self::Failed
        )
    }
}

#[derive(Debug, Clone)]
pub struct UploadRow {
    pub upload_id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub directory: String,
    pub file_name: String,
    pub overwrite: bool,
    pub size: u64,
    pub subject_kind: String,
    pub subject_id: String,
    pub idempotency_key: String,
    pub state: UploadState,
    pub committed_offset: u64,
    pub parts: Vec<PartRecord>,
    pub lease_until: Option<String>,
    pub created_at: String,
    pub last_part_at: String,
    pub finished_at: Option<String>,
    pub result: Option<UploadReceipt>,
    pub failure_code: Option<String>,
}

impl UploadRow {
    pub fn lease_held(&self, now: &str) -> bool {
        self.lease_until.as_deref().is_some_and(|until| until > now)
    }
    pub fn same_subject(&self, kind: &str, id: &str) -> bool {
        self.subject_kind == kind && self.subject_id == id
    }
}

#[derive(Debug, Clone)]
pub struct BeginParams<'a> {
    pub workspace_id: &'a str,
    pub server_id: &'a str,
    pub directory: &'a str,
    pub file_name: &'a str,
    pub overwrite: bool,
    pub size: u64,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub idempotency_key: &'a str,
    pub upload_id: &'a str,
}

#[derive(Debug)]
pub enum BeginOutcome {
    /// A new `pending` row; the caller must begin on the Controller and then
    /// activate or delete it.
    Created(UploadRow),
    /// The same key with identical parameters: nothing was consumed.
    Replayed(UploadRow),
    /// The same key with different parameters.
    Conflict,
    /// A quota or the begin rate is exhausted; the code is fixed.
    Refused(&'static str),
}

#[derive(Debug)]
pub struct UploadStoreError(pub sqlx::Error);

impl From<sqlx::Error> for UploadStoreError {
    fn from(error: sqlx::Error) -> Self {
        Self(error)
    }
}

#[derive(Clone)]
pub struct UploadStore {
    pool: PgPool,
}

fn row_from(row: sqlx::postgres::PgRow) -> UploadRow {
    let parts: Value = row.get("parts");
    let result: Option<Value> = row.get("result");
    UploadRow {
        upload_id: row.get("upload_id"),
        workspace_id: row.get("workspace_id"),
        server_id: row.get("server_id"),
        directory: row.get("directory"),
        file_name: row.get("file_name"),
        overwrite: row.get("overwrite"),
        size: row.get::<i64, _>("size").max(0) as u64,
        subject_kind: row.get("subject_kind"),
        subject_id: row.get("subject_id"),
        idempotency_key: row.get("idempotency_key"),
        state: UploadState::parse(row.get::<String, _>("state").as_str()),
        committed_offset: row.get::<i64, _>("committed_offset").max(0) as u64,
        parts: serde_json::from_value(parts).unwrap_or_default(),
        lease_until: row.get("lease_until"),
        created_at: row.get("created_at"),
        last_part_at: row.get("last_part_at"),
        finished_at: row.get("finished_at"),
        result: result.and_then(|value| serde_json::from_value(value).ok()),
        failure_code: row.get("failure_code"),
    }
}

const COLUMNS: &str = "upload_id, workspace_id, server_id, directory, file_name, overwrite, size, \
    subject_kind, subject_id, idempotency_key, state, committed_offset, parts, lease_until, \
    created_at, last_part_at, finished_at, result, failure_code";

impl UploadStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Quotas, begin rate and idempotency in one transaction serialized per
    /// machine, then a `pending` row (counted against the quotas already).
    pub async fn begin(&self, params: BeginParams<'_>) -> Result<BeginOutcome, UploadStoreError> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!(
                "machine_uploads:{}:{}",
                params.workspace_id, params.server_id
            ))
            .execute(&mut *tx)
            .await?;
        let existing = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM machine_uploads WHERE subject_kind = $1 AND subject_id = $2 \
             AND workspace_id = $3 AND server_id = $4 AND idempotency_key = $5"
        ))
        .bind(params.subject_kind)
        .bind(params.subject_id)
        .bind(params.workspace_id)
        .bind(params.server_id)
        .bind(params.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        .map(row_from);
        if let Some(existing) = existing {
            tx.commit().await?;
            let same = existing.directory == params.directory
                && existing.file_name == params.file_name
                && existing.overwrite == params.overwrite
                && existing.size == params.size;
            return Ok(if same {
                BeginOutcome::Replayed(existing)
            } else {
                BeginOutcome::Conflict
            });
        }
        let counts = sqlx::query(
            "SELECT \
             (SELECT count(*) FROM machine_uploads WHERE subject_kind = $1 AND subject_id = $2 \
              AND state IN ('pending','active','committing')) AS by_user, \
             (SELECT count(*) FROM machine_uploads WHERE workspace_id = $3 \
              AND state IN ('pending','active','committing')) AS by_workspace, \
             (SELECT count(*) FROM machine_uploads WHERE workspace_id = $3 AND server_id = $4 \
              AND state IN ('pending','active','committing')) AS by_machine, \
             (SELECT coalesce(sum(size), 0)::BIGINT FROM machine_uploads WHERE workspace_id = $3 AND server_id = $4 \
              AND state IN ('pending','active','committing')) AS declared, \
             (SELECT count(*) FROM machine_uploads WHERE subject_kind = $1 AND subject_id = $2 \
              AND created_at > $5) AS recent",
        )
        .bind(params.subject_kind)
        .bind(params.subject_id)
        .bind(params.workspace_id)
        .bind(params.server_id)
        .bind(stamp(now - Duration::seconds(60)))
        .fetch_one(&mut *tx)
        .await?;
        let by_user: i64 = counts.get("by_user");
        let by_workspace: i64 = counts.get("by_workspace");
        let by_machine: i64 = counts.get("by_machine");
        let declared: i64 = counts.get("declared");
        let recent: i64 = counts.get("recent");
        let refused = if recent >= BEGINS_PER_MINUTE {
            Some("machine_upload_rate_limited")
        } else if by_user >= UPLOADS_PER_USER
            || by_workspace >= UPLOADS_PER_WORKSPACE
            || by_machine >= UPLOADS_PER_MACHINE
            || declared.saturating_add(params.size as i64) > DECLARED_BYTES_PER_MACHINE
        {
            Some("machine_upload_busy")
        } else {
            None
        };
        if let Some(code) = refused {
            tx.commit().await?;
            return Ok(BeginOutcome::Refused(code));
        }
        let row = sqlx::query(&format!(
            "INSERT INTO machine_uploads({COLUMNS}) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'pending',0,'[]'::jsonb,NULL,$11,$11,NULL,NULL,NULL) \
             RETURNING {COLUMNS}"
        ))
        .bind(params.upload_id)
        .bind(params.workspace_id)
        .bind(params.server_id)
        .bind(params.directory)
        .bind(params.file_name)
        .bind(params.overwrite)
        .bind(params.size as i64)
        .bind(params.subject_kind)
        .bind(params.subject_id)
        .bind(params.idempotency_key)
        .bind(stamp(now))
        .fetch_one(&mut *tx)
        .await
        .map(row_from)?;
        tx.commit().await?;
        Ok(BeginOutcome::Created(row))
    }

    pub async fn activate(&self, upload_id: &str) -> Result<(), UploadStoreError> {
        sqlx::query("UPDATE machine_uploads SET state = 'active' WHERE upload_id = $1 AND state = 'pending'")
            .bind(upload_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete(&self, upload_id: &str) -> Result<(), UploadStoreError> {
        sqlx::query("DELETE FROM machine_uploads WHERE upload_id = $1")
            .bind(upload_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get(&self, upload_id: &str) -> Result<Option<UploadRow>, UploadStoreError> {
        Ok(sqlx::query(&format!(
            "SELECT {COLUMNS} FROM machine_uploads WHERE upload_id = $1"
        ))
        .bind(upload_id)
        .fetch_optional(&self.pool)
        .await?
        .map(row_from))
    }

    pub async fn list_own(
        &self,
        workspace_id: &str,
        server_id: &str,
        subject_kind: &str,
        subject_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Vec<UploadRow>, UploadStoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM machine_uploads WHERE workspace_id = $1 AND server_id = $2 \
             AND subject_kind = $3 AND subject_id = $4 AND ($5::text IS NULL OR idempotency_key = $5) \
             ORDER BY created_at DESC LIMIT 16"
        ))
        .bind(workspace_id)
        .bind(server_id)
        .bind(subject_kind)
        .bind(subject_id)
        .bind(idempotency_key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_from).collect())
    }

    /// Takes the per-upload lease for one part or commit; `None` when the
    /// upload is not active or another request holds it.
    pub async fn acquire_lease(
        &self,
        upload_id: &str,
    ) -> Result<Option<UploadRow>, UploadStoreError> {
        let now = Utc::now();
        Ok(sqlx::query(&format!(
            "UPDATE machine_uploads SET lease_until = $2 WHERE upload_id = $1 AND state = 'active' \
             AND (lease_until IS NULL OR lease_until < $3) RETURNING {COLUMNS}"
        ))
        .bind(upload_id)
        .bind(stamp(now + Duration::seconds(LEASE_SECONDS)))
        .bind(stamp(now))
        .fetch_optional(&self.pool)
        .await?
        .map(row_from))
    }

    pub async fn release_lease(&self, upload_id: &str) -> Result<(), UploadStoreError> {
        sqlx::query("UPDATE machine_uploads SET lease_until = NULL WHERE upload_id = $1")
            .bind(upload_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records Controller-confirmed progress (also used to adopt a Controller
    /// offset ahead of the row after a lost response).
    pub async fn record_progress(
        &self,
        upload_id: &str,
        committed_offset: u64,
        parts: &[PartRecord],
    ) -> Result<(), UploadStoreError> {
        sqlx::query(
            "UPDATE machine_uploads SET committed_offset = $2, parts = $3, last_part_at = $4 \
             WHERE upload_id = $1 AND state IN ('active','committing')",
        )
        .bind(upload_id)
        .bind(committed_offset as i64)
        .bind(serde_json::to_value(parts).unwrap_or(Value::Array(vec![])))
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves an active, complete, unleased upload to `committing`.
    pub async fn start_commit(
        &self,
        upload_id: &str,
    ) -> Result<Option<UploadRow>, UploadStoreError> {
        let now = Utc::now();
        Ok(sqlx::query(&format!(
            "UPDATE machine_uploads SET state = 'committing', lease_until = $2 WHERE upload_id = $1 \
             AND state = 'active' AND committed_offset = size AND (lease_until IS NULL OR lease_until < $3) \
             RETURNING {COLUMNS}"
        ))
        .bind(upload_id)
        .bind(stamp(now + Duration::seconds(LEASE_SECONDS)))
        .bind(stamp(now))
        .fetch_optional(&self.pool)
        .await?
        .map(row_from))
    }

    pub async fn set_committed(
        &self,
        upload_id: &str,
        receipt: &UploadReceipt,
    ) -> Result<(), UploadStoreError> {
        sqlx::query(
            "UPDATE machine_uploads SET state = 'committed', result = $2, finished_at = $3, lease_until = NULL \
             WHERE upload_id = $1 AND state IN ('active','committing')",
        )
        .bind(upload_id)
        .bind(serde_json::to_value(receipt).unwrap_or(Value::Null))
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_terminal(
        &self,
        upload_id: &str,
        state: UploadState,
        failure_code: &str,
    ) -> Result<(), UploadStoreError> {
        sqlx::query(
            "UPDATE machine_uploads SET state = $2, failure_code = $3, finished_at = $4, lease_until = NULL \
             WHERE upload_id = $1 AND state IN ('pending','active','committing')",
        )
        .bind(upload_id)
        .bind(state.as_str())
        .bind(failure_code)
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Rows the expiry job must look at: idle or overaged active/pending
    /// uploads, stuck pending rows and committing rows past the recovery age.
    pub async fn expiry_candidates(
        &self,
        idle_seconds: i64,
        lifetime_seconds: i64,
    ) -> Result<Vec<UploadRow>, UploadStoreError> {
        let now = Utc::now();
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM machine_uploads WHERE \
             (state = 'active' AND (last_part_at < $1 OR created_at < $2)) \
             OR (state = 'pending' AND created_at < $3) \
             OR (state = 'committing' AND lease_until < $4) \
             ORDER BY created_at LIMIT 64"
        ))
        .bind(stamp(now - Duration::seconds(idle_seconds)))
        .bind(stamp(now - Duration::seconds(lifetime_seconds)))
        .bind(stamp(now - Duration::seconds(PENDING_RECOVERY_SECONDS)))
        .bind(stamp(
            now - Duration::seconds(COMMITTING_RECOVERY_SECONDS - LEASE_SECONDS),
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_from).collect())
    }

    /// Deletes terminal rows past the receipt retention.
    pub async fn purge_terminal(&self) -> Result<u64, UploadStoreError> {
        let cutoff =
            stamp(Utc::now() - Duration::seconds(MACHINE_UPLOAD_RECEIPT_RETENTION_SECONDS as i64));
        Ok(sqlx::query(
            "DELETE FROM machine_uploads WHERE state IN ('committed','aborted','expired','failed') AND finished_at < $1",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}
