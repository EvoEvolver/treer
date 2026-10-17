//! Fixed-code failure reports. Hub sends ONLY to the pinned endpoint; the relay
//! applies its independent local recipient pins and renders text without external
//! provenance. A handoff to Core is never a delivery/read receipt.
use crate::{
    hub::{Directory, Error, Hub},
    protocol::{digest_id, Envelope, NoticeCode, SenderKind},
    relay::Mailbox,
    store::valid_id,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

pub struct NoticeIntent {
    endpoint: String,
    generation: i64,
    transfer: String,
    token: String,
    pub parts: Vec<(String, String)>,
}
impl NoticeIntent {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}
#[derive(Serialize)]
pub struct NoticeView {
    pub transfer_id: String,
    pub status: String,
    pub code: String,
    pub transport_attempts: u16,
    pub handoff_attempts: i64,
}

pub(crate) fn insert_notice(
    tx: &Transaction<'_>,
    binding: &crate::hub::Binding,
    envelope: &Envelope,
    transfer: &str,
    code: NoticeCode,
    now: i64,
) -> Result<(), Error> {
    let endpoint = binding.endpoint_id.as_str();
    let generation = binding.generation;
    let recipient = binding.recipient.as_str();
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM hub_failure_notices WHERE endpoint=?1 AND generation=?2 AND transfer=?3)",params![endpoint,generation,transfer],|r|r.get(0))?;
    if exists {
        return Ok(());
    }
    let (global, local): (i64, i64) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(endpoint=?1),0) FROM hub_failure_notices",
        [endpoint],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if global >= 10_000 || local >= 1000 {
        return Err(Error::FailureBudget);
    }
    let eligible = matches!(envelope,Envelope::Outbound{original_sender,..} if original_sender.kind==SenderKind::Agent && original_sender.id==recipient);
    let chunks: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_chunks')",
        [],
        |r| r.get(0),
    )?;
    let attempts: u16 = if chunks {
        tx.query_row("SELECT COALESCE(SUM(attempts),0) FROM telegram_chunks WHERE endpoint=?1 AND generation=?2 AND transfer=?3",params![endpoint,generation,transfer],|r|r.get(0))?
    } else {
        0
    };
    if attempts > 256 {
        return Err(Error::Binding);
    }
    tx.execute("INSERT INTO hub_failure_notices(endpoint,generation,transfer,recipient,code,transport_attempts,created_at,state,terminal_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![endpoint,generation,transfer,if eligible {Some(recipient)} else {None},code.as_str(),attempts,now,if eligible {0} else {2},if eligible {None} else {Some(now)}])?;
    Ok(())
}
fn valid_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

impl<M: Mailbox + Directory> Hub<M> {
    pub(crate) fn notice_schema(&self) -> Result<(), Error> {
        self.store.db.execute_batch("CREATE TABLE IF NOT EXISTS hub_failure_notices(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,transfer TEXT NOT NULL,recipient TEXT,code TEXT NOT NULL,transport_attempts INTEGER NOT NULL,created_at INTEGER NOT NULL,state INTEGER NOT NULL DEFAULT 0,handoff_attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,lease_token TEXT,lease_until INTEGER NOT NULL DEFAULT 0,core_id TEXT,terminal_at INTEGER,PRIMARY KEY(endpoint,generation,transfer));")?;
        Ok(())
    }
    /// Internal worker only; fresh CSPRNG lease tokens never come from a browser.
    /// Pending reports wait while binding is paused or metadata is stale.
    pub fn prepare_failure_notice(
        &mut self,
        endpoint: &str,
        now: i64,
        token: &str,
    ) -> Result<Option<NoticeIntent>, Error> {
        if !valid_token(token) || now < 0 {
            return Err(Error::Binding);
        }
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let view = self.active(endpoint, now)?;
        let until = now.checked_add(60).ok_or(Error::Binding)?;
        let tx = self.store.db.transaction()?;
        tx.execute("UPDATE hub_failure_notices SET state=3,terminal_at=?3 WHERE endpoint=?1 AND generation=?2 AND state=0 AND handoff_attempts>=8 AND lease_until<=?3 AND next_attempt<=?3",params![endpoint,view.binding.generation,now])?;
        let row:Option<(String,String,String,u16,i64)>=tx.query_row("SELECT transfer,recipient,code,transport_attempts,handoff_attempts FROM hub_failure_notices WHERE endpoint=?1 AND generation=?2 AND state=0 AND lease_until<=?3 AND next_attempt<=?3 ORDER BY created_at,transfer LIMIT 1",params![endpoint,view.binding.generation,now],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let Some((transfer, recipient, code, attempts, handoffs)) = row else {
            tx.commit()?;
            return Ok(None);
        };
        if recipient != view.binding.recipient || !(0..8).contains(&handoffs) {
            return Err(Error::Binding);
        }
        let code = match code.as_str() {
            "transport_rejected" => NoticeCode::TransportRejected,
            "transport_retries_exhausted" => NoticeCode::TransportRetriesExhausted,
            _ => return Err(Error::Binding),
        };
        let source = format!(
            "failure-{}",
            digest_id(
                "failure-notice",
                endpoint,
                view.binding.generation as u64,
                &transfer
            )
        );
        let envelope = Envelope::FailureNotice {
            job_id: source,
            recipient,
            transfer_id: transfer.clone(),
            code,
            attempts,
        };
        let parts = envelope
            .fragments(endpoint, view.binding.generation as u64)
            .map_err(|_| Error::Binding)?
            .into_iter()
            .map(|body| {
                let frame = crate::protocol::Fragment::parse(&body).map_err(|_| Error::Binding)?;
                Ok((frame.idempotency_key(), body))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let next = now
            .checked_add((5_i64 << handoffs.min(6)).min(300))
            .ok_or(Error::Binding)?;
        tx.execute("UPDATE hub_failure_notices SET lease_token=?4,lease_until=?5,next_attempt=?6,handoff_attempts=handoff_attempts+1 WHERE endpoint=?1 AND generation=?2 AND transfer=?3",params![endpoint,view.binding.generation,transfer,token,until,next])?;
        tx.commit()?;
        Ok(Some(NoticeIntent {
            endpoint: endpoint.into(),
            generation: view.binding.generation,
            transfer,
            token: token.into(),
            parts,
        }))
    }
    /// Commit only after ALL fragments were accepted by Core. Allowed after pause
    /// and fenced against stale leases. No endpoint/recipient delivery assertion.
    pub fn complete_failure_notice(
        &mut self,
        intent: &NoticeIntent,
        core_id: &str,
        now: i64,
    ) -> Result<(), Error> {
        if !valid_id(core_id) || now < 0 {
            return Err(Error::Binding);
        }
        let changed=self.store.db.execute("UPDATE hub_failure_notices SET state=1,core_id=?5,terminal_at=?6 WHERE endpoint=?1 AND generation=?2 AND transfer=?3 AND lease_token=?4 AND state IN (0,1) AND (core_id IS NULL OR core_id=?5) AND EXISTS(SELECT 1 FROM endpoints e WHERE e.id=?1 AND e.generation=?2)",params![intent.endpoint,intent.generation,intent.transfer,intent.token,core_id,now])?;
        if changed != 1 {
            return Err(Error::Binding);
        }
        Ok(())
    }
    pub fn fail_failure_notice(&mut self, intent: &NoticeIntent, now: i64) -> Result<(), Error> {
        let attempts:Option<i64>=self.store.db.query_row("SELECT handoff_attempts FROM hub_failure_notices WHERE endpoint=?1 AND generation=?2 AND transfer=?3 AND lease_token=?4 AND state=0",params![intent.endpoint,intent.generation,intent.transfer,intent.token],|r|r.get(0)).optional()?;
        let attempts = attempts
            .filter(|n| (1..=8).contains(n))
            .ok_or(Error::Binding)?;
        let next = now
            .checked_add((5_i64 << (attempts - 1).min(6)).min(300))
            .filter(|_| now >= 0)
            .ok_or(Error::Binding)?;
        self.store.db.execute("UPDATE hub_failure_notices SET lease_until=0,next_attempt=?5 WHERE endpoint=?1 AND generation=?2 AND transfer=?3 AND lease_token=?4 AND state=0",params![intent.endpoint,intent.generation,intent.transfer,intent.token,next])?;
        Ok(())
    }
    pub fn failure_notices(&self, endpoint: &str, limit: u32) -> Result<Vec<NoticeView>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Binding);
        }
        let view = self.view(endpoint)?;
        let mut q=self.store.db.prepare("SELECT transfer,state,code,transport_attempts,handoff_attempts FROM hub_failure_notices WHERE endpoint=?1 AND generation=?2 ORDER BY created_at DESC,transfer LIMIT ?3")?;
        let rows = q
            .query_map(params![endpoint, view.binding.generation, limit], |r| {
                let state: i64 = r.get(1)?;
                Ok(NoticeView {
                    transfer_id: r.get(0)?,
                    status: match state {
                        0 => "pending",
                        1 => "handed_to_endpoint",
                        2 => "recipient_not_pinned",
                        3 => "handoff_retries_exhausted",
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    }
                    .into(),
                    code: r.get(2)?,
                    transport_attempts: r.get(3)?,
                    handoff_attempts: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    pub(crate) fn cleanup_failure_notices(&self, now: i64) -> Result<(), Error> {
        if now < crate::transfers::TOMBSTONE_TTL {
            return Err(Error::Binding);
        }
        self.store.db.execute(
            "DELETE FROM hub_failure_notices WHERE state<>0 AND terminal_at<=?1 AND NOT EXISTS(SELECT 1 FROM transfers t WHERE t.endpoint=hub_failure_notices.endpoint AND t.generation=hub_failure_notices.generation AND t.direction=0 AND t.transfer=hub_failure_notices.transfer)",
            [now - crate::transfers::TOMBSTONE_TTL],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{Assembly, Direction, Fragment, Sender},
        recovery::{
            tests::{fixture, metadata, NoIo},
            Recovery,
        },
        store::{Limits, Store},
    };
    const NOW: i64 = 1_800_000_000;
    fn memory() -> (Hub<NoIo>, String) {
        fixture(Store::memory(Limits::default()).unwrap())
    }
    fn resume(hub: &mut Hub<NoIo>, now: i64) {
        let revision = hub.view("ag_endpoint").unwrap().revision;
        hub.resume_after_readback(
            "ag_endpoint",
            revision,
            crate::hub::Readback {
                observed: Some(metadata()),
                requested_at: now,
            },
            now,
        )
        .unwrap();
    }
    fn envelope(intent: &NoticeIntent) -> Envelope {
        let first = Fragment::parse(&intent.parts[0].1).unwrap();
        let mut assembly = Assembly::new(&first, "ag_endpoint", 1, Direction::Inbound).unwrap();
        let mut result = None;
        for (_, body) in &intent.parts {
            result = assembly.push(&Fragment::parse(body).unwrap()).unwrap();
        }
        result.unwrap()
    }
    #[test]
    fn notice_is_durable_fixed_text_and_sent_only_through_endpoint_to_pinned_recipient() {
        let (mut hub, id) = memory();
        assert!(hub
            .prepare_failure_notice("ag_endpoint", NOW, &"1".repeat(64))
            .is_err());
        resume(&mut hub, NOW);
        let intent = hub
            .prepare_failure_notice("ag_endpoint", NOW, &"1".repeat(64))
            .unwrap()
            .unwrap();
        assert_eq!(intent.endpoint(), "ag_endpoint");
        let env = envelope(&intent);
        assert!(crate::protocol::check_recipient(&env, &["ag_recipient".into()]).is_ok());
        let text = env.failure_notice_body().unwrap();
        assert!(text.contains(&id));
        assert!(text.contains("transport_rejected"));
        assert!(text.contains("Attempts: 2"));
        assert!(!text.contains("sensitive"));
        let json = serde_json::to_value(&env).unwrap();
        assert!(json.get("source").is_none());
        assert!(json.get("context_ids").is_none());
        assert!(json.get("body").is_none());
        hub.pause("ag_endpoint", 5).unwrap();
        hub.complete_failure_notice(&intent, "msg_handoff", NOW)
            .unwrap();
        assert_eq!(
            hub.failure_notices("ag_endpoint", 10).unwrap()[0].status,
            "handed_to_endpoint"
        );
        // Handoff completion after pause is permitted, not downstream delivery.
        resume(&mut hub, NOW);
        assert!(hub
            .prepare_failure_notice("ag_endpoint", NOW, &"2".repeat(64))
            .unwrap()
            .is_none());
    }
    #[test]
    fn lease_restart_retry_uses_identical_payload_keys_and_fences_old_result() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("hub");
        let (mut hub, _) = fixture(Store::open(&dir, Limits::default()).unwrap());
        resume(&mut hub, NOW);
        let first = hub
            .prepare_failure_notice("ag_endpoint", NOW, &"1".repeat(64))
            .unwrap()
            .unwrap();
        drop(hub);
        let mut hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            NoIo,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        hub.record_readback(
            "ag_endpoint",
            5,
            crate::hub::Readback {
                observed: Some(metadata()),
                requested_at: NOW + 60,
            },
            NOW + 60,
        )
        .unwrap();
        let second = hub
            .prepare_failure_notice("ag_endpoint", NOW + 60, &"2".repeat(64))
            .unwrap()
            .unwrap();
        assert_eq!(first.parts, second.parts);
        assert!(hub
            .complete_failure_notice(&first, "msg_old", NOW + 60)
            .is_err());
        assert!(hub.fail_failure_notice(&first, NOW + 60).is_err());
        hub.complete_failure_notice(&second, "msg_accepted", NOW + 60)
            .unwrap();
    }
    #[test]
    fn arbitrary_original_sender_or_human_never_becomes_a_callback_recipient() {
        let (mut hub, _) = memory();
        resume(&mut hub, NOW);
        for (i, kind, id) in [
            (1, SenderKind::Agent, "ag_outside"),
            (2, SenderKind::Human, "ag_recipient"),
        ] {
            let env = Envelope::Outbound {
                delivery_id: format!("del_other_{i}"),
                message_id: format!("msg_other_{i}"),
                original_sender: Sender {
                    kind,
                    id: id.into(),
                },
                body: "no routing authority".into(),
                context_ids: vec![],
            };
            let parts = env.fragments("ag_endpoint", 1).unwrap();
            let transfer = Fragment::parse(&parts[0]).unwrap().transfer_id;
            for part in parts {
                hub.store
                    .save_fragment("ag_endpoint", 1, Direction::Outbound, &part, NOW)
                    .unwrap();
            }
            hub.park_transport(
                "ag_endpoint",
                1,
                &transfer,
                crate::failures::FailureReason::TransportRejected,
                NOW,
            )
            .unwrap();
        }
        let views = hub.failure_notices("ag_endpoint", 10).unwrap();
        assert_eq!(
            views
                .iter()
                .filter(|v| v.status == "recipient_not_pinned")
                .count(),
            2
        );
        let recipients:i64=hub.store.db.query_row("SELECT COUNT(*) FROM hub_failure_notices WHERE recipient IS NOT NULL AND recipient<>'ag_recipient'",[],|r|r.get(0)).unwrap();
        assert_eq!(recipients, 0);
    }
    #[test]
    fn owner_retry_and_repark_cannot_generate_a_second_notice() {
        let (mut hub, id) = memory();
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Retry, "human_owner", NOW)
            .unwrap();
        hub.park_transport(
            "ag_endpoint",
            1,
            &id,
            crate::failures::FailureReason::TransportRetriesExhausted,
            NOW + 1,
        )
        .unwrap();
        let views = hub.failure_notices("ag_endpoint", 10).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].code, "transport_rejected");
        assert_eq!(views[0].transport_attempts, 2);
    }
    #[test]
    fn retries_are_bounded_backoff_persists_and_pending_reports_do_not_expire() {
        let (mut hub, _) = memory();
        resume(&mut hub, NOW);
        let mut at = NOW;
        for attempt in 0..8 {
            hub.record_readback(
                "ag_endpoint",
                5,
                crate::hub::Readback {
                    observed: Some(metadata()),
                    requested_at: at,
                },
                at,
            )
            .unwrap();
            let intent = hub
                .prepare_failure_notice("ag_endpoint", at, &format!("{attempt:064x}"))
                .unwrap()
                .unwrap();
            hub.fail_failure_notice(&intent, at + 10).unwrap();
            assert!(hub
                .prepare_failure_notice("ag_endpoint", at + 10, &"f".repeat(64))
                .unwrap()
                .is_none());
            at += 10 + (5_i64 << attempt.min(6)).min(300);
        }
        hub.record_readback(
            "ag_endpoint",
            5,
            crate::hub::Readback {
                observed: Some(metadata()),
                requested_at: at,
            },
            at,
        )
        .unwrap();
        assert!(hub
            .prepare_failure_notice("ag_endpoint", at, &"f".repeat(64))
            .unwrap()
            .is_none());
        let view = hub.failure_notices("ag_endpoint", 1).unwrap();
        assert_eq!(view[0].handoff_attempts, 8);
        assert_eq!(view[0].status, "handoff_retries_exhausted");
        hub.cleanup_failure_notices(at + crate::transfers::TOMBSTONE_TTL)
            .unwrap();
        assert_eq!(hub.failure_notices("ag_endpoint", 10).unwrap().len(), 1);
        // Keep the one-notice marker while original parked payload still exists.
        // Only owner discard + ordinary transfer tombstone expiry frees it.
        hub.pause("ag_endpoint", 5).unwrap();
        let transfer = hub.failure_notices("ag_endpoint", 1).unwrap()[0]
            .transfer_id
            .clone();
        hub.recover_parked(
            "ag_endpoint",
            6,
            &transfer,
            Recovery::Discard,
            "human_owner",
            at,
        )
        .unwrap();
        hub.store
            .cleanup_transfers(at + crate::transfers::TOMBSTONE_TTL)
            .unwrap();
        hub.cleanup_failure_notices(at + crate::transfers::TOMBSTONE_TTL)
            .unwrap();
        assert!(hub.failure_notices("ag_endpoint", 10).unwrap().is_empty());
        // Separate pending receipt must survive terminal retention.
        hub.store.db.execute("INSERT INTO hub_failure_notices(endpoint,generation,transfer,recipient,code,transport_attempts,created_at) VALUES('ag_endpoint',1,'synthetic','ag_recipient','transport_rejected',1,0)",[]).unwrap();
        hub.cleanup_failure_notices(at + crate::transfers::TOMBSTONE_TTL)
            .unwrap();
        assert_eq!(hub.failure_notices("ag_endpoint", 10).unwrap().len(), 1);
    }
    #[test]
    fn full_notice_budget_blocks_safely_without_discarding_transfer() {
        let (mut hub, id) = memory();
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Retry, "human_owner", NOW)
            .unwrap();
        hub.store
            .db
            .execute("DELETE FROM hub_failure_notices", [])
            .unwrap();
        hub.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO hub_failure_notices(endpoint,generation,transfer,code,transport_attempts,created_at,state) SELECT 'ag_endpoint',1,'synthetic-'||i,'transport_rejected',1,0,2 FROM n;").unwrap();
        assert!(matches!(
            hub.park_transport(
                "ag_endpoint",
                1,
                &id,
                crate::failures::FailureReason::TransportRejected,
                NOW
            ),
            Err(Error::FailureBudget)
        ));
        assert!(hub.blocked().unwrap());
        assert!(hub
            .store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &id)
            .is_ok());
        assert!(hub.parked_transfers("ag_endpoint", 10).unwrap().is_empty());
    }
    #[test]
    fn notice_write_failure_rolls_back_park_without_losing_original() {
        let (mut hub, id) = memory();
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Retry, "human_owner", NOW)
            .unwrap();
        hub.store
            .db
            .execute("DELETE FROM hub_failure_notices", [])
            .unwrap();
        hub.store.db.execute_batch("CREATE TRIGGER synthetic_notice_failure BEFORE INSERT ON hub_failure_notices BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(hub
            .park_transport(
                "ag_endpoint",
                1,
                &id,
                crate::failures::FailureReason::TransportRejected,
                NOW
            )
            .is_err());
        assert!(hub.parked_transfers("ag_endpoint", 10).unwrap().is_empty());
        assert!(hub
            .store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &id)
            .is_ok());
    }
}
