//! Transactional queue slice. No public activation API exists: endpoints start
//! paused until owner provisioning and runtime readback are implemented/reviewed.
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("queue unavailable")]
    Storage(#[from] rusqlite::Error),
    #[error("private storage unavailable")]
    PrivateStorage,
    #[error("binding unavailable")]
    Unavailable,
    #[error("idempotency conflict")]
    Conflict,
    #[error("queue capacity reached")]
    Full,
    #[error("invalid queue item")]
    Invalid,
    #[error("lease unavailable")]
    Lease,
}

/// Configurable only downwards from these initial absolute maxima.
#[derive(Clone, Copy)]
pub struct Limits {
    pub endpoint_count: i64,
    pub endpoint_bytes: i64,
    pub global_count: i64,
    pub global_bytes: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            endpoint_count: 1000,
            endpoint_bytes: 16 * 1024 * 1024,
            global_count: 10_000,
            global_bytes: 128 * 1024 * 1024,
        }
    }
}

pub struct Store {
    pub(crate) db: Connection,
    pub(crate) limits: Limits,
    #[cfg(unix)]
    _private_files: Option<crate::private_store::PrivateFiles>,
}

/// One unique transport item, with stable IDs supplied by the trusted adapter.
/// Recipient is fixed by binding, never taken from external message text.
pub struct Inbound<'a> {
    pub endpoint: &'a str,
    pub generation: i64,
    pub external_id: &'a str,
    pub recipient: &'a str,
    pub body: &'a str,
}

pub struct Leased {
    pub job_id: i64,
    pub recipient: String,
    pub body: String,
    pub idempotency_key: String,
}

impl Store {
    /// In-memory construction for hermetic development.
    pub fn memory(limits: Limits) -> Result<Self, Error> {
        Self::from_connection(Connection::open_in_memory()?, limits)
    }

    /// Unix-only private persistent store; never creates missing parent paths.
    /// No runtime activation is implied by opening storage.
    #[cfg(unix)]
    pub fn open(directory: &std::path::Path, limits: Limits) -> Result<Self, Error> {
        let (db, files) = crate::private_store::open(directory)?;
        let mut store = Self::from_connection(db, limits)?;
        crate::private_store::enable_wal(&store.db, directory)?;
        store._private_files = Some(files);
        Ok(store)
    }

    fn from_connection(db: Connection, limits: Limits) -> Result<Self, Error> {
        let max = Limits::default();
        if limits.endpoint_count <= 0
            || limits.endpoint_count > max.endpoint_count
            || limits.endpoint_bytes <= 0
            || limits.endpoint_bytes > max.endpoint_bytes
            || limits.global_count <= 0
            || limits.global_count > max.global_count
            || limits.global_bytes <= 0
            || limits.global_bytes > max.global_bytes
        {
            return Err(Error::Invalid);
        }
        let application: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let tables: i64 = db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if !((application == 0 && version == 0 && tables == 0)
            || (application == 1413828168 && version == 1))
        {
            return Err(Error::PrivateStorage);
        }
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS endpoints (
                id TEXT PRIMARY KEY, generation INTEGER NOT NULL CHECK(generation>0),
                paused INTEGER NOT NULL DEFAULT 1 CHECK(paused IN (0,1)),
                recipient TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS outbound (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint TEXT NOT NULL REFERENCES endpoints(id), generation INTEGER NOT NULL,
                delivery TEXT NOT NULL, message TEXT NOT NULL, payload BLOB NOT NULL,
                digest BLOB NOT NULL, pending INTEGER NOT NULL DEFAULT 1,
                UNIQUE(endpoint,generation,delivery), UNIQUE(endpoint,generation,message));
             CREATE TABLE IF NOT EXISTS inbound (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint TEXT NOT NULL REFERENCES endpoints(id), generation INTEGER NOT NULL,
                external_id TEXT NOT NULL, recipient TEXT NOT NULL, body TEXT NOT NULL,
                digest BLOB NOT NULL, pending INTEGER NOT NULL DEFAULT 1,
                lease_token TEXT, lease_until INTEGER, core_id TEXT,
                UNIQUE(endpoint,generation,external_id));
             CREATE TABLE IF NOT EXISTS transfers (
                endpoint TEXT NOT NULL REFERENCES endpoints(id), generation INTEGER NOT NULL,
                direction INTEGER NOT NULL, transfer TEXT NOT NULL, digest TEXT NOT NULL,
                parts INTEGER NOT NULL, state INTEGER NOT NULL DEFAULT 0,
                expires INTEGER NOT NULL, terminal_at INTEGER,
                PRIMARY KEY(endpoint,generation,direction,transfer));
             CREATE TABLE IF NOT EXISTS fragments (
                endpoint TEXT NOT NULL, generation INTEGER NOT NULL,
                direction INTEGER NOT NULL, transfer TEXT NOT NULL, part INTEGER NOT NULL,
                wire TEXT, digest BLOB NOT NULL,
                PRIMARY KEY(endpoint,generation,direction,transfer,part),
                FOREIGN KEY(endpoint,generation,direction,transfer)
                  REFERENCES transfers(endpoint,generation,direction,transfer) ON DELETE CASCADE);
             PRAGMA application_id=1413828168;
             PRAGMA user_version=1;
             COMMIT;",
        )?;
        Ok(Self {
            db,
            limits,
            #[cfg(unix)]
            _private_files: None,
        })
    }

    pub fn register_paused(
        &mut self,
        id: &str,
        generation: i64,
        recipient: &str,
    ) -> Result<(), Error> {
        if !valid_id(id) || !valid_id(recipient) || generation <= 0 {
            return Err(Error::Invalid);
        }
        if self
            .db
            .query_row("SELECT COUNT(*) FROM endpoints", [], |r| r.get::<_, i64>(0))?
            >= 20
        {
            return Err(Error::Full);
        }
        self.db.execute(
            "INSERT INTO endpoints(id,generation,recipient) VALUES (?1,?2,?3)",
            params![id, generation, recipient],
        )?;
        Ok(())
    }

    pub fn pause(&mut self, endpoint: &str) -> Result<(), Error> {
        self.db
            .execute("UPDATE endpoints SET paused=1 WHERE id=?1", [endpoint])?;
        Ok(())
    }

    /// Payload is the strictly parsed, canonically serialized submit envelope.
    /// The daemon, not an external client, supplies Core IDs and serialization.
    pub fn submit(
        &mut self,
        endpoint: &str,
        generation: i64,
        delivery: &str,
        message: &str,
        payload: &[u8],
    ) -> Result<i64, Error> {
        if !valid_id(delivery)
            || !valid_id(message)
            || payload.is_empty()
            || payload.len() > super::protocol::MAX_ENVELOPE
        {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        available(&transaction, endpoint, generation)?;
        let digest = Sha256::digest(payload).to_vec();
        let previous = transaction
            .query_row(
                "SELECT id,digest,delivery,message FROM outbound
             WHERE endpoint=?1 AND generation=?2 AND (delivery=?3 OR message=?4)",
                params![endpoint, generation, delivery, message],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, old_digest, old_delivery, old_message)) = previous {
            if digest != old_digest || old_delivery != delivery || old_message != message {
                return Err(Error::Conflict);
            }
            return Ok(id);
        }
        capacity(&transaction, self.limits, endpoint, payload.len(), 1)?;
        transaction.execute(
            "INSERT INTO outbound(endpoint,generation,delivery,message,payload,digest)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![endpoint, generation, delivery, message, payload, digest],
        )?;
        let receipt = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn enqueue(&mut self, item: Inbound<'_>) -> Result<i64, Error> {
        if !valid_id(item.external_id) || item.body.len() > super::protocol::MAX_BODY {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        let recipient = available(&transaction, item.endpoint, item.generation)?;
        if recipient != item.recipient {
            return Err(Error::Unavailable);
        }
        let digest = Sha256::digest(item.body.as_bytes()).to_vec();
        let previous = transaction.query_row(
            "SELECT id,digest,recipient FROM inbound WHERE endpoint=?1 AND generation=?2 AND external_id=?3",
            params![item.endpoint,item.generation,item.external_id],
            |r| Ok((r.get::<_, i64>(0)?,r.get::<_, Vec<u8>>(1)?,r.get::<_, String>(2)?)),
        ).optional()?;
        if let Some((id, old_digest, old_recipient)) = previous {
            if old_digest != digest || old_recipient != item.recipient {
                return Err(Error::Conflict);
            }
            return Ok(id);
        }
        capacity(&transaction, self.limits, item.endpoint, item.body.len(), 1)?;
        transaction.execute(
            "INSERT INTO inbound(endpoint,generation,external_id,recipient,body,digest)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                item.endpoint,
                item.generation,
                item.external_id,
                item.recipient,
                item.body,
                digest
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(id)
    }

    /// Caller supplies a fresh cryptographically random opaque lease token.
    /// Time is injected for deterministic expiry tests; daemon uses its own clock.
    pub fn lease(
        &mut self,
        endpoint: &str,
        generation: i64,
        now: i64,
        token: &str,
    ) -> Result<Option<Leased>, Error> {
        if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid);
        }
        let until = now.checked_add(60).ok_or(Error::Invalid)?;
        let transaction = self.db.transaction()?;
        available(&transaction, endpoint, generation)?;
        let first = transaction
            .query_row(
                "SELECT id,recipient,body,lease_until,external_id FROM inbound
             WHERE endpoint=?1 AND generation=?2 AND pending=1 ORDER BY id LIMIT 1",
                params![endpoint, generation],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, recipient, body, expiry, external_id)) = first else {
            return Ok(None);
        };
        if expiry.is_some_and(|expiry| expiry > now) {
            return Ok(None);
        }
        transaction.execute(
            "UPDATE inbound SET lease_token=?1,lease_until=?2 WHERE id=?3",
            params![token, until, id],
        )?;
        transaction.commit()?;
        Ok(Some(Leased {
            job_id: id,
            recipient,
            body,
            idempotency_key: format!(
                "ep-in-{}",
                super::protocol::digest_id(
                    "inbound-delivery",
                    endpoint,
                    generation as u64,
                    &external_id
                )
            ),
        }))
    }

    pub fn complete(
        &mut self,
        endpoint: &str,
        generation: i64,
        job: i64,
        token: &str,
        core_id: &str,
    ) -> Result<(), Error> {
        if !valid_id(core_id) {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        // Allow recording an already accepted send after pause, but never across
        // generation changes or without this job's active lease capability.
        let current: Option<i64> = transaction
            .query_row(
                "SELECT generation FROM endpoints WHERE id=?1",
                [endpoint],
                |r| r.get(0),
            )
            .optional()?;
        if current != Some(generation) {
            return Err(Error::Unavailable);
        }
        let row = transaction.query_row(
            "SELECT lease_token,core_id FROM inbound WHERE id=?1 AND endpoint=?2 AND generation=?3",
            params![job,endpoint,generation],
            |r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?)),
        ).optional()?.ok_or(Error::Lease)?;
        if row.0.as_deref() != Some(token) {
            return Err(Error::Lease);
        }
        if let Some(previous) = row.1 {
            return if previous == core_id {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        transaction.execute(
            "UPDATE inbound SET pending=0,core_id=?1 WHERE id=?2",
            params![core_id, job],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}
pub(crate) fn available(
    transaction: &Transaction<'_>,
    endpoint: &str,
    generation: i64,
) -> Result<String, Error> {
    transaction
        .query_row(
            "SELECT recipient FROM endpoints WHERE id=?1 AND generation=?2 AND paused=0",
            params![endpoint, generation],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::Unavailable)
}
pub(crate) fn capacity(
    transaction: &Transaction<'_>,
    limits: Limits,
    endpoint: &str,
    added: usize,
    added_count: i64,
) -> Result<(), Error> {
    let (count,bytes,local_count,local_bytes) = transaction.query_row(
        "SELECT COUNT(*),COALESCE(SUM(size),0),COALESCE(SUM(endpoint=?1),0),
         COALESCE(SUM(CASE WHEN endpoint=?1 THEN size ELSE 0 END),0)
         FROM (SELECT endpoint,length(payload) AS size FROM outbound WHERE pending=1
               UNION ALL SELECT endpoint,length(CAST(body AS BLOB)) AS size FROM inbound WHERE pending=1
               UNION ALL SELECT t.endpoint,COALESCE(SUM(length(CAST(f.wire AS BLOB))),0) AS size
                 FROM transfers t LEFT JOIN fragments f USING(endpoint,generation,direction,transfer)
                 WHERE t.state IN (0,1) GROUP BY t.endpoint,t.generation,t.direction,t.transfer)",
        [endpoint], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?)),
    )?;
    let added = i64::try_from(added).map_err(|_| Error::Invalid)?;
    if count + added_count > limits.global_count
        || local_count + added_count > limits.endpoint_count
        || bytes + added > limits.global_bytes
        || local_bytes + added > limits.endpoint_bytes
    {
        return Err(Error::Full);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(limits: Limits) -> Store {
        let mut s = Store::memory(limits).unwrap();
        s.register_paused("ag_one", 1, "ag_target").unwrap();
        s.register_paused("ag_two", 1, "ag_target").unwrap();
        // Test-only authorization fixture. No production activation API exists.
        s.db.execute("UPDATE endpoints SET paused=0", []).unwrap();
        s
    }
    fn enqueue(s: &mut Store, endpoint: &str, id: &str, body: &str) -> Result<i64, Error> {
        s.enqueue(Inbound {
            endpoint,
            generation: 1,
            external_id: id,
            recipient: "ag_target",
            body,
        })
    }
    #[test]
    fn idempotent_receipts_conflicts_and_namespaces() {
        let mut s = fixture(Limits::default());
        let id = s.submit("ag_one", 1, "d", "m", b"payload").unwrap();
        assert_eq!(id, s.submit("ag_one", 1, "d", "m", b"payload").unwrap());
        assert!(matches!(
            s.submit("ag_one", 1, "d", "m", b"changed"),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            s.submit("ag_one", 1, "other", "m", b"payload"),
            Err(Error::Conflict)
        ));
        assert_ne!(id, s.submit("ag_two", 1, "d", "m", b"payload").unwrap());
        assert!(matches!(
            s.submit("ag_one", 2, "d", "m", b"payload"),
            Err(Error::Unavailable)
        ));
    }
    #[test]
    fn shared_capacity_never_evicts_and_dedup_survives_full() {
        let mut s = fixture(Limits {
            endpoint_count: 2,
            global_count: 2,
            ..Limits::default()
        });
        let id = enqueue(&mut s, "ag_one", "external", "hello").unwrap();
        s.submit("ag_one", 1, "d", "m", b"payload").unwrap();
        assert!(matches!(
            enqueue(&mut s, "ag_two", "new", "lost"),
            Err(Error::Full)
        ));
        assert_eq!(id, enqueue(&mut s, "ag_one", "external", "hello").unwrap());
        assert!(matches!(
            enqueue(&mut s, "ag_one", "external", "different"),
            Err(Error::Conflict)
        ));
        assert_eq!(
            s.db.query_row("SELECT COUNT(*) FROM inbound", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn byte_caps_measure_utf8_not_characters() {
        let mut s = fixture(Limits {
            endpoint_bytes: 5,
            ..Limits::default()
        });
        enqueue(&mut s, "ag_one", "one", "界").unwrap();
        assert!(matches!(
            enqueue(&mut s, "ag_one", "two", "界"),
            Err(Error::Full)
        ));
    }
    #[test]
    fn fifo_lease_expiry_reuses_key_and_rejects_stale_or_cross_endpoint_completion() {
        let mut s = fixture(Limits::default());
        let first = enqueue(&mut s, "ag_one", "one", "first").unwrap();
        enqueue(&mut s, "ag_one", "two", "second").unwrap();
        let token1 = "a".repeat(64);
        let token2 = "b".repeat(64);
        let initial = s.lease("ag_one", 1, 100, &token1).unwrap().unwrap();
        assert_eq!(initial.job_id, first);
        assert!(s.lease("ag_one", 1, 159, &token2).unwrap().is_none());
        let retry = s.lease("ag_one", 1, 160, &token2).unwrap().unwrap();
        assert_eq!(retry.idempotency_key, initial.idempotency_key);
        assert!(matches!(
            s.complete("ag_one", 1, first, &token1, "msg"),
            Err(Error::Lease)
        ));
        assert!(matches!(
            s.complete("ag_two", 1, first, &token2, "msg"),
            Err(Error::Lease)
        ));
        s.complete("ag_one", 1, first, &token2, "msg").unwrap();
        s.complete("ag_one", 1, first, &token2, "msg").unwrap();
        assert!(matches!(
            s.complete("ag_one", 1, first, &token2, "other"),
            Err(Error::Conflict)
        ));
        assert_ne!(
            s.lease("ag_one", 1, 161, &token1).unwrap().unwrap().job_id,
            first
        );
    }
    #[test]
    fn pause_blocks_both_directions_but_records_already_accepted_send() {
        let mut s = fixture(Limits::default());
        let id = enqueue(&mut s, "ag_one", "one", "hello").unwrap();
        let token = "a".repeat(64);
        s.lease("ag_one", 1, 10, &token).unwrap();
        s.pause("ag_one").unwrap();
        assert!(matches!(
            s.lease("ag_one", 1, 100, &token),
            Err(Error::Unavailable)
        ));
        assert!(matches!(
            s.submit("ag_one", 1, "d", "m", b"body"),
            Err(Error::Unavailable)
        ));
        assert!(matches!(
            enqueue(&mut s, "ag_one", "two", "body"),
            Err(Error::Unavailable)
        ));
        s.complete("ag_one", 1, id, &token, "msg").unwrap();
    }
    #[test]
    fn rebuilt_database_reused_row_ids_do_not_reuse_unrelated_core_keys() {
        let mut original = fixture(Limits::default());
        let token = "a".repeat(64);
        let first_id = enqueue(&mut original, "ag_one", "external-original", "hello").unwrap();
        let original_key = original
            .lease("ag_one", 1, 100, &token)
            .unwrap()
            .unwrap()
            .idempotency_key;
        // Reconstruct the row after backup/rebuild, deliberately changing row order.
        let mut restored = fixture(Limits::default());
        let reused_id = enqueue(&mut restored, "ag_one", "external-unrelated", "other").unwrap();
        assert_eq!(first_id, reused_id);
        let unrelated = restored.lease("ag_one", 1, 100, &token).unwrap().unwrap();
        assert_ne!(original_key, unrelated.idempotency_key);
        restored
            .complete("ag_one", 1, reused_id, &token, "msg_other")
            .unwrap();
        let replay_id = enqueue(&mut restored, "ag_one", "external-original", "hello").unwrap();
        assert_ne!(first_id, replay_id);
        let replay = restored.lease("ag_one", 1, 101, &token).unwrap().unwrap();
        assert_eq!(original_key, replay.idempotency_key);
    }

    #[test]
    fn recipient_is_binding_owned_not_external_text() {
        let mut s = fixture(Limits::default());
        assert!(matches!(
            s.enqueue(Inbound {
                endpoint: "ag_one",
                generation: 1,
                external_id: "id",
                recipient: "other",
                body: "x"
            }),
            Err(Error::Unavailable)
        ));
    }
}
