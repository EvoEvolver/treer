//! Durable bounded fragment inbox. Outer Core sender authentication happens first.
use crate::protocol::{Assembly, Direction, Envelope, Fragment};
use crate::store::{available, capacity, Error, Store};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};

pub const INCOMPLETE_TTL: i64 = 600;
pub const TOMBSTONE_TTL: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, PartialEq, Eq)]
pub enum FragmentReceipt {
    Pending,
    Ready,
    Duplicate,
    /// An incomplete transfer expired; the durable tombstone permits terminal ack.
    Expired,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cleanup {
    pub expired: usize,
    pub pruned: usize,
}

fn direction_id(direction: Direction) -> i64 {
    if direction == Direction::Outbound {
        0
    } else {
        1
    }
}

impl Store {
    /// Caller must first authenticate the outer sender against its pinned binding.
    /// A successful return means the part (or terminal expiry) is durable; only
    /// then may it ack the corresponding Core delivery. Ready parts remain queued
    /// until finish_transfer; caller must drain them after restart as well.
    pub fn save_fragment(
        &mut self,
        endpoint: &str,
        generation: i64,
        direction: Direction,
        wire: &str,
        now: i64,
    ) -> Result<FragmentReceipt, Error> {
        let frame = Fragment::parse(wire).map_err(|_| Error::Invalid)?;
        if now < 0
            || generation <= 0
            || frame.generation != generation as u64
            || frame.direction != direction
        {
            return Err(Error::Invalid);
        }
        let expires = now.checked_add(INCOMPLETE_TTL).ok_or(Error::Invalid)?;
        let dir = direction_id(direction);
        let transaction = self.db.transaction()?;
        available(&transaction, endpoint, generation)?;
        let previous = transaction
            .query_row(
                "SELECT digest,parts,state,expires FROM transfers
             WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4",
                params![endpoint, generation, dir, frame.transfer_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u16>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let new = previous.is_none();
        if let Some((digest, parts, state, expiry)) = previous {
            if digest != frame.payload_digest || parts != frame.parts {
                return Err(Error::Conflict);
            }
            if state == 3 || (state == 0 && expiry <= now) {
                transaction.execute(
                    "UPDATE transfers SET state=3,terminal_at=COALESCE(terminal_at,?5)
                     WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4",
                    params![endpoint, generation, dir, frame.transfer_id, now],
                )?;
                transaction.execute(
                    "UPDATE fragments SET wire=NULL
                     WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4",
                    params![endpoint, generation, dir, frame.transfer_id],
                )?;
                transaction.commit()?;
                return Ok(FragmentReceipt::Expired);
            }
            let old: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT digest FROM fragments
                 WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4 AND part=?5",
                    params![endpoint, generation, dir, frame.transfer_id, frame.part],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old {
                if old != Sha256::digest(wire.as_bytes()).to_vec() {
                    return Err(Error::Conflict);
                }
                return Ok(if state == 1 {
                    FragmentReceipt::Ready
                } else {
                    FragmentReceipt::Duplicate
                });
            }
            if state != 0 {
                return Err(Error::Conflict);
            }
        }
        capacity(
            &transaction,
            self.limits,
            endpoint,
            wire.len(),
            i64::from(new),
        )?;
        if new {
            transaction.execute(
                "INSERT INTO transfers(endpoint,generation,direction,transfer,digest,parts,expires)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    endpoint,
                    generation,
                    dir,
                    frame.transfer_id,
                    frame.payload_digest,
                    frame.parts,
                    expires
                ],
            )?;
        }
        // Validate the complete set before committing the final part. Partial
        // sets survive crashes; poisoning a final part never replaces good data.
        let mut assembly = Assembly::new(&frame, endpoint, generation as u64, direction)
            .map_err(|_| Error::Invalid)?;
        let mut statement = transaction.prepare(
            "SELECT wire FROM fragments WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4 ORDER BY part")?;
        let wires = statement
            .query_map(params![endpoint, generation, dir, frame.transfer_id], |r| {
                r.get::<_, String>(0)
            })?;
        for previous in wires {
            let old = Fragment::parse(&previous?).map_err(|_| Error::Invalid)?;
            assembly.push(&old).map_err(|_| Error::Conflict)?;
        }
        drop(statement);
        let ready = assembly
            .push(&frame)
            .map_err(|_| Error::Conflict)?
            .is_some();
        transaction.execute(
            "INSERT INTO fragments(endpoint,generation,direction,transfer,part,wire,digest)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                endpoint,
                generation,
                dir,
                frame.transfer_id,
                frame.part,
                wire,
                Sha256::digest(wire.as_bytes()).to_vec()
            ],
        )?;
        if ready {
            transaction.execute(
                "UPDATE transfers SET state=1 WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4",
                params![endpoint,generation,dir,frame.transfer_id])?;
        }
        transaction.commit()?;
        Ok(if ready {
            FragmentReceipt::Ready
        } else {
            FragmentReceipt::Pending
        })
    }

    /// Bounded recovery/drain discovery. No payloads in status/log responses.
    pub fn ready_transfers(
        &self,
        endpoint: &str,
        generation: i64,
        direction: Direction,
    ) -> Result<Vec<String>, Error> {
        let mut statement = self.db.prepare(
            "SELECT transfer FROM transfers WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND state=1
             ORDER BY expires,transfer LIMIT 100")?;
        let rows = statement.query_map(
            params![endpoint, generation, direction_id(direction)],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn transfer_payload(
        &self,
        endpoint: &str,
        generation: i64,
        direction: Direction,
        transfer: &str,
    ) -> Result<Envelope, Error> {
        let mut statement = self.db.prepare(
            "SELECT f.wire FROM fragments f JOIN transfers t USING(endpoint,generation,direction,transfer)
             WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4 AND t.state=1 ORDER BY part")?;
        let rows = statement.query_map(
            params![endpoint, generation, direction_id(direction), transfer],
            |r| r.get::<_, String>(0),
        )?;
        let mut assembly = None;
        let mut envelope = None;
        for wire in rows {
            let frame = Fragment::parse(&wire?).map_err(|_| Error::Invalid)?;
            let parts = match assembly.as_mut() {
                Some(parts) => parts,
                None => assembly.insert(
                    Assembly::new(&frame, endpoint, generation as u64, direction)
                        .map_err(|_| Error::Invalid)?,
                ),
            };
            envelope = parts.push(&frame).map_err(|_| Error::Conflict)?;
        }
        envelope.ok_or(Error::Unavailable)
    }

    /// Call only after the downstream durable queue/send has accepted this exact
    /// envelope with its stable key. Keeps per-part hashes, removes plaintext.
    pub fn finish_transfer(
        &mut self,
        endpoint: &str,
        generation: i64,
        direction: Direction,
        transfer: &str,
        now: i64,
    ) -> Result<(), Error> {
        if now < 0 {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        let changed = transaction.execute(
            "UPDATE transfers SET state=2,terminal_at=COALESCE(terminal_at,?5)
             WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4 AND state IN (1,2)",
            params![endpoint,generation,direction_id(direction),transfer,now])?;
        if changed != 1 {
            return Err(Error::Unavailable);
        }
        transaction.execute(
            "UPDATE fragments SET wire=NULL WHERE endpoint=?1 AND generation=?2 AND direction=?3 AND transfer=?4",
            params![endpoint,generation,direction_id(direction),transfer])?;
        transaction.commit()?;
        Ok(())
    }

    /// Daemons must call periodically and before capacity retries. Duplicates do
    /// not refresh expiry; expiry is a terminal, visible failure until pruning.
    pub fn cleanup_transfers(&mut self, now: i64) -> Result<Cleanup, Error> {
        if now < TOMBSTONE_TTL {
            return Err(Error::Invalid);
        }
        let transaction = self.db.transaction()?;
        let expired = transaction.execute(
            "UPDATE transfers SET state=3,terminal_at=?1 WHERE state=0 AND expires<=?1",
            [now],
        )?;
        transaction.execute(
            "UPDATE fragments SET wire=NULL WHERE EXISTS
             (SELECT 1 FROM transfers t WHERE t.endpoint=fragments.endpoint AND t.generation=fragments.generation
               AND t.direction=fragments.direction AND t.transfer=fragments.transfer AND t.state=3)", [])?;
        let pruned = transaction.execute(
            "DELETE FROM transfers WHERE state IN (2,3) AND terminal_at<=?1",
            [now - TOMBSTONE_TTL],
        )?;
        transaction.commit()?;
        Ok(Cleanup { expired, pruned })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Sender, SenderKind};
    use crate::store::Limits;
    const NOW: i64 = 5_000_000;

    fn frames(id: &str) -> Vec<String> {
        Envelope::Outbound {
            delivery_id: id.into(),
            message_id: format!("msg_{id}"),
            original_sender: Sender {
                kind: SenderKind::Human,
                id: "human_fixture".into(),
            },
            body: "界".repeat(10_000),
            context_ids: vec![],
        }
        .fragments("ag_endpoint", 1)
        .unwrap()
    }
    fn fixture(limits: Limits) -> Store {
        let mut store = Store::memory(limits).unwrap();
        store
            .register_paused("ag_endpoint", 1, "ag_target")
            .unwrap();
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        store
    }
    fn save(store: &mut Store, wire: &str, now: i64) -> Result<FragmentReceipt, Error> {
        store.save_fragment("ag_endpoint", 1, Direction::Outbound, wire, now)
    }
    #[test]
    fn fragment_sets_share_queue_capacity_and_can_finish_at_count_cap() {
        let mut store = fixture(Limits {
            endpoint_count: 1,
            ..Limits::default()
        });
        let first = frames("del_first");
        let other = frames("del_other");
        assert_eq!(
            save(&mut store, &first[0], NOW).unwrap(),
            FragmentReceipt::Pending
        );
        assert!(matches!(save(&mut store, &other[0], NOW), Err(Error::Full)));
        assert!(matches!(
            store.submit("ag_endpoint", 1, "d", "m", b"payload"),
            Err(Error::Full)
        ));
        assert_eq!(
            save(&mut store, &first[1], NOW).unwrap(),
            FragmentReceipt::Ready
        );
        assert_eq!(
            save(&mut store, &first[1], NOW).unwrap(),
            FragmentReceipt::Ready
        );
        let transfer = Fragment::parse(&first[0]).unwrap().transfer_id;
        assert_eq!(
            store
                .ready_transfers("ag_endpoint", 1, Direction::Outbound)
                .unwrap(),
            vec![transfer.clone()]
        );
        assert!(store
            .ready_transfers("ag_endpoint", 2, Direction::Outbound)
            .unwrap()
            .is_empty());
        let payload = store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &transfer)
            .unwrap();
        assert_eq!(payload.source_id(), "del_first");
        // Ready work is not expired while awaiting downstream processing.
        assert_eq!(
            store
                .cleanup_transfers(NOW + INCOMPLETE_TTL)
                .unwrap()
                .expired,
            0
        );
        store
            .finish_transfer(
                "ag_endpoint",
                1,
                Direction::Outbound,
                &transfer,
                NOW + INCOMPLETE_TTL,
            )
            .unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW + INCOMPLETE_TTL).unwrap(),
            FragmentReceipt::Duplicate
        );
        assert_eq!(
            save(&mut store, &other[0], NOW + INCOMPLETE_TTL).unwrap(),
            FragmentReceipt::Pending
        );
    }

    #[test]
    fn duplicates_do_not_extend_expiry_or_add_capacity_and_tombstones_prune() {
        let mut store = fixture(Limits {
            endpoint_count: 1,
            ..Limits::default()
        });
        let first = frames("del_first");
        save(&mut store, &first[0], NOW).unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW + INCOMPLETE_TTL - 1).unwrap(),
            FragmentReceipt::Duplicate
        );
        assert_eq!(
            save(&mut store, &first[1], NOW + INCOMPLETE_TTL).unwrap(),
            FragmentReceipt::Expired
        );
        assert_eq!(
            store
                .db
                .query_row(
                    "SELECT COUNT(*) FROM fragments WHERE wire IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        // Expired tombstones do not occupy pending queue capacity.
        store
            .submit("ag_endpoint", 1, "d", "m", b"payload")
            .unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW + INCOMPLETE_TTL + 1).unwrap(),
            FragmentReceipt::Expired
        );
        assert_eq!(
            store
                .cleanup_transfers(NOW + INCOMPLETE_TTL + TOMBSTONE_TTL - 1)
                .unwrap()
                .pruned,
            0
        );
        assert_eq!(
            store
                .cleanup_transfers(NOW + INCOMPLETE_TTL + TOMBSTONE_TTL)
                .unwrap()
                .pruned,
            1
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn periodic_cleanup_expires_sets_and_preserves_ready_work() {
        let mut store = fixture(Limits::default());
        let first = frames("del_first");
        let other = frames("del_other");
        save(&mut store, &first[0], NOW).unwrap();
        for frame in &other {
            save(&mut store, frame, NOW).unwrap();
        }
        assert_eq!(
            store.cleanup_transfers(NOW + INCOMPLETE_TTL).unwrap(),
            Cleanup {
                expired: 1,
                pruned: 0
            }
        );
        let transfer = Fragment::parse(&other[0]).unwrap().transfer_id;
        assert!(store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &transfer)
            .is_ok());
    }

    #[test]
    fn capacity_and_conflict_rollbacks_never_replace_durable_parts() {
        let first = frames("del_first");
        let mut store = fixture(Limits {
            endpoint_bytes: first[0].len() as i64,
            ..Limits::default()
        });
        save(&mut store, &first[0], NOW).unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW).unwrap(),
            FragmentReceipt::Duplicate
        );
        assert!(matches!(save(&mut store, &first[1], NOW), Err(Error::Full)));
        let mut changed = Fragment::parse(&first[0]).unwrap();
        changed.data = "YQ==".into();
        assert!(matches!(
            save(&mut store, &serde_json::to_string(&changed).unwrap(), NOW),
            Err(Error::Conflict)
        ));
        assert_eq!(
            store
                .db
                .query_row("SELECT wire FROM fragments", [], |r| r.get::<_, String>(0))
                .unwrap(),
            first[0]
        );
        changed.generation = 2;
        assert!(matches!(
            save(&mut store, &serde_json::to_string(&changed).unwrap(), NOW),
            Err(Error::Invalid)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn partial_sets_and_completed_dedup_survive_reopen() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = base.join("state");
        let first = frames("del_first");
        let transfer = Fragment::parse(&first[0]).unwrap().transfer_id;
        let mut store = Store::open(&path, Limits::default()).unwrap();
        store
            .register_paused("ag_endpoint", 1, "ag_target")
            .unwrap();
        store
            .db
            .execute("UPDATE endpoints SET paused=0", [])
            .unwrap();
        save(&mut store, &first[1], NOW).unwrap();
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW + 1).unwrap(),
            FragmentReceipt::Ready
        );
        assert_eq!(
            store
                .transfer_payload("ag_endpoint", 1, Direction::Outbound, &transfer)
                .unwrap()
                .source_id(),
            "del_first"
        );
        store
            .finish_transfer("ag_endpoint", 1, Direction::Outbound, &transfer, NOW + 2)
            .unwrap();
        drop(store);
        let mut store = Store::open(&path, Limits::default()).unwrap();
        assert_eq!(
            save(&mut store, &first[0], NOW + 3).unwrap(),
            FragmentReceipt::Duplicate
        );
        assert!(store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &transfer)
            .is_err());
        assert_eq!(
            store
                .cleanup_transfers(NOW + 2 + TOMBSTONE_TTL)
                .unwrap()
                .pruned,
            1
        );
    }
}
