//! Bounded per-binding mailbox holds. Core ack follows commit; overflow is an
//! explicit terminal rejection, not silent durable delivery. No network here.
use crate::{
    core::Delivery,
    hub::{Directory, Error, Hub},
    protocol::{Direction, Fragment, MAX_BODY},
    relay::Mailbox,
    store::{valid_id, Error as StorageError},
    transfers::FragmentReceipt,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
const HOLD_COUNT: i64 = 20;
const HOLD_BYTES: i64 = 256 * 1024;
const GLOBAL_COUNT: i64 = 400;
const GLOBAL_BYTES: i64 = 5 * 1024 * 1024;
const HOLD_TTL: i64 = 3600;
const RECEIPT_CAP: i64 = 1000;
const GLOBAL_RECEIPTS: i64 = 10_000;

#[derive(Serialize)]
pub struct HoldSummary {
    pub held_fragments: i64,
    pub held_bytes: i64,
    /// Attempts, not distinct messages after receipt capacity is exhausted.
    pub capacity_rejections: i64,
    pub expired_fragments: i64,
    pub invalid_fragments: i64,
    pub receipts: i64,
    pub global_capacity_exceeded: bool,
}
#[derive(Serialize)]
pub struct CapacityReceipt {
    pub delivery_id: String,
    pub message_id: String,
    pub generation: i64,
    pub reason: String,
    pub at: i64,
    /// Fragment admission, not completion of the original external message.
    pub recovered_at: Option<i64>,
}

/// Capped metadata receipt + counter, committed before removing plaintext/acking.
fn reject(
    tx: &Transaction<'_>,
    endpoint: &str,
    generation: i64,
    delivery: &str,
    message: &str,
    reason: &str,
    now: i64,
) -> Result<(), Error> {
    let prior:Option<String>=tx.query_row("SELECT message FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2 AND delivery=?3",params![endpoint,generation,delivery],|r|r.get(0)).optional()?;
    if let Some(prior) = prior {
        return if prior == message {
            Ok(())
        } else {
            Err(Error::Binding)
        };
    }
    let (capacity, expired, invalid) = match reason {
        "hold_expired" => (0, 1, 0),
        "owner_discarded_hold" => (0, 0, 0),
        "invalid_held_fragment" | "generation_changed" => (0, 0, 1),
        "endpoint_hold_full" | "hub_hold_full" => (1, 0, 0),
        _ => return Err(Error::Binding),
    };
    tx.execute(
        "INSERT OR IGNORE INTO hub_hold_counters(endpoint,generation) VALUES(?1,?2)",
        params![endpoint, generation],
    )?;
    tx.execute("UPDATE hub_hold_counters SET capacity=CASE WHEN capacity<9223372036854775807 THEN capacity+?3 ELSE capacity END,expired=CASE WHEN expired<9223372036854775807 THEN expired+?4 ELSE expired END,invalid=CASE WHEN invalid<9223372036854775807 THEN invalid+?5 ELSE invalid END WHERE endpoint=?1 AND generation=?2",params![endpoint,generation,capacity,expired,invalid])?;
    if reason == "hub_hold_full" {
        tx.execute(
            "UPDATE hub_hold_health SET capacity_exceeded=1 WHERE singleton=1",
            [],
        )?;
    }
    let (global, local): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(endpoint=?1),0) FROM hub_capacity_receipts",
        [endpoint],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if global < GLOBAL_RECEIPTS && local < RECEIPT_CAP {
        tx.execute("INSERT INTO hub_capacity_receipts(endpoint,generation,delivery,message,reason,at) VALUES(?1,?2,?3,?4,?5,?6)",params![endpoint,generation,delivery,message,reason,now])?;
    }
    Ok(())
}

/// A discarded held fragment leaves a metadata tombstone, never refetchable.
/// Refuse rather than lose dedup evidence when the receipt budget cannot fit.
#[cfg(any(test, feature = "hub"))]
pub(crate) fn discard_for_resume(
    tx: &Transaction<'_>,
    endpoint: &str,
    generation: i64,
    now: i64,
) -> Result<(), Error> {
    let mut q=tx.prepare("SELECT delivery,message FROM hub_holds WHERE endpoint=?1 AND generation=?2 ORDER BY delivery LIMIT 21")?;
    let rows = q
        .query_map(params![endpoint, generation], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(q);
    let (global, local): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(endpoint=?1),0) FROM hub_capacity_receipts",
        [endpoint],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rows.len() > 20
        || global + rows.len() as i64 > GLOBAL_RECEIPTS
        || local + rows.len() as i64 > RECEIPT_CAP
    {
        return Err(StorageError::Full.into());
    }
    for (delivery, message) in rows {
        reject(
            tx,
            endpoint,
            generation,
            &delivery,
            &message,
            "owner_discarded_hold",
            now,
        )?;
    }
    tx.execute(
        "DELETE FROM hub_holds WHERE endpoint=?1 AND generation=?2",
        params![endpoint, generation],
    )?;
    Ok(())
}

impl<M: Mailbox + Directory> Hub<M> {
    pub(crate) fn hold_schema(&self) -> Result<(), Error> {
        self.store.db.execute_batch("CREATE TABLE IF NOT EXISTS hub_holds(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,delivery TEXT NOT NULL,message TEXT NOT NULL,wire TEXT NOT NULL,created_at INTEGER NOT NULL,expires INTEGER NOT NULL,PRIMARY KEY(endpoint,generation,delivery));
        CREATE TABLE IF NOT EXISTS hub_hold_counters(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,capacity INTEGER NOT NULL DEFAULT 0,expired INTEGER NOT NULL DEFAULT 0,invalid INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(endpoint,generation));
        CREATE TABLE IF NOT EXISTS hub_capacity_receipts(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,delivery TEXT NOT NULL,message TEXT NOT NULL,reason TEXT NOT NULL,at INTEGER NOT NULL,recovered_at INTEGER,PRIMARY KEY(endpoint,generation,delivery));
        CREATE TABLE IF NOT EXISTS hub_hold_health(singleton INTEGER PRIMARY KEY CHECK(singleton=1),capacity_exceeded INTEGER NOT NULL DEFAULT 0);
        INSERT OR IGNORE INTO hub_hold_health(singleton) VALUES(1);")?;
        Ok(())
    }
    /// Called only after the hub's own recipient and bound outer Agent checks.
    /// Success means durable held fragment OR visible capacity rejection, and
    /// permits Core ack. No mutation of the active fragment/failure quotas.
    pub(crate) fn hold_delivery(&mut self, delivery: &Delivery, now: i64) -> Result<(), Error> {
        let endpoint = delivery.message.sender.id.as_str();
        let generation = self.view(endpoint)?.binding.generation;
        if now < 0
            || !valid_id(&delivery.delivery_id)
            || !valid_id(&delivery.message.message_id)
            || delivery.message.body.len() > MAX_BODY
        {
            return Err(Error::Binding);
        }
        let valid_frame = Fragment::parse(&delivery.message.body).is_ok_and(|frame| {
            frame.generation == generation as u64 && frame.direction == Direction::Outbound
        });
        let expires = now.checked_add(HOLD_TTL).ok_or(Error::Binding)?;
        let tx = self.store.db.transaction()?;
        if !valid_frame {
            reject(
                &tx,
                endpoint,
                generation,
                &delivery.delivery_id,
                &delivery.message.message_id,
                "invalid_held_fragment",
                now,
            )?;
            tx.commit()?;
            return Ok(());
        }
        let prior:Option<(String,String)>=tx.query_row("SELECT message,wire FROM hub_holds WHERE endpoint=?1 AND generation=?2 AND delivery=?3",params![endpoint,generation,delivery.delivery_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((message, wire)) = prior {
            return if message == delivery.message.message_id && wire == delivery.message.body {
                Ok(())
            } else {
                Err(Error::Binding)
            };
        }
        let rejected:Option<String>=tx.query_row("SELECT message FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2 AND delivery=?3",params![endpoint,generation,delivery.delivery_id],|r|r.get(0)).optional()?;
        if let Some(message) = rejected {
            return if message == delivery.message.message_id {
                Ok(())
            } else {
                Err(Error::Binding)
            };
        }
        let (global,bytes,local,local_bytes):(i64,i64,i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(wire AS BLOB))),0),COALESCE(SUM(endpoint=?1),0),COALESCE(SUM(CASE WHEN endpoint=?1 THEN length(CAST(wire AS BLOB)) ELSE 0 END),0) FROM hub_holds",[endpoint],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let reason = if local >= HOLD_COUNT
            || local_bytes + delivery.message.body.len() as i64 > HOLD_BYTES
        {
            Some("endpoint_hold_full")
        } else if global >= GLOBAL_COUNT
            || bytes + delivery.message.body.len() as i64 > GLOBAL_BYTES
        {
            Some("hub_hold_full")
        } else {
            None
        };
        if let Some(reason) = reason {
            reject(
                &tx,
                endpoint,
                generation,
                &delivery.delivery_id,
                &delivery.message.message_id,
                reason,
                now,
            )?;
        } else {
            tx.execute("INSERT INTO hub_holds(endpoint,generation,delivery,message,wire,created_at,expires) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![endpoint,generation,delivery.delivery_id,delivery.message.message_id,delivery.message.body,now,expires])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Bounded FIFO promotion after fresh activation. Validation/capacity checks
    /// use the normal fragment store. Delete follows its durable commit, so a
    /// crash replays the same hashes instead of creating a second transfer.
    pub fn promote_held(&mut self, endpoint: &str, now: i64) -> Result<usize, Error> {
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let view = self.active(endpoint, now)?;
        let mut q=self.store.db.prepare("SELECT delivery,message,wire,expires FROM hub_holds WHERE endpoint=?1 AND generation=?2 ORDER BY created_at,rowid LIMIT 10")?;
        let rows = q
            .query_map(params![endpoint, view.binding.generation], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let mut count = 0;
        for (delivery, message, wire, expires) in rows {
            let result = if expires <= now {
                Ok(FragmentReceipt::Expired)
            } else {
                self.store.save_fragment(
                    endpoint,
                    view.binding.generation,
                    Direction::Outbound,
                    &wire,
                    now,
                )
            };
            let reason = match result {
                Ok(FragmentReceipt::Expired) => Some("hold_expired"),
                Ok(_) => None,
                Err(StorageError::Full) => break,
                Err(StorageError::Invalid | StorageError::Conflict) => {
                    Some("invalid_held_fragment")
                }
                Err(error) => return Err(error.into()),
            };
            let tx = self.store.db.transaction()?;
            if let Some(reason) = reason {
                reject(
                    &tx,
                    endpoint,
                    view.binding.generation,
                    &delivery,
                    &message,
                    reason,
                    now,
                )?;
            }
            tx.execute(
                "DELETE FROM hub_holds WHERE endpoint=?1 AND generation=?2 AND delivery=?3",
                params![endpoint, view.binding.generation, delivery],
            )?;
            tx.commit()?;
            count += 1;
        }
        Ok(count)
    }
    pub(crate) fn cleanup_holds(&mut self, now: i64) -> Result<(), Error> {
        if now < 0 {
            return Err(Error::Binding);
        }
        // At most GLOBAL_COUNT rows: no network, no unbounded backlog scan.
        let mut q=self.store.db.prepare("SELECT h.endpoint,h.generation,h.delivery,h.message,CASE WHEN e.generation IS NULL OR e.generation<>h.generation THEN 'generation_changed' ELSE 'hold_expired' END FROM hub_holds h LEFT JOIN endpoints e ON e.id=h.endpoint WHERE h.expires<=?1 OR e.generation IS NULL OR e.generation<>h.generation ORDER BY h.created_at,h.rowid LIMIT 400")?;
        let rows = q
            .query_map([now], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let tx = self.store.db.transaction()?;
        for (endpoint, generation, delivery, message, reason) in rows {
            reject(
                &tx, &endpoint, generation, &delivery, &message, &reason, now,
            )?;
            tx.execute(
                "DELETE FROM hub_holds WHERE endpoint=?1 AND generation=?2 AND delivery=?3",
                params![endpoint, generation, delivery],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn hold_summary(&self, endpoint: &str) -> Result<HoldSummary, Error> {
        let generation = self.view(endpoint)?.binding.generation;
        let (count,bytes):(i64,i64)=self.store.db.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(wire AS BLOB))),0) FROM hub_holds WHERE endpoint=?1 AND generation=?2",params![endpoint,generation],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let counters:Option<(i64,i64,i64)>=self.store.db.query_row("SELECT capacity,expired,invalid FROM hub_hold_counters WHERE endpoint=?1 AND generation=?2",params![endpoint,generation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (capacity, expired, invalid) = counters.unwrap_or_default();
        let receipts: i64 = self.store.db.query_row(
            "SELECT COUNT(*) FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2",
            params![endpoint, generation],
            |r| r.get(0),
        )?;
        let global: bool = self.store.db.query_row(
            "SELECT capacity_exceeded FROM hub_hold_health WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        Ok(HoldSummary {
            held_fragments: count,
            held_bytes: bytes,
            capacity_rejections: capacity,
            expired_fragments: expired,
            invalid_fragments: invalid,
            receipts,
            global_capacity_exceeded: global,
        })
    }
    pub fn capacity_receipts(
        &self,
        endpoint: &str,
        before: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CapacityReceipt>, Error> {
        if limit == 0 || limit > 100 || before.is_some_and(|id| !valid_id(id)) {
            return Err(Error::Binding);
        }
        let generation = self.view(endpoint)?.binding.generation;
        let mut q=self.store.db.prepare("SELECT delivery,message,generation,reason,at,recovered_at FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2 AND (?4 IS NULL OR delivery<?4) ORDER BY delivery DESC LIMIT ?3")?;
        let rows = q
            .query_map(params![endpoint, generation, limit, before], |r| {
                Ok(CapacityReceipt {
                    delivery_id: r.get(0)?,
                    message_id: r.get(1)?,
                    generation: r.get(2)?,
                    reason: r.get(3)?,
                    at: r.get(4)?,
                    recovered_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Message, Principal},
        hub::{Binding, Readback},
        protocol::{Envelope, Sender, SenderKind},
        recovery::tests::{metadata, NoIo},
        store::{Limits, Store},
    };
    const NOW: i64 = 1_800_000_000;
    fn fixture(store: Store) -> Hub<NoIo> {
        let mut hub = Hub::new(store, NoIo, "app_hub", "ws_test").unwrap();
        for id in ["ag_paused", "ag_healthy"] {
            hub.add_binding(Binding {
                endpoint_id: id.into(),
                generation: 1,
                recipient: "ag_recipient".into(),
                expected_path: format!("/team/{id}"),
                expected_cwd: "/workspace/endpoint".into(),
                expected_server_id: "machine_test".into(),
            })
            .unwrap();
        }
        hub
    }
    fn activate(hub: &mut Hub<NoIo>, id: &str, now: i64) {
        let mut m = metadata();
        m.agent_id = id.into();
        m.path = Some(format!("/team/{id}"));
        let rev = hub.view(id).unwrap().revision;
        hub.resume_after_readback(
            id,
            rev,
            Readback {
                observed: Some(m),
                requested_at: now,
            },
            now,
        )
        .unwrap();
    }
    fn delivery(endpoint: &str, n: usize) -> Delivery {
        let env = Envelope::Outbound {
            delivery_id: format!("del_source_{n}"),
            message_id: format!("msg_source_{n}"),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_recipient".into(),
            },
            body: format!("synthetic body {n}"),
            context_ids: vec![],
        };
        Delivery {
            delivery_id: format!("del_fragment_{n}"),
            recipient: Principal {
                kind: SenderKind::Agent,
                id: "app_hub".into(),
            },
            message: Message {
                message_id: format!("msg_fragment_{n}"),
                sender: Principal {
                    kind: SenderKind::Agent,
                    id: endpoint.into(),
                },
                body: env.fragments(endpoint, 1).unwrap().remove(0),
                context_ids: vec![],
            },
        }
    }
    #[test]
    fn paused_flood_over_caps_drains_ack_window_and_admits_healthy_endpoint_without_failure_ledger()
    {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        activate(&mut hub, "ag_healthy", NOW);
        let mut queue = std::collections::VecDeque::new();
        for n in 0..1100 {
            queue.push_back(delivery("ag_paused", n));
        }
        queue.push_back(delivery("ag_healthy", 1101));
        let mut acknowledgements = 0;
        // Simulated oldest-first 10-item Core pages, bounded to 10 pages/tick.
        // Pop (ack) only after state admission commits. No invented Core cursor.
        for _tick in 0..12 {
            hub.maintenance(NOW).unwrap();
            for _page in 0..10 {
                for _ in 0..10 {
                    let Some(d) = queue.front() else { break };
                    hub.admit_core_delivery(d, NOW).unwrap();
                    queue.pop_front();
                    acknowledgements += 1;
                }
            }
        }
        assert!(queue.is_empty());
        assert_eq!(acknowledgements, 1101);
        let summary = hub.hold_summary("ag_paused").unwrap();
        assert_eq!(summary.held_fragments, 20);
        assert_eq!(summary.capacity_rejections, 1080);
        assert_eq!(summary.receipts, 1000);
        assert!(!summary.global_capacity_exceeded);
        assert_eq!(hub.ready_outbound("ag_healthy", NOW).unwrap().len(), 1);
        let failures: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(failures, 0);
        assert!(!hub.blocked().unwrap());
        let text =
            serde_json::to_string(&hub.capacity_receipts("ag_paused", None, 100).unwrap()).unwrap();
        assert!(!text.contains("synthetic body"));
        let mut cursor = None;
        let mut total = 0;
        let mut ids = std::collections::HashSet::new();
        loop {
            let page = hub
                .capacity_receipts("ag_paused", cursor.as_deref(), 100)
                .unwrap();
            if page.is_empty() {
                break;
            }
            cursor = Some(page.last().unwrap().delivery_id.clone());
            for receipt in page {
                assert!(ids.insert(receipt.delivery_id));
                total += 1;
            }
        }
        assert_eq!(total, 1000);
    }
    #[test]
    fn malformed_paused_flood_uses_bounded_receipts_not_hub_failure_budget() {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        for n in 0..1100 {
            let mut bad = delivery("ag_paused", n);
            bad.message.body = "invalid synthetic fragment".into();
            hub.admit_core_delivery(&bad, NOW).unwrap();
        }
        let summary = hub.hold_summary("ag_paused").unwrap();
        assert_eq!(summary.invalid_fragments, 1100);
        assert_eq!(summary.receipts, 1000);
        assert_eq!(summary.held_fragments, 0);
        let faults: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(faults, 0);
        assert!(!hub.blocked().unwrap());
    }
    #[test]
    fn promotion_is_fifo_bounded_fresh_and_deduplicated_after_commit_before_delete_crash() {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        for n in 0..12 {
            hub.admit_core_delivery(&delivery("ag_paused", n), NOW)
                .unwrap();
        }
        assert!(hub.promote_held("ag_paused", NOW).is_err());
        activate(&mut hub, "ag_paused", NOW);
        assert_eq!(hub.promote_held("ag_paused", NOW).unwrap(), 10);
        assert_eq!(hub.hold_summary("ag_paused").unwrap().held_fragments, 2);
        let ready = hub.ready_outbound("ag_paused", NOW).unwrap();
        assert_eq!(ready[0].1.source_id(), "del_source_0");
        assert_eq!(ready[9].1.source_id(), "del_source_9");
        // Last commit happened; emulate crash before deleting the next held row.
        let d = delivery("ag_paused", 10);
        hub.store
            .save_fragment("ag_paused", 1, Direction::Outbound, &d.message.body, NOW)
            .unwrap();
        assert_eq!(hub.promote_held("ag_paused", NOW).unwrap(), 2);
        assert_eq!(hub.ready_outbound("ag_paused", NOW).unwrap().len(), 12);
        assert_eq!(hub.promote_held("ag_paused", NOW).unwrap(), 0);
        assert!(hub.promote_held("ag_paused", NOW + 31).is_err());
    }
    #[test]
    fn expiry_drops_plaintext_to_metadata_receipt_and_changed_generation_never_promotes() {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        hub.admit_core_delivery(&delivery("ag_paused", 1), NOW)
            .unwrap();
        hub.admit_core_delivery(&delivery("ag_healthy", 2), NOW)
            .unwrap();
        // Model a later human generation change; no production rebind API exists.
        hub.store
            .db
            .execute(
                "UPDATE endpoints SET generation=2 WHERE id='ag_healthy'",
                [],
            )
            .unwrap();
        hub.cleanup_holds(NOW).unwrap();
        let old: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM hub_holds WHERE endpoint='ag_healthy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);
        hub.cleanup_holds(NOW + HOLD_TTL).unwrap();
        let summary = hub.hold_summary("ag_paused").unwrap();
        assert_eq!(summary.held_fragments, 0);
        assert_eq!(summary.expired_fragments, 1);
        assert_eq!(
            hub.capacity_receipts("ag_paused", None, 1).unwrap()[0].reason,
            "hold_expired"
        );
        activate(&mut hub, "ag_paused", NOW + HOLD_TTL);
        assert_eq!(hub.promote_held("ag_paused", NOW + HOLD_TTL).unwrap(), 0);
        assert!(hub
            .ready_outbound("ag_paused", NOW + HOLD_TTL)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn restart_keeps_holds_duplicate_admission_does_not_extend_ttl_or_capacity() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("hub");
        let mut hub = fixture(Store::open(&dir, Limits::default()).unwrap());
        let d = delivery("ag_paused", 1);
        hub.admit_core_delivery(&d, NOW).unwrap();
        drop(hub);
        let mut hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            NoIo,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.admit_core_delivery(&d, NOW + 100).unwrap();
        assert_eq!(hub.hold_summary("ag_paused").unwrap().held_fragments, 1);
        hub.cleanup_holds(NOW + HOLD_TTL).unwrap();
        assert_eq!(hub.hold_summary("ag_paused").unwrap().expired_fragments, 1);
        hub.admit_core_delivery(&d, NOW + HOLD_TTL + 1).unwrap();
        assert_eq!(hub.hold_summary("ag_paused").unwrap().held_fragments, 0);
        assert_eq!(hub.hold_summary("ag_paused").unwrap().expired_fragments, 1);
    }
    #[test]
    fn full_active_quota_uses_separate_hold_and_promotion_waits_for_capacity() {
        let mut hub = fixture(
            Store::memory(Limits {
                endpoint_count: 1,
                ..Limits::default()
            })
            .unwrap(),
        );
        activate(&mut hub, "ag_paused", NOW);
        hub.admit_core_delivery(&delivery("ag_paused", 1), NOW)
            .unwrap();
        hub.admit_core_delivery(&delivery("ag_paused", 2), NOW)
            .unwrap();
        assert_eq!(hub.hold_summary("ag_paused").unwrap().held_fragments, 1);
        assert_eq!(hub.promote_held("ag_paused", NOW).unwrap(), 0);
        let first = hub.ready_outbound("ag_paused", NOW).unwrap().remove(0).0;
        hub.store
            .finish_transfer("ag_paused", 1, Direction::Outbound, &first, NOW)
            .unwrap();
        assert_eq!(hub.promote_held("ag_paused", NOW).unwrap(), 1);
        assert_eq!(hub.ready_outbound("ag_paused", NOW).unwrap().len(), 1);
    }
    #[test]
    fn global_hold_ceiling_counts_and_acks_without_storing_more_content_or_global_block() {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        // Synthetic store pressure; no unrelated App/process or real messages.
        hub.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<400) INSERT INTO hub_holds(endpoint,generation,delivery,message,wire,created_at,expires) SELECT 'synthetic-pressure',1,'d'||i,'m'||i,'not-read',0,9223372036854775807 FROM n;").unwrap();
        hub.admit_core_delivery(&delivery("ag_paused", 1), NOW)
            .unwrap();
        let summary = hub.hold_summary("ag_paused").unwrap();
        assert_eq!(summary.held_fragments, 0);
        assert_eq!(summary.capacity_rejections, 1);
        assert!(summary.global_capacity_exceeded);
        assert!(!hub.blocked().unwrap());
        assert_eq!(
            hub.capacity_receipts("ag_paused", None, 1).unwrap()[0].reason,
            "hub_hold_full"
        );
    }
    #[test]
    fn storage_failure_never_authorizes_ack_or_erases_held_plaintext() {
        let mut hub = fixture(Store::memory(Limits::default()).unwrap());
        for n in 0..20 {
            hub.admit_core_delivery(&delivery("ag_paused", n), NOW)
                .unwrap();
        }
        hub.store.db.execute_batch("CREATE TRIGGER synthetic_receipt_failure BEFORE INSERT ON hub_capacity_receipts BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(hub
            .admit_core_delivery(&delivery("ag_paused", 21), NOW)
            .is_err());
        let summary = hub.hold_summary("ag_paused").unwrap();
        assert_eq!(summary.held_fragments, 20);
        assert_eq!(summary.capacity_rejections, 0);
        assert_eq!(summary.receipts, 0);
        assert!(hub.cleanup_holds(NOW + HOLD_TTL).is_err());
        assert_eq!(hub.hold_summary("ag_paused").unwrap().held_fragments, 20);
    }
}
