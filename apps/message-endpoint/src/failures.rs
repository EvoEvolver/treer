//! Durable terminal rejection receipts. The runtime must record before Core ack.
use crate::store::{valid_id, Error, Store};
use crate::transfers::TOMBSTONE_TTL;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    InvalidEnvelope,
    RecipientDenied,
    FragmentConflict,
    TransferExpired,
}
impl FailureReason {
    fn code(self) -> i64 {
        match self {
            Self::InvalidEnvelope => 0,
            Self::RecipientDenied => 1,
            Self::FragmentConflict => 2,
            Self::TransferExpired => 3,
        }
    }
    fn from_code(code: i64) -> rusqlite::Result<Self> {
        match code {
            0 => Ok(Self::InvalidEnvelope),
            1 => Ok(Self::RecipientDenied),
            2 => Ok(Self::FragmentConflict),
            3 => Ok(Self::TransferExpired),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Safe for the future owner settings API. No raw body/error or body hash.
#[derive(Serialize)]
pub struct Failure {
    pub delivery_id: String,
    pub reason: FailureReason,
    pub created_at: i64,
}

impl Store {
    /// Only receiver-derived trusted endpoint/generation/delivery may be used.
    /// Caller is responsible for identifying poison and verifying outer sender.
    /// Replaying the same receipt is safe. Full/disk failure means DO NOT ACK.
    pub fn record_failure(
        &mut self,
        endpoint: &str,
        generation: i64,
        delivery: &str,
        body: &[u8],
        reason: FailureReason,
        now: i64,
    ) -> Result<(), Error> {
        if !valid_id(delivery)
            || generation <= 0
            || now < 0
            || body.len() > crate::protocol::MAX_ENVELOPE
        {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        let binding: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM endpoints WHERE id=?1 AND generation=?2)",
            params![endpoint, generation],
            |r| r.get(0),
        )?;
        if !binding {
            return Err(Error::Unavailable);
        }
        let digest = Sha256::digest(body).to_vec();
        let previous=transaction.query_row(
            "SELECT digest,reason FROM failures WHERE endpoint=?1 AND generation=?2 AND delivery=?3",
            params![endpoint,generation,delivery], |r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,i64>(1)?))).optional()?;
        if let Some((old, code)) = previous {
            return if old == digest && code == reason.code() {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        // Separate bounded terminal receipt budget: a full pending queue must
        // not prevent recording poison, and receipts may not grow without bound.
        let (global, local) = transaction.query_row(
            "SELECT COUNT(*),COALESCE(SUM(endpoint=?1),0) FROM failures",
            [endpoint],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?;
        if global >= 10_000 || local >= 1000 {
            return Err(Error::Full);
        }
        transaction.execute(
            "INSERT INTO failures(endpoint,generation,delivery,digest,reason,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![endpoint,generation,delivery,digest,reason.code(),now])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn recent_failures(
        &self,
        endpoint: &str,
        generation: i64,
        limit: u32,
    ) -> Result<Vec<Failure>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Invalid);
        }
        let mut statement = self.db.prepare(
            "SELECT delivery,reason,created_at FROM failures WHERE endpoint=?1 AND generation=?2
             ORDER BY created_at DESC,delivery LIMIT ?3",
        )?;
        let rows = statement.query_map(params![endpoint, generation, limit], |r| {
            Ok(Failure {
                delivery_id: r.get(0)?,
                reason: FailureReason::from_code(r.get(1)?)?,
                created_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Completed queue/failure retention; pending and leased work is untouched.
    /// Run alongside cleanup_transfers. Returns total removed terminal rows.
    pub fn prune_completed(&mut self, now: i64) -> Result<usize, Error> {
        if now < TOMBSTONE_TTL {
            return Err(Error::Invalid);
        }
        let before = now - TOMBSTONE_TTL;
        let transaction = self.db.transaction()?;
        let mut removed =
            transaction.execute("DELETE FROM failures WHERE created_at<=?1", [before])?;
        for table in ["inbound", "outbound"] {
            // Constant table names only, never caller-controlled SQL.
            removed += transaction.execute(
                &format!("DELETE FROM {table} WHERE pending=0 AND terminal_at<=?1"),
                [before],
            )?;
        }
        transaction.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Limits;
    const NOW: i64 = 5_000_000;
    fn fixture() -> Store {
        let mut store = Store::memory(Limits {
            endpoint_count: 1,
            ..Limits::default()
        })
        .unwrap();
        store.register_paused("ag_one", 1, "ag_target").unwrap();
        store.register_paused("ag_two", 1, "ag_target").unwrap();
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        store
    }
    #[test]
    fn rejection_is_durable_idempotent_namespaced_and_contains_no_plaintext() {
        let mut store = fixture();
        // Fill pending quota first; terminal failure has its own bounded budget.
        store
            .submit("ag_one", 1, "del_good", "msg_good", b"accepted")
            .unwrap();
        store
            .record_failure(
                "ag_one",
                1,
                "del_bad",
                b"sensitive body",
                FailureReason::InvalidEnvelope,
                NOW,
            )
            .unwrap();
        store
            .record_failure(
                "ag_one",
                1,
                "del_bad",
                b"sensitive body",
                FailureReason::InvalidEnvelope,
                NOW + 1,
            )
            .unwrap();
        assert!(matches!(
            store.record_failure(
                "ag_one",
                1,
                "del_bad",
                b"other",
                FailureReason::InvalidEnvelope,
                NOW
            ),
            Err(Error::Conflict)
        ));
        store
            .record_failure(
                "ag_two",
                1,
                "del_bad",
                b"other",
                FailureReason::RecipientDenied,
                NOW,
            )
            .unwrap();
        assert!(matches!(
            store.record_failure(
                "ag_one",
                2,
                "del_bad",
                b"body",
                FailureReason::InvalidEnvelope,
                NOW
            ),
            Err(Error::Unavailable)
        ));
        let rows = store.recent_failures("ag_one", 1, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].created_at, NOW);
        let view = serde_json::to_string(&rows).unwrap();
        assert!(!view.contains("sensitive"));
        assert!(!view.contains("digest"));
        assert!(view.contains("invalid_envelope"));
        assert!(store.recent_failures("ag_one", 2, 10).unwrap().is_empty());
        assert!(store.recent_failures("ag_one", 1, 101).is_err());
    }
    #[test]
    fn full_failure_budget_keeps_previous_receipts_and_refuses_new_ack_permission() {
        let mut store = fixture();
        for index in 0..1000 {
            store
                .record_failure(
                    "ag_one",
                    1,
                    &format!("del_{index}"),
                    b"body",
                    FailureReason::InvalidEnvelope,
                    NOW,
                )
                .unwrap();
        }
        assert!(matches!(
            store.record_failure(
                "ag_one",
                1,
                "del_new",
                b"body",
                FailureReason::InvalidEnvelope,
                NOW
            ),
            Err(Error::Full)
        ));
        store
            .record_failure(
                "ag_one",
                1,
                "del_0",
                b"body",
                FailureReason::InvalidEnvelope,
                NOW,
            )
            .unwrap();
        assert_eq!(store.prune_completed(NOW + TOMBSTONE_TTL - 1).unwrap(), 0);
        assert_eq!(store.prune_completed(NOW + TOMBSTONE_TTL).unwrap(), 1000);
        store
            .record_failure(
                "ag_one",
                1,
                "del_new",
                b"body",
                FailureReason::InvalidEnvelope,
                NOW + TOMBSTONE_TTL,
            )
            .unwrap();
    }
    #[test]
    fn terminal_pruning_never_evicts_pending_or_leased_rows() {
        let mut store = fixture();
        store.submit("ag_one", 1, "d", "m", b"pending").unwrap();
        store
            .db
            .execute("UPDATE outbound SET terminal_at=?1", [NOW])
            .unwrap();
        assert_eq!(store.prune_completed(NOW + TOMBSTONE_TTL).unwrap(), 0);
        store
            .db
            .execute("UPDATE outbound SET pending=0", [])
            .unwrap();
        assert_eq!(store.prune_completed(NOW + TOMBSTONE_TTL).unwrap(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn schema_one_migrates_without_losing_data_or_rejection_reopen() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = base.join("state");
        let mut store = Store::open(&path, Limits::default()).unwrap();
        store.register_paused("ag_one", 1, "ag_target").unwrap();
        // Recreate this App's previous v1 shape, not another application's file.
        store
            .db
            .execute_batch(
                "DROP TABLE failures; ALTER TABLE inbound DROP COLUMN terminal_at;
            ALTER TABLE outbound DROP COLUMN terminal_at; PRAGMA user_version=1;",
            )
            .unwrap();
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        assert!(store.register_paused("ag_one", 1, "ag_target").is_err());
        assert_eq!(
            store
                .db
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        store
            .record_failure(
                "ag_one",
                1,
                "del_bad",
                b"bad body",
                FailureReason::InvalidEnvelope,
                NOW,
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path, Limits::default()).unwrap();
        assert_eq!(store.recent_failures("ag_one", 1, 10).unwrap().len(), 1);
    }
}
