//! State-only recovery. The future HTTP service must authenticate the current
//! owner, check CSRF and confirmation, and derive actor ID itself before calling.
use crate::{
    hub::{Directory, Error, Hub},
    relay::Mailbox,
    store::valid_id,
};
use rusqlite::params;
use serde::Serialize;

const AUDIT_LIMIT: i64 = 10_000;

#[derive(Clone, Copy)]
pub enum Recovery {
    Retry,
    Discard,
}
impl Recovery {
    fn code(self) -> &'static str {
        match self {
            Self::Retry => "retry_parked",
            Self::Discard => "discard_parked",
        }
    }
}

/// Metadata only: plaintext/provider errors and private lease tokens are absent.
#[derive(Serialize)]
pub struct Parked {
    pub transfer_id: String,
    pub reason: String,
    pub created_at: i64,
    pub attempts: i64,
    pub accepted_chunks: i64,
    pub wire_bytes: i64,
}
#[derive(Serialize)]
pub struct Audit {
    pub id: i64,
    pub actor_id: String,
    pub action: String,
    pub endpoint_id: String,
    pub transfer_id: String,
    pub previous_revision: i64,
    pub at: i64,
}

impl<M: Mailbox + Directory> Hub<M> {
    fn recovery_schema(&self) -> Result<(), Error> {
        self.store.db.execute_batch("CREATE TABLE IF NOT EXISTS hub_owner_audit(id INTEGER PRIMARY KEY AUTOINCREMENT,actor TEXT NOT NULL,action TEXT NOT NULL,endpoint TEXT NOT NULL,transfer TEXT NOT NULL,revision INTEGER NOT NULL,at INTEGER NOT NULL);")?;
        Ok(())
    }
    pub fn parked_transfers(&self, endpoint: &str, limit: u32) -> Result<Vec<Parked>, Error> {
        if limit == 0 || limit > 100 {
            return Err(Error::Binding);
        }
        let view = self.view(endpoint)?;
        // Only Telegram has a transport implementation at this stage.
        self.telegram_schema()?;
        let mut query = self.store.db.prepare("SELECT p.transfer,p.reason,p.created_at,COALESCE((SELECT SUM(c.attempts) FROM telegram_chunks c WHERE c.endpoint=p.endpoint AND c.generation=p.generation AND c.transfer=p.transfer),0), (SELECT COUNT(*) FROM telegram_chunks c WHERE c.endpoint=p.endpoint AND c.generation=p.generation AND c.transfer=p.transfer AND c.message_id IS NOT NULL),COALESCE((SELECT SUM(length(CAST(f.wire AS BLOB))) FROM fragments f WHERE f.endpoint=p.endpoint AND f.generation=p.generation AND f.direction=0 AND f.transfer=p.transfer),0) FROM hub_parked p WHERE p.endpoint=?1 AND p.generation=?2 ORDER BY p.created_at,p.transfer LIMIT ?3")?;
        let rows = query
            .query_map(params![endpoint, view.binding.generation, limit], |r| {
                Ok(Parked {
                    transfer_id: r.get(0)?,
                    reason: r.get(1)?,
                    created_at: r.get(2)?,
                    attempts: r.get(3)?,
                    accepted_chunks: r.get(4)?,
                    wire_bytes: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    /// Internal owner-service call, not an authorization check. Requires an
    /// explicit pause, revision, current verified human actor, and confirmation.
    /// A live transport lease forbids either operation. Retrying preserves all
    /// accepted chunks and stable external keys; ambiguous sends can duplicate.
    pub fn recover_parked(
        &mut self,
        endpoint: &str,
        revision: i64,
        transfer: &str,
        action: Recovery,
        actor: &str,
        now: i64,
    ) -> Result<(), Error> {
        if !valid_id(actor) || !valid_id(transfer) || now < 0 {
            return Err(Error::Binding);
        }
        let view = self.view(endpoint)?;
        if view.revision != revision {
            return Err(Error::Revision);
        }
        if !view.paused {
            return Err(Error::Binding);
        }
        self.recovery_schema()?;
        self.telegram_schema()?;
        let generation = view.binding.generation;
        let tx = self.store.db.transaction()?;
        let parked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM hub_parked p JOIN transfers t ON t.endpoint=p.endpoint AND t.generation=p.generation AND t.transfer=p.transfer AND t.direction=0 AND t.state=1 WHERE p.endpoint=?1 AND p.generation=?2 AND p.transfer=?3)",params![endpoint,generation,transfer],|r|r.get(0))?;
        let leased:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM telegram_chunks WHERE endpoint=?1 AND generation=?2 AND transfer=?3 AND lease_until>?4 AND message_id IS NULL)",params![endpoint,generation,transfer,now],|r|r.get(0))?;
        if !parked || leased {
            return Err(Error::Binding);
        }
        let audits: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
        if audits >= AUDIT_LIMIT {
            return Err(Error::Storage(crate::store::Error::Full));
        }
        match action {
            Recovery::Retry => {
                // Accepted chunks remain immutable and are never resent. Clear
                // private capabilities so old workers cannot commit after retry.
                tx.execute("UPDATE telegram_chunks SET attempts=0,next_attempt=0,failure_class=0,lease_token=NULL,lease_until=NULL WHERE endpoint=?1 AND generation=?2 AND transfer=?3 AND message_id IS NULL",params![endpoint,generation,transfer])?;
            }
            Recovery::Discard => {
                // Distinct terminal state: deliberate discard is NOT delivered.
                // Per-part hashes remain for replay dedup until terminal pruning.
                tx.execute("UPDATE transfers SET state=4,terminal_at=?4 WHERE endpoint=?1 AND generation=?2 AND direction=0 AND transfer=?3 AND state=1",params![endpoint,generation,transfer,now])?;
                tx.execute("UPDATE fragments SET wire=NULL WHERE endpoint=?1 AND generation=?2 AND direction=0 AND transfer=?3",params![endpoint,generation,transfer])?;
                tx.execute("DELETE FROM telegram_chunks WHERE endpoint=?1 AND generation=?2 AND transfer=?3",params![endpoint,generation,transfer])?;
            }
        }
        tx.execute(
            "DELETE FROM hub_parked WHERE endpoint=?1 AND generation=?2 AND transfer=?3",
            params![endpoint, generation, transfer],
        )?;
        tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,?2,?3,?4,?5,?6)",params![actor,action.code(),endpoint,transfer,revision,now])?;
        tx.execute(
            "UPDATE hub_bindings SET revision=revision+1 WHERE endpoint=?1",
            [endpoint],
        )?;
        // Recovery does NOT clear the circuit breaker or resume a binding.
        tx.commit()?;
        Ok(())
    }
    pub fn owner_audit(&self, before: Option<i64>, limit: u32) -> Result<Vec<Audit>, Error> {
        if limit == 0 || limit > 100 || before.is_some_and(|n| n <= 0) {
            return Err(Error::Binding);
        }
        self.recovery_schema()?;
        let mut query=self.store.db.prepare("SELECT id,actor,action,endpoint,transfer,revision,at FROM hub_owner_audit WHERE (?1 IS NULL OR id<?1) ORDER BY id DESC LIMIT ?2")?;
        let rows = query
            .query_map(params![before, limit], |r| {
                Ok(Audit {
                    id: r.get(0)?,
                    actor_id: r.get(1)?,
                    action: r.get(2)?,
                    endpoint_id: r.get(3)?,
                    transfer_id: r.get(4)?,
                    previous_revision: r.get(5)?,
                    at: r.get(6)?,
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
        core::{AgentMetadata, Delivery},
        hub::{Binding, Readback},
        protocol::{Direction, Envelope, Fragment, Sender, SenderKind},
        store::{Limits, Store},
        telegram::{Error as TransportError, Target},
    };
    const NOW: i64 = 1_800_000_000;
    struct NoIo;
    impl Directory for NoIo {
        fn metadata(&self, _: &str) -> Result<AgentMetadata, crate::core::Error> {
            panic!("state must not query Core")
        }
    }
    impl Mailbox for NoIo {
        fn receive(&self, _: u32) -> Result<Vec<Delivery>, crate::core::Error> {
            panic!("state must not receive")
        }
        fn ack(&self, _: &str) -> Result<(), crate::core::Error> {
            panic!("state must not ack")
        }
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
            _: Option<serde_json::Value>,
        ) -> Result<String, crate::core::Error> {
            panic!("state must not send")
        }
    }
    fn metadata() -> AgentMetadata {
        AgentMetadata {
            agent_id: "ag_endpoint".into(),
            workspace_id: "ws_test".into(),
            server_id: "machine_test".into(),
            kind: "command".into(),
            cwd: "/workspace/endpoint".into(),
            status: "running".into(),
            path: Some("/team/endpoint".into()),
        }
    }
    fn detailed_fixture(store: Store) -> (Hub<NoIo>, String, crate::telegram::ChunkIntent) {
        let mut hub = Hub::new(store, NoIo, "app_hub", "ws_test").unwrap();
        hub.add_binding(Binding {
            endpoint_id: "ag_endpoint".into(),
            generation: 1,
            recipient: "ag_recipient".into(),
            expected_path: "/team/endpoint".into(),
            expected_cwd: "/workspace/endpoint".into(),
            expected_server_id: "machine_test".into(),
        })
        .unwrap();
        hub.bind_telegram(
            "ag_endpoint",
            1,
            Target {
                account_id: "bot_test".into(),
                chat_id: -100,
                topic_id: None,
                allowed_senders: vec![42],
            },
        )
        .unwrap();
        hub.resume_after_readback(
            "ag_endpoint",
            2,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW,
        )
        .unwrap();
        let env = Envelope::Outbound {
            delivery_id: "del_source".into(),
            message_id: "msg_source".into(),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_recipient".into(),
            },
            body: "sensitive synthetic body ".repeat(200),
            context_ids: vec![],
        };
        let parts = env.fragments("ag_endpoint", 1).unwrap();
        let transfer = Fragment::parse(&parts[0]).unwrap().transfer_id;
        for p in parts {
            hub.store
                .save_fragment("ag_endpoint", 1, Direction::Outbound, &p, NOW)
                .unwrap();
        }
        // One accepted chunk, then a permanent failure on the second chunk.
        let first = hub
            .prepare_telegram_chunk("ag_endpoint", NOW, &"1".repeat(64))
            .unwrap()
            .unwrap();
        hub.complete_telegram_chunk(&first, 1001, NOW).unwrap();
        let second = hub
            .prepare_telegram_chunk("ag_endpoint", NOW, &"2".repeat(64))
            .unwrap()
            .unwrap();
        hub.fail_telegram_chunk(&second, &TransportError::Rejected, NOW)
            .unwrap();
        assert!(hub
            .prepare_telegram_chunk("ag_endpoint", NOW, &"3".repeat(64))
            .unwrap()
            .is_none());
        hub.pause("ag_endpoint", 3).unwrap();
        (hub, transfer, second)
    }
    fn fixture(store: Store) -> (Hub<NoIo>, String) {
        let (hub, transfer, _) = detailed_fixture(store);
        (hub, transfer)
    }
    fn memory() -> (Hub<NoIo>, String) {
        fixture(Store::memory(Limits::default()).unwrap())
    }
    #[test]
    fn explicit_retry_can_repark_with_new_reason_without_rewriting_first_receipt() {
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
        assert_eq!(
            hub.parked_transfers("ag_endpoint", 1).unwrap()[0].reason,
            "transport_retries_exhausted"
        );
        let count: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let first: i64 = hub
            .store
            .db
            .query_row("SELECT reason FROM failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(first, 5); // original transport_rejected receipt is immutable
    }
    #[test]
    fn old_worker_result_cannot_commit_after_owner_retry_or_discard() {
        for action in [Recovery::Retry, Recovery::Discard] {
            let (mut hub, id, intent) = detailed_fixture(Store::memory(Limits::default()).unwrap());
            hub.recover_parked("ag_endpoint", 4, &id, action, "human_owner", NOW)
                .unwrap();
            assert!(hub.complete_telegram_chunk(&intent, 1002, NOW + 1).is_err());
            assert!(hub
                .fail_telegram_chunk(&intent, &TransportError::Transport, NOW + 1)
                .is_err());
            assert!(hub.view("ag_endpoint").unwrap().paused);
        }
    }
    #[test]
    fn retry_preserves_accepted_chunks_invalidates_capabilities_and_stays_paused() {
        let (mut hub, id) = memory();
        let before = hub.parked_transfers("ag_endpoint", 100).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].accepted_chunks, 1);
        assert_eq!(before[0].attempts, 2);
        assert!(before[0].wire_bytes > 0);
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Retry, "human_owner", NOW)
            .unwrap();
        assert!(hub.view("ag_endpoint").unwrap().paused);
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 5);
        assert!(hub.parked_transfers("ag_endpoint", 100).unwrap().is_empty());
        let accepted: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM telegram_chunks WHERE message_id=1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(accepted, 1);
        let unsent: (i64, Option<String>) = hub
            .store
            .db
            .query_row(
                "SELECT attempts,lease_token FROM telegram_chunks WHERE message_id IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(unsent, (0, None));
        hub.resume_after_readback(
            "ag_endpoint",
            5,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW + 1,
            },
            NOW + 1,
        )
        .unwrap();
        let next = hub
            .prepare_telegram_chunk("ag_endpoint", NOW + 1, &"4".repeat(64))
            .unwrap()
            .unwrap();
        // The first accepted text remains skipped on retry.
        assert!(next.text.contains("| "));
        let audit = hub.owner_audit(None, 100).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].actor_id, "human_owner");
        assert_eq!(audit[0].action, "retry_parked");
    }
    #[test]
    fn discard_clears_plaintext_and_chunk_rows_keeps_dedup_then_prunes_tombstone() {
        let (mut hub, id) = memory();
        let wire: String = hub
            .store
            .db
            .query_row(
                "SELECT wire FROM fragments WHERE transfer=?1 LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Discard, "human_owner", NOW)
            .unwrap();
        let state: i64 = hub
            .store
            .db
            .query_row(
                "SELECT state FROM transfers WHERE transfer=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, 4);
        let remaining: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM fragments WHERE wire IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        let chunks: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM telegram_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks, 0);
        hub.resume_after_readback(
            "ag_endpoint",
            5,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW,
        )
        .unwrap();
        assert_eq!(
            hub.store
                .save_fragment("ag_endpoint", 1, Direction::Outbound, &wire, NOW)
                .unwrap(),
            crate::transfers::FragmentReceipt::Duplicate
        );
        assert!(hub.ready_outbound("ag_endpoint", NOW).unwrap().is_empty());
        assert!(hub
            .store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &id)
            .is_err());
        assert_eq!(
            hub.store
                .cleanup_transfers(NOW + crate::transfers::TOMBSTONE_TTL)
                .unwrap()
                .pruned,
            1
        );
        assert_eq!(
            hub.owner_audit(None, 100).unwrap()[0].action,
            "discard_parked"
        );
    }
    #[test]
    fn recovery_rejects_active_binding_wrong_revision_live_lease_and_duplicate_action() {
        let (mut hub, id) = memory();
        assert!(matches!(
            hub.recover_parked("ag_endpoint", 3, &id, Recovery::Retry, "human_owner", NOW),
            Err(Error::Revision)
        ));
        hub.resume_after_readback(
            "ag_endpoint",
            4,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW,
            },
            NOW,
        )
        .unwrap();
        assert!(hub
            .recover_parked("ag_endpoint", 5, &id, Recovery::Retry, "human_owner", NOW)
            .is_err());
        hub.pause("ag_endpoint", 5).unwrap();
        hub.store
            .db
            .execute(
                "UPDATE telegram_chunks SET lease_until=?1 WHERE message_id IS NULL",
                [NOW + 60],
            )
            .unwrap();
        assert!(hub
            .recover_parked("ag_endpoint", 6, &id, Recovery::Discard, "human_owner", NOW)
            .is_err());
        assert!(hub.owner_audit(None, 100).unwrap().is_empty());
        hub.recover_parked(
            "ag_endpoint",
            6,
            &id,
            Recovery::Discard,
            "human_owner",
            NOW + 60,
        )
        .unwrap();
        assert!(hub
            .recover_parked(
                "ag_endpoint",
                7,
                &id,
                Recovery::Discard,
                "human_owner",
                NOW + 60
            )
            .is_err());
        assert_eq!(hub.owner_audit(None, 100).unwrap().len(), 1);
    }
    #[test]
    fn audit_failure_rolls_back_discard_and_revision() {
        let (mut hub, id) = memory();
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("CREATE TRIGGER synthetic_audit_failure BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(hub
            .recover_parked("ag_endpoint", 4, &id, Recovery::Discard, "human_owner", NOW)
            .is_err());
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 4);
        assert_eq!(hub.parked_transfers("ag_endpoint", 10).unwrap().len(), 1);
        assert!(hub
            .store
            .transfer_payload("ag_endpoint", 1, Direction::Outbound, &id)
            .is_ok());
        assert!(hub.owner_audit(None, 10).unwrap().is_empty());
    }
    #[test]
    fn full_audit_budget_refuses_recovery_without_losing_parked_payload() {
        let (mut hub, id) = memory();
        hub.recovery_schema().unwrap();
        hub.store.db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'human_owner','synthetic','ag_endpoint','synthetic',1,0 FROM n;").unwrap();
        assert!(matches!(
            hub.recover_parked("ag_endpoint", 4, &id, Recovery::Discard, "human_owner", NOW),
            Err(Error::Storage(crate::store::Error::Full))
        ));
        assert_eq!(hub.parked_transfers("ag_endpoint", 10).unwrap().len(), 1);
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 4);
    }
    #[test]
    fn recovery_audit_and_accepted_chunks_survive_restart_without_body_in_views() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("hub");
        let (mut hub, id) = fixture(Store::open(&dir, Limits::default()).unwrap());
        let view =
            serde_json::to_string(&hub.parked_transfers("ag_endpoint", 10).unwrap()).unwrap();
        assert!(!view.contains("sensitive"));
        assert!(!view.contains("lease"));
        hub.recover_parked("ag_endpoint", 4, &id, Recovery::Retry, "human_owner", NOW)
            .unwrap();
        drop(hub);
        let hub = Hub::new(
            Store::open(&dir, Limits::default()).unwrap(),
            NoIo,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        assert_eq!(hub.view("ag_endpoint").unwrap().revision, 5);
        assert!(hub.view("ag_endpoint").unwrap().paused);
        let audit = hub.owner_audit(None, 1).unwrap();
        assert_eq!(audit.len(), 1);
        assert!(hub.owner_audit(Some(audit[0].id), 1).unwrap().is_empty());
        assert!(!serde_json::to_string(&audit).unwrap().contains("sensitive"));
        let accepted: i64 = hub
            .store
            .db
            .query_row(
                "SELECT COUNT(*) FROM telegram_chunks WHERE message_id=1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(accepted, 1);
    }
}
