//! Audited receipt recovery primitives, NOT an HTTP authorization boundary.
//! The owner service must verify a current human owner, CSRF and confirmation;
//! actor IDs and lease randomness are supplied by that service, never browser JSON.
use crate::{
    core::{Core, Message},
    hub::{Directory, Error, Hub},
    protocol::Direction,
    recovery::AUDIT_LIMIT,
    relay::Mailbox,
    store::{valid_id, Error as StorageError},
    transfers::{FragmentReceipt, TOMBSTONE_TTL},
};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

#[derive(Clone)]
pub struct RefetchIntent {
    endpoint: String,
    generation: i64,
    revision: i64,
    delivery: String,
    message: String,
    workspace: String,
    hub: String,
    actor: String,
    token: String,
    audit: i64,
}
/// Constructible only by the fixed Core adapter (or private unit tests).
/// No Deserialize or public constructor: a browser cannot supply a body/proof.
pub struct Refetched {
    intent: RefetchIntent,
    message: Option<Message>,
}
impl RefetchIntent {
    /// Run outside the state actor. No caller-selected message ID, identity or
    /// workspace is accepted here; all expectations came from durable state.
    pub fn fetch(&self, core: &Core) -> Refetched {
        Refetched {
            intent: self.clone(),
            message: if core.is_identity(&self.hub) {
                core.get_bound_message(&self.message, &self.endpoint, &self.workspace)
                    .ok()
            } else {
                None
            },
        }
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefetchOutcome {
    Accepted,
    RetrievalUnavailable,
    QueueFull,
    InvalidFragment,
    TransferExpired,
}
impl RefetchOutcome {
    fn audit_action(&self) -> &'static str {
        match self {
            Self::Accepted => "refetch_accepted",
            Self::RetrievalUnavailable => "refetch_unavailable",
            Self::QueueFull => "refetch_queue_full",
            Self::InvalidFragment => "refetch_invalid_fragment",
            Self::TransferExpired => "refetch_transfer_expired",
        }
    }
}
impl<M: Mailbox + Directory> Hub<M> {
    fn refetch_schema(&self) -> Result<(), Error> {
        self.recovery_schema()?;
        self.store.db.execute_batch("CREATE TABLE IF NOT EXISTS hub_refetch_leases(endpoint TEXT NOT NULL,generation INTEGER NOT NULL,delivery TEXT NOT NULL,token TEXT NOT NULL,until_at INTEGER NOT NULL,audit INTEGER NOT NULL,PRIMARY KEY(endpoint,generation,delivery));")?;
        Ok(())
    }
    /// Internal verified-owner operation. Only a stored, current-generation
    /// capacity/expiry receipt can be selected. No auto-refetch or arbitrary get.
    /// The runtime supplies a fresh CSPRNG 256-bit lowercase hex lease token.
    pub fn prepare_receipt_refetch(
        &mut self,
        endpoint: &str,
        revision: i64,
        delivery: &str,
        actor: &str,
        token: &str,
        now: i64,
    ) -> Result<RefetchIntent, Error> {
        if !valid_id(actor)
            || !valid_id(delivery)
            || token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || now < TOMBSTONE_TTL
        {
            return Err(Error::Binding);
        }
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        let view = self.active(endpoint, now)?;
        if view.revision != revision {
            return Err(Error::Revision);
        }
        self.refetch_schema()?;
        let generation = view.binding.generation;
        let until = now.checked_add(60).ok_or(Error::Binding)?;
        let tx = self.store.db.transaction()?;
        let receipt:Option<String> = tx.query_row("SELECT message FROM hub_capacity_receipts WHERE endpoint=?1 AND generation=?2 AND delivery=?3 AND recovered_at IS NULL AND at<=?4 AND at>=?5 AND reason IN ('endpoint_hold_full','hub_hold_full','hold_expired')",params![endpoint,generation,delivery,now,now-TOMBSTONE_TTL],|r|r.get(0)).optional()?;
        let message = receipt.ok_or(Error::Binding)?;
        let leased:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM hub_refetch_leases WHERE endpoint=?1 AND generation=?2 AND delivery=?3 AND until_at>?4)",params![endpoint,generation,delivery,now],|r|r.get(0))?;
        if leased {
            return Err(Error::Binding);
        }
        let audits: i64 = tx.query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
        // A completion also needs an audit slot; it checks again before save.
        if audits >= AUDIT_LIMIT - 1 {
            return Err(StorageError::Full.into());
        }
        tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,'refetch_started',?2,?3,?4,?5)",params![actor,endpoint,delivery,revision,now])?;
        let audit = tx.last_insert_rowid();
        tx.execute("INSERT INTO hub_refetch_leases(endpoint,generation,delivery,token,until_at,audit) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(endpoint,generation,delivery) DO UPDATE SET token=excluded.token,until_at=excluded.until_at,audit=excluded.audit",params![endpoint,generation,delivery,token,until,audit])?;
        tx.commit()?;
        Ok(RefetchIntent {
            endpoint: endpoint.into(),
            generation,
            revision,
            delivery: delivery.into(),
            message,
            workspace: self.workspace.clone(),
            hub: self.own_id.clone(),
            actor: actor.into(),
            token: token.into(),
            audit,
        })
    }
    /// State-only completion, after the fixed adapter has fetched outside state.
    /// The started audit is durable BEFORE any fragment save. Fragment save and
    /// final receipt/audit commit are separate transactions: a crash between them
    /// leaves a visible started attempt, and retry uses normal hash dedup.
    pub fn complete_receipt_refetch(
        &mut self,
        result: Refetched,
        now: i64,
    ) -> Result<RefetchOutcome, Error> {
        let intent = result.intent;
        if self.blocked()? {
            return Err(Error::FailureBudget);
        }
        if self.own_id != intent.hub || self.workspace != intent.workspace {
            return Err(Error::Binding);
        }
        let view = self.active(&intent.endpoint, now)?;
        if view.revision != intent.revision {
            return Err(Error::Revision);
        }
        if view.binding.generation != intent.generation {
            return Err(Error::Binding);
        }
        self.refetch_schema()?;
        let valid:bool = self.store.db.query_row("SELECT EXISTS(SELECT 1 FROM hub_refetch_leases l JOIN hub_capacity_receipts r USING(endpoint,generation,delivery) JOIN hub_owner_audit a ON a.id=l.audit WHERE l.endpoint=?1 AND l.generation=?2 AND l.delivery=?3 AND l.token=?4 AND l.audit=?5 AND l.until_at>?6 AND a.at<=?6 AND r.message=?7 AND r.recovered_at IS NULL)",params![intent.endpoint,intent.generation,intent.delivery,intent.token,intent.audit,now,intent.message],|r|r.get(0))?;
        if !valid {
            return Err(Error::Binding);
        }
        let audits: i64 =
            self.store
                .db
                .query_row("SELECT COUNT(*) FROM hub_owner_audit", [], |r| r.get(0))?;
        if audits >= AUDIT_LIMIT {
            return Err(StorageError::Full.into());
        }
        let outcome = if let Some(message) = result.message {
            // Defense in depth; only get_bound_message creates production proof.
            if message.message_id != intent.message
                || message.sender.id != intent.endpoint
                || message.sender.kind != crate::protocol::SenderKind::Agent
            {
                return Err(Error::Binding);
            }
            match self.store.save_fragment(
                &intent.endpoint,
                intent.generation,
                Direction::Outbound,
                &message.body,
                now,
            ) {
                Ok(FragmentReceipt::Expired) => RefetchOutcome::TransferExpired,
                Ok(_) => RefetchOutcome::Accepted,
                Err(StorageError::Full) => RefetchOutcome::QueueFull,
                Err(StorageError::Invalid | StorageError::Conflict) => {
                    RefetchOutcome::InvalidFragment
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            RefetchOutcome::RetrievalUnavailable
        };
        let tx = self.store.db.transaction()?;
        if outcome == RefetchOutcome::Accepted {
            tx.execute("UPDATE hub_capacity_receipts SET recovered_at=?4 WHERE endpoint=?1 AND generation=?2 AND delivery=?3 AND recovered_at IS NULL",params![intent.endpoint,intent.generation,intent.delivery,now])?;
        }
        tx.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES(?1,?2,?3,?4,?5,?6)",params![intent.actor,outcome.audit_action(),intent.endpoint,intent.delivery,intent.revision,now])?;
        tx.execute("DELETE FROM hub_refetch_leases WHERE endpoint=?1 AND generation=?2 AND delivery=?3 AND token=?4 AND audit=?5",params![intent.endpoint,intent.generation,intent.delivery,intent.token,intent.audit])?;
        tx.commit()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Delivery, Principal},
        hub::{Binding, Readback},
        protocol::{Envelope, Sender, SenderKind},
        recovery::tests::{metadata, NoIo},
        store::{Limits, Store},
    };
    const NOW: i64 = 1_800_000_000;
    fn wire(n: usize) -> String {
        Envelope::Outbound {
            delivery_id: format!("del_original_{n}"),
            message_id: format!("msg_original_{n}"),
            original_sender: Sender {
                kind: SenderKind::Agent,
                id: "ag_recipient".into(),
            },
            body: "synthetic private body".into(),
            context_ids: vec![],
        }
        .fragments("ag_endpoint", 1)
        .unwrap()
        .remove(0)
    }
    fn message() -> Message {
        Message {
            message_id: "msg_fragment_20".into(),
            sender: Principal {
                kind: SenderKind::Agent,
                id: "ag_endpoint".into(),
            },
            body: wire(20),
            context_ids: vec![],
        }
    }
    fn activate(hub: &mut Hub<NoIo>, now: i64) {
        let rev = hub.view("ag_endpoint").unwrap().revision;
        hub.resume_after_readback(
            "ag_endpoint",
            rev,
            Readback {
                observed: Some(metadata()),
                requested_at: now,
            },
            now,
        )
        .unwrap();
    }
    fn fixture(limits: Limits) -> Hub<NoIo> {
        let mut hub = Hub::new(Store::memory(limits).unwrap(), NoIo, "app_hub", "ws_test").unwrap();
        hub.add_binding(Binding {
            endpoint_id: "ag_endpoint".into(),
            generation: 1,
            recipient: "ag_recipient".into(),
            expected_path: "/team/endpoint".into(),
            expected_cwd: "/workspace/endpoint".into(),
            expected_server_id: "machine_test".into(),
        })
        .unwrap();
        for n in 0..21 {
            hub.admit_core_delivery(
                &Delivery {
                    delivery_id: format!("del_fragment_{n}"),
                    recipient: Principal {
                        kind: SenderKind::Agent,
                        id: "app_hub".into(),
                    },
                    message: Message {
                        message_id: format!("msg_fragment_{n}"),
                        sender: Principal {
                            kind: SenderKind::Agent,
                            id: "ag_endpoint".into(),
                        },
                        body: wire(n),
                        context_ids: vec![],
                    },
                },
                NOW,
            )
            .unwrap();
        }
        assert_eq!(hub.hold_summary("ag_endpoint").unwrap().receipts, 1);
        activate(&mut hub, NOW);
        hub
    }
    fn prepare(hub: &mut Hub<NoIo>, token: char, now: i64) -> RefetchIntent {
        let rev = hub.view("ag_endpoint").unwrap().revision;
        hub.prepare_receipt_refetch(
            "ag_endpoint",
            rev,
            "del_fragment_20",
            "human_owner",
            &token.to_string().repeat(64),
            now,
        )
        .unwrap()
    }
    fn proof(intent: RefetchIntent) -> Refetched {
        Refetched {
            intent,
            message: Some(message()),
        }
    }
    #[test]
    fn overflow_refetch_admits_once_after_resume_and_audit_has_no_body_or_token() {
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        assert_eq!(
            hub.owner_audit(None, 10).unwrap()[0].action,
            "refetch_started"
        );
        assert_eq!(
            hub.complete_receipt_refetch(proof(intent.clone()), NOW)
                .unwrap(),
            RefetchOutcome::Accepted
        );
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW).unwrap().len(), 1);
        assert!(hub.complete_receipt_refetch(proof(intent), NOW).is_err());
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW
            )
            .is_err());
        let audit = serde_json::to_string(&hub.owner_audit(None, 10).unwrap()).unwrap();
        assert!(audit.contains("refetch_accepted"));
        assert!(!audit.contains("synthetic private body"));
        assert!(!audit.contains(&"1".repeat(64)));
    }
    #[test]
    fn unknown_receipt_wrong_revision_paused_and_stale_never_prepare() {
        let mut hub = fixture(Limits::default());
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                99,
                "del_fragment_20",
                "human_owner",
                &"1".repeat(64),
                NOW
            )
            .is_err());
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_arbitrary",
                "human_owner",
                &"1".repeat(64),
                NOW
            )
            .is_err());
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"1".repeat(64),
                NOW + 31
            )
            .is_err());
        hub.pause("ag_endpoint", rev).unwrap();
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"1".repeat(64),
                NOW
            )
            .is_err());
        assert!(hub.owner_audit(None, 10).unwrap().is_empty());
    }
    #[test]
    fn stale_result_pause_expiry_and_replaced_lease_cannot_commit() {
        let mut hub = fixture(Limits::default());
        let old = prepare(&mut hub, '1', NOW);
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW + 1
            )
            .is_err());
        hub.pause("ag_endpoint", rev).unwrap();
        assert!(hub
            .complete_receipt_refetch(proof(old.clone()), NOW)
            .is_err());
        activate(&mut hub, NOW + 61);
        assert!(hub
            .complete_receipt_refetch(proof(old.clone()), NOW + 61)
            .is_err());
        let new = prepare(&mut hub, '2', NOW + 61);
        assert!(hub.complete_receipt_refetch(proof(old), NOW + 61).is_err());
        assert_eq!(
            hub.complete_receipt_refetch(proof(new), NOW + 61).unwrap(),
            RefetchOutcome::Accepted
        );
    }
    #[test]
    fn crash_after_fragment_commit_retries_with_hash_dedup_and_pending_audit() {
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        hub.store.db.execute_batch("CREATE TRIGGER fail_final_audit BEFORE INSERT ON hub_owner_audit WHEN NEW.action='refetch_accepted' BEGIN SELECT RAISE(ABORT,'synthetic disk fault'); END;").unwrap();
        assert!(matches!(
            hub.complete_receipt_refetch(proof(intent.clone()), NOW),
            Err(Error::Storage(_))
        ));
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW).unwrap().len(), 1);
        assert_eq!(hub.owner_audit(None, 10).unwrap().len(), 1);
        let recovered: Option<i64> = hub
            .store
            .db
            .query_row("SELECT recovered_at FROM hub_capacity_receipts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(recovered, None);
        hub.store
            .db
            .execute_batch("DROP TRIGGER fail_final_audit")
            .unwrap();
        assert_eq!(
            hub.complete_receipt_refetch(proof(intent), NOW).unwrap(),
            RefetchOutcome::Accepted
        );
        let count: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn retrieval_failure_and_invalid_frame_are_fixed_audited_outcomes() {
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        assert_eq!(
            hub.complete_receipt_refetch(
                Refetched {
                    intent,
                    message: None
                },
                NOW
            )
            .unwrap(),
            RefetchOutcome::RetrievalUnavailable
        );
        let intent = prepare(&mut hub, '2', NOW);
        let mut bad = message();
        bad.body = "not a fragment".into();
        assert_eq!(
            hub.complete_receipt_refetch(
                Refetched {
                    intent,
                    message: Some(bad)
                },
                NOW
            )
            .unwrap(),
            RefetchOutcome::InvalidFragment
        );
        assert!(hub.ready_outbound("ag_endpoint", NOW).unwrap().is_empty());
        assert_eq!(
            hub.owner_audit(None, 10).unwrap()[0].action,
            "refetch_invalid_fragment"
        );
    }
    #[test]
    fn queue_capacity_and_started_audit_failure_never_bypass_limits() {
        let mut hub = fixture(Limits {
            endpoint_count: 1,
            ..Limits::default()
        });
        hub.store
            .save_fragment("ag_endpoint", 1, Direction::Outbound, &wire(99), NOW)
            .unwrap();
        let intent = prepare(&mut hub, '1', NOW);
        assert_eq!(
            hub.complete_receipt_refetch(proof(intent), NOW).unwrap(),
            RefetchOutcome::QueueFull
        );
        assert_eq!(hub.ready_outbound("ag_endpoint", NOW).unwrap().len(), 1);
        hub.store.db.execute_batch("CREATE TRIGGER fail_start_audit BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'synthetic disk fault'); END;").unwrap();
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(matches!(
            hub.prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW
            ),
            Err(Error::Storage(_))
        ));
        let leases: i64 = hub
            .store
            .db
            .query_row("SELECT COUNT(*) FROM hub_refetch_leases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leases, 0);
    }
    #[test]
    fn real_fixed_adapter_produces_proof_and_wrong_hub_identity_never_runs_cli() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let executable = tmp.path().join("synthetic-treer");
        let response = serde_json::json!({"message":{"message_id":"msg_fragment_20","workspace_id":"ws_test","sender":{"kind":"agent","id":"ag_endpoint"},"recipients":[{"kind":"agent","id":"app_hub"}],"body":wire(20),"context_ids":[]}});
        let code=format!("#!/usr/bin/python3\nimport json,sys\nassert sys.argv[1:]==['message','get','--','msg_fragment_20']\nprint({})\n",serde_json::to_string(&response.to_string()).unwrap());
        std::fs::write(&executable, code).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        let core = Core::new(&executable, "app_hub").unwrap();
        assert_eq!(
            hub.complete_receipt_refetch(intent.fetch(&core), NOW)
                .unwrap(),
            RefetchOutcome::Accepted
        );
        assert_eq!(
            hub.capacity_receipts("ag_endpoint", None, 10).unwrap()[0].recovered_at,
            Some(NOW)
        );
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '2', NOW);
        // A nonexistent executable would fail too, but checking absence of a
        // marker proves identity mismatch is rejected BEFORE invoking any CLI.
        let marker = tmp.path().join("must-not-run");
        std::fs::write(
            &executable,
            format!(
                "#!/usr/bin/python3\nopen({},'w').write('ran')\n",
                serde_json::to_string(marker.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        let wrong = Core::new(&executable, "app_other").unwrap();
        assert_eq!(
            hub.complete_receipt_refetch(intent.fetch(&wrong), NOW)
                .unwrap(),
            RefetchOutcome::RetrievalUnavailable
        );
        assert!(!marker.exists());
    }
    #[test]
    fn expired_invalid_generation_and_old_receipts_cannot_be_resurrected() {
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        // An incomplete transfer's terminal expiry is preserved by normal
        // admission; owner refetch cannot resurrect it or claim success.
        hub.store
            .save_fragment("ag_endpoint", 1, Direction::Outbound, &wire(20), NOW)
            .unwrap();
        hub.store
            .db
            .execute(
                "UPDATE transfers SET state=3,terminal_at=?1,expires=?1",
                [NOW],
            )
            .unwrap();
        assert_eq!(
            hub.complete_receipt_refetch(proof(intent), NOW).unwrap(),
            RefetchOutcome::TransferExpired
        );
        assert_eq!(
            hub.capacity_receipts("ag_endpoint", None, 10).unwrap()[0].recovered_at,
            None
        );
        hub.store
            .db
            .execute(
                "UPDATE hub_capacity_receipts SET at=?1",
                [NOW - TOMBSTONE_TTL - 1],
            )
            .unwrap();
        let rev = hub.view("ag_endpoint").unwrap().revision;
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW
            )
            .is_err());
        hub.store
            .db
            .execute("UPDATE hub_capacity_receipts SET at=?1,generation=2", [NOW])
            .unwrap();
        assert!(hub
            .prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW
            )
            .is_err());
    }
    #[test]
    fn full_audit_blocks_before_core_or_fragment_admission() {
        let mut hub = fixture(Limits::default());
        let intent = prepare(&mut hub, '1', NOW);
        hub.store.db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) SELECT 'human_owner','synthetic','ag_endpoint','del_synthetic',1,1800000000 FROM n;").unwrap();
        assert!(matches!(
            hub.complete_receipt_refetch(proof(intent), NOW),
            Err(Error::Storage(StorageError::Full))
        ));
        assert!(hub.ready_outbound("ag_endpoint", NOW).unwrap().is_empty());
        let rev = hub.view("ag_endpoint").unwrap().revision;
        hub.record_readback(
            "ag_endpoint",
            rev,
            Readback {
                observed: Some(metadata()),
                requested_at: NOW + 61,
            },
            NOW + 61,
        )
        .unwrap();
        assert!(matches!(
            hub.prepare_receipt_refetch(
                "ag_endpoint",
                rev,
                "del_fragment_20",
                "human_owner",
                &"2".repeat(64),
                NOW + 61
            ),
            Err(Error::Storage(StorageError::Full))
        ));
    }
}
