//! Inert terminal-mismatch ledger schema. No runtime caller or ACK authority.
//! Receipt admission, confirmed-ACK marking and owner recovery are separate work.
use crate::store::{Error, Store};

pub const PER_BINDING_ROWS: i64 = 32;
pub const GLOBAL_ROWS: i64 = 256;
pub const ACKED_FLOOR: i64 = 8;
/// Two 32-byte hashes plus five 64-bit scalars, including the declared sequence.
pub const LOGICAL_ROW_BYTES: usize = 104;

/// Uses the existing declared AUTOINCREMENT inbound key. Binding identity comes
/// from that protected parent, never an implicit rowid or another copied name.
/// No existing parent records are rewritten. All schema changes commit together.
pub fn initialize(store: &mut Store) -> Result<(), Error> {
    #[cfg(unix)]
    store.recheck_private_storage()?;
    let tx = store.db.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS telegram_terminal_dispositions(
          ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
          delivery_key BLOB NOT NULL UNIQUE
            CHECK(typeof(delivery_key)='blob' AND length(delivery_key)=32),
          receipt_digest BLOB NOT NULL
            CHECK(typeof(receipt_digest)='blob' AND length(receipt_digest)=32),
          job INTEGER NOT NULL REFERENCES inbound(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
          first_seen INTEGER NOT NULL CHECK(typeof(first_seen)='integer' AND first_seen>=0),
          acked INTEGER NOT NULL DEFAULT 0 CHECK(typeof(acked)='integer' AND acked IN (0,1)),
          conflict INTEGER NOT NULL DEFAULT 0 CHECK(typeof(conflict)='integer' AND conflict IN (0,1)),
          CHECK(typeof(job)='integer' AND job>0),
          CHECK(ordinal>0));
         CREATE INDEX IF NOT EXISTS telegram_terminal_dispositions_job
          ON telegram_terminal_dispositions(job);
         CREATE TRIGGER IF NOT EXISTS telegram_terminal_dispositions_cap
          BEFORE INSERT ON telegram_terminal_dispositions BEGIN
          SELECT CASE WHEN
            (SELECT count(*) FROM telegram_terminal_dispositions)>=256 OR
            (SELECT count(*) FROM telegram_terminal_dispositions d
             JOIN inbound old ON old.id=d.job
             WHERE old.endpoint=(SELECT endpoint FROM inbound WHERE id=NEW.job))>=32
          THEN RAISE(ABORT,'terminal_disposition_capacity') END;
         END;
         CREATE TRIGGER IF NOT EXISTS telegram_terminal_dispositions_identity
          BEFORE UPDATE OF ordinal,delivery_key,receipt_digest,job,first_seen
          ON telegram_terminal_dispositions BEGIN
          SELECT RAISE(ABORT,'terminal_disposition_identity'); END;
         CREATE TRIGGER IF NOT EXISTS telegram_terminal_dispositions_parent
          BEFORE UPDATE OF id,endpoint,generation,recipient,core_id ON inbound
          WHEN EXISTS(SELECT 1 FROM telegram_terminal_dispositions WHERE job=OLD.id)
          BEGIN SELECT RAISE(ABORT,'terminal_disposition_parent'); END;",
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Limits;
    use rusqlite::params;

    fn parent(s: &Store, binding: i64, generation: i64) -> i64 {
        let endpoint = format!("endpoint_{binding}");
        s.db.execute(
            "INSERT OR IGNORE INTO endpoints(id,generation,recipient) VALUES(?1,1,'ag_recipient')",
            [&endpoint],
        )
        .unwrap();
        s.db.execute("INSERT INTO inbound(endpoint,generation,external_id,recipient,body,digest,pending,core_id) VALUES(?1,?2,lower(hex(randomblob(16))),'ag_recipient','synthetic',zeroblob(32),0,'msg_final')", params![endpoint,generation]).unwrap();
        s.db.last_insert_rowid()
    }

    fn insert(s: &Store, job: i64, key: i64, time: i64) -> rusqlite::Result<usize> {
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&key.to_be_bytes());
        s.db.execute("INSERT INTO telegram_terminal_dispositions(delivery_key,receipt_digest,job,first_seen) VALUES(?1,zeroblob(32),?2,?3)", params![digest.as_slice(),job,time])
    }

    #[test]
    fn migration_preserves_history_and_declared_parent_identity() {
        let mut s = Store::memory(Limits::default()).unwrap();
        let job = parent(&s, 1, 1);
        initialize(&mut s).unwrap();
        insert(&s, job, 1, 10).unwrap();
        initialize(&mut s).unwrap();
        s.db.execute_batch("VACUUM").unwrap();
        let row: (i64, String) = s.db.query_row("SELECT d.job,i.core_id FROM telegram_terminal_dispositions d JOIN inbound i ON i.id=d.job", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(row, (job, "msg_final".into()));
        assert!(s
            .db
            .execute("DELETE FROM inbound WHERE id=?1", [job])
            .is_err());
        assert!(s
            .db
            .execute(
                "UPDATE inbound SET core_id='msg_changed' WHERE id=?1",
                [job]
            )
            .is_err());
        assert!(s
            .db
            .execute("UPDATE telegram_terminal_dispositions SET job=job+1", [])
            .is_err());
    }

    #[test]
    fn schema_enforces_hash_width_scalar_states_and_parent_existence() {
        let mut s = Store::memory(Limits::default()).unwrap();
        initialize(&mut s).unwrap();
        let job = parent(&s, 1, 1);
        for hash in ["zeroblob(31)", "zeroblob(33)", "printf('%032d',0)"] {
            let sql = format!("INSERT INTO telegram_terminal_dispositions(delivery_key,receipt_digest,job,first_seen) VALUES({hash},zeroblob(32),?1,0)");
            assert!(s.db.execute(&sql, [job]).is_err());
        }
        assert!(insert(&s, job + 1, 1, 0).is_err());
        assert!(insert(&s, job, 1, -1).is_err());
        insert(&s, job, 1, 0).unwrap();
        for sql in [
            "UPDATE telegram_terminal_dispositions SET acked=2",
            "UPDATE telegram_terminal_dispositions SET conflict=-1",
            "UPDATE telegram_terminal_dispositions SET receipt_digest=zeroblob(31)",
        ] {
            assert!(s.db.execute(sql, []).is_err());
        }
        let bytes: usize = s.db.query_row("SELECT length(delivery_key)+length(receipt_digest)+5*8 FROM telegram_terminal_dispositions", [], |r| r.get(0)).unwrap();
        assert_eq!(bytes, LOGICAL_ROW_BYTES);
        assert!(bytes <= 256);
    }

    #[test]
    fn caps_span_generations_and_bindings_without_automatic_eviction() {
        let mut s = Store::memory(Limits::default()).unwrap();
        initialize(&mut s).unwrap();
        for n in 0..GLOBAL_ROWS {
            let binding = n / PER_BINDING_ROWS;
            let job = parent(&s, binding, n + 1);
            insert(&s, job, n, n).unwrap();
            if n == PER_BINDING_ROWS - 1 {
                let overflow = parent(&s, binding, 999);
                assert!(insert(&s, overflow, 9999, 0).is_err());
            }
        }
        let overflow = parent(&s, 100, 1);
        assert!(insert(&s, overflow, 10000, 0).is_err());
        let count: i64 =
            s.db.query_row(
                "SELECT count(*) FROM telegram_terminal_dispositions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, GLOBAL_ROWS);
    }

    #[test]
    fn declared_sequence_survives_clock_steps_and_deleted_maximum() {
        let mut s = Store::memory(Limits::default()).unwrap();
        initialize(&mut s).unwrap();
        let job = parent(&s, 1, 1);
        insert(&s, job, 1, 100).unwrap();
        insert(&s, job, 2, 100).unwrap();
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1 WHERE ordinal=2",
            [],
        )
        .unwrap();
        s.db.execute(
            "DELETE FROM telegram_terminal_dispositions WHERE ordinal=2 AND acked=1",
            [],
        )
        .unwrap();
        insert(&s, job, 3, 1).unwrap();
        let ordinals: Vec<i64> =
            s.db.prepare("SELECT ordinal FROM telegram_terminal_dispositions ORDER BY ordinal")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
        assert_eq!(ordinals, vec![1, 3]);
    }
}
