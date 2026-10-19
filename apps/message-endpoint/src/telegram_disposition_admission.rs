//! Local ledger mutation primitive, deliberately not an ACK capability.
//! The caller must own an IMMEDIATE transaction and compose current hub, mode,
//! receipt authorization, counter and durable recovery holds before committing.
use crate::{
    store::Error,
    telegram_disposition_identity::Identity,
    telegram_dispositions::{ACKED_FLOOR, GLOBAL_ROWS, PER_BINDING_ROWS},
};
use rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Inserted,
    Replay,
    Conflict,
    Full,
}

/// This function neither commits nor acknowledges, and does not update counters
/// or pause a binding. Every outcome remains subject to the enclosing operation.
/// In particular Conflict/Full require a durable owner-recovery hold, not ACK.
pub fn record_in_transaction(
    tx: &Transaction<'_>,
    identity: &Identity,
    now: i64,
) -> Result<Admission, Error> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let version: i64 = tx.query_row(
        "SELECT version FROM telegram_terminal_disposition_schema WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if version != 1 {
        return Err(Error::Invalid);
    }
    let (job, endpoint, generation, recipient) = identity.parent();
    let terminal: Option<String> = tx
        .query_row(
            "SELECT core_id FROM inbound WHERE id=?1 AND endpoint=?2 AND generation=?3
             AND recipient=?4 AND pending=0 AND core_id IS NOT NULL",
            params![job, endpoint, generation, recipient],
            |r| r.get(0),
        )
        .optional()?;
    if terminal
        .as_deref()
        .is_none_or(|id| id == identity.asserted_final())
    {
        return Err(Error::Invalid);
    }
    let (key, digest) = identity.storage_hashes();
    let previous: Option<(Vec<u8>, i64, bool)> = tx
        .query_row(
            "SELECT receipt_digest,job,conflict FROM telegram_terminal_dispositions
             WHERE delivery_key=?1",
            [key.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((stored, parent, conflict)) = previous {
        if parent == job && stored == digest.as_slice() && !conflict {
            return Ok(Admission::Replay);
        }
        // The original identity and ack state are immutable. A conflict latches
        // even if a later retry presents the original receipt again.
        tx.execute(
            "UPDATE telegram_terminal_dispositions SET conflict=1 WHERE delivery_key=?1",
            [key.as_slice()],
        )?;
        return Ok(Admission::Conflict);
    }
    let local: i64 = tx.query_row(
        "SELECT count(*) FROM telegram_terminal_dispositions d
         JOIN inbound i ON i.id=d.job WHERE i.endpoint=?1",
        [endpoint],
        |r| r.get(0),
    )?;
    let global: i64 = tx.query_row(
        "SELECT count(*) FROM telegram_terminal_dispositions",
        [],
        |r| r.get(0),
    )?;
    if local >= PER_BINDING_ROWS || global >= GLOBAL_ROWS {
        let acked: i64 = tx.query_row(
            "SELECT count(*) FROM telegram_terminal_dispositions d
             JOIN inbound i ON i.id=d.job WHERE i.endpoint=?1 AND d.acked=1 AND d.conflict=0",
            [endpoint],
            |r| r.get(0),
        )?;
        if acked <= ACKED_FLOOR {
            return Ok(Admission::Full);
        }
        // Only this incoming binding's oldest confirmed, non-conflicting row.
        // Deleting one suffices for the fixed caps; an insert failure must roll
        // back the enclosing transaction, including this cleanup.
        tx.execute(
            "DELETE FROM telegram_terminal_dispositions WHERE ordinal=(
             SELECT d.ordinal FROM telegram_terminal_dispositions d
             JOIN inbound i ON i.id=d.job
             WHERE i.endpoint=?1 AND d.acked=1 AND d.conflict=0
             ORDER BY d.ordinal LIMIT 1)",
            [endpoint],
        )?;
    }
    tx.execute(
        "INSERT INTO telegram_terminal_dispositions(delivery_key,receipt_digest,job,first_seen)
         VALUES(?1,?2,?3,?4)",
        params![key.as_slice(), digest.as_slice(), job, now],
    )?;
    Ok(Admission::Inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        store::{Limits, Store},
        telegram_dispositions::initialize,
    };
    use rusqlite::TransactionBehavior;

    fn fixture() -> Store {
        let mut s = Store::memory(Limits::default()).unwrap();
        initialize(&mut s).unwrap();
        for n in 1..=9 {
            let endpoint = format!("endpoint_{n}");
            s.db.execute(
                "INSERT INTO endpoints(id,generation,recipient) VALUES(?1,1,'ag_recipient')",
                [&endpoint],
            )
            .unwrap();
            s.db.execute("INSERT INTO inbound(id,endpoint,generation,external_id,recipient,body,digest,pending,core_id) VALUES(?1,?2,1,'one','ag_recipient','synthetic',zeroblob(32),0,'msg_final')", params![n,endpoint]).unwrap();
        }
        s
    }
    fn identity(job: i64, key: u16, digest: u8) -> Identity {
        Identity::fixture(job, &format!("endpoint_{job}"), 1, key, digest)
    }
    fn record(s: &mut Store, id: &Identity) -> Admission {
        let tx =
            s.db.transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
        let result = record_in_transaction(&tx, id, 1).unwrap();
        tx.commit().unwrap();
        result
    }
    fn count(s: &Store) -> i64 {
        s.db.query_row(
            "SELECT count(*) FROM telegram_terminal_dispositions",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }
    #[test]
    fn replay_and_changed_parent_or_digest_latch_without_overwriting() {
        let mut s = fixture();
        assert_eq!(record(&mut s, &identity(1, 1, 1)), Admission::Inserted);
        assert_eq!(record(&mut s, &identity(1, 1, 1)), Admission::Replay);
        assert_eq!(record(&mut s, &identity(2, 1, 1)), Admission::Conflict);
        assert_eq!(record(&mut s, &identity(1, 1, 1)), Admission::Conflict);
        assert_eq!(record(&mut s, &identity(1, 2, 1)), Admission::Inserted);
        assert_eq!(record(&mut s, &identity(1, 2, 2)), Admission::Conflict);
        let rows: Vec<(i64, Vec<u8>, bool)> =
            s.db.prepare("SELECT job,receipt_digest,conflict FROM telegram_terminal_dispositions")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
        assert_eq!(rows, vec![(1, vec![1; 32], true), (1, vec![1; 32], true)]);
    }
    #[test]
    fn parent_scope_pending_equal_final_and_orphan_are_refused() {
        let mut s = fixture();
        for sql in [
            "UPDATE inbound SET pending=1 WHERE id=1",
            "UPDATE inbound SET generation=2 WHERE id=1",
            "UPDATE inbound SET recipient='ag_other' WHERE id=1",
            "UPDATE inbound SET core_id='msg_other' WHERE id=1",
            "DELETE FROM inbound WHERE id=1",
        ] {
            let tx =
                s.db.transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
            tx.execute(sql, []).unwrap();
            assert!(record_in_transaction(&tx, &identity(1, 1, 1), 1).is_err());
            tx.rollback().unwrap();
        }
        assert_eq!(count(&s), 0);
    }
    #[test]
    fn local_capacity_prunes_oldest_acked_only_and_keeps_floor() {
        let mut s = fixture();
        for key in 0..32 {
            assert_eq!(
                record(&mut s, &identity(1, key, key as u8)),
                Admission::Inserted
            );
        }
        assert_eq!(record(&mut s, &identity(1, 32, 32)), Admission::Full);
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1 WHERE ordinal<=8",
            [],
        )
        .unwrap();
        assert_eq!(record(&mut s, &identity(1, 32, 32)), Admission::Full);
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1 WHERE ordinal=9",
            [],
        )
        .unwrap();
        assert_eq!(record(&mut s, &identity(1, 32, 32)), Admission::Inserted);
        assert_eq!(count(&s), 32);
        let (oldest, acked): (i64, i64) =
            s.db.query_row(
                "SELECT min(ordinal),sum(acked) FROM telegram_terminal_dispositions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((oldest, acked), (2, 8));
    }
    #[test]
    fn global_capacity_never_evicts_other_binding_or_conflict_rows() {
        let mut s = fixture();
        for job in 1..=8 {
            for key in 0..32 {
                let key = ((job - 1) * 32 + key) as u16;
                assert_eq!(
                    record(&mut s, &identity(job, key, key as u8)),
                    Admission::Inserted
                );
            }
        }
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1 WHERE job=1",
            [],
        )
        .unwrap();
        assert_eq!(record(&mut s, &identity(9, 256, 0)), Admission::Full);
        assert_eq!(count(&s), GLOBAL_ROWS);
        // Capacity can only be recovered from the incoming binding itself.
        assert_eq!(record(&mut s, &identity(1, 256, 0)), Admission::Inserted);
        assert_eq!(count(&s), GLOBAL_ROWS);
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET conflict=1 WHERE job=1",
            [],
        )
        .unwrap();
        assert_eq!(record(&mut s, &identity(1, 257, 0)), Admission::Full);
        assert_eq!(count(&s), GLOBAL_ROWS);
    }
    #[test]
    fn failed_commit_rolls_back_prune_and_insert() {
        let mut s = fixture();
        for key in 0..32 {
            record(&mut s, &identity(1, key, key as u8));
        }
        s.db.execute(
            "UPDATE telegram_terminal_dispositions SET acked=1 WHERE ordinal<=9",
            [],
        )
        .unwrap();
        s.db.commit_hook(Some(|| true));
        let tx =
            s.db.transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
        assert_eq!(
            record_in_transaction(&tx, &identity(1, 32, 32), 1).unwrap(),
            Admission::Inserted
        );
        assert!(tx.commit().is_err());
        s.db.commit_hook(None::<fn() -> bool>);
        assert_eq!(count(&s), 32);
        assert_eq!(record(&mut s, &identity(1, 0, 0)), Admission::Replay);
    }
}
