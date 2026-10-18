//! Owner declarations are audit evidence, never Core/profile/Policy attestation.
use super::*;
use crate::{
    hub::{Hub, Readback},
    owner_actions::AuditContext,
};
use rusqlite::OptionalExtension;
use std::sync::Arc;

pub(crate) struct Activation {
    pub(crate) enabled: bool,
    pub(crate) account_ready: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}
impl Default for Activation {
    fn default() -> Self {
        Self {
            enabled: false,
            account_ready: Arc::new(|_| false),
        }
    }
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResumePreview {
    pins: Preview,
    revision: i64,
    target: Target,
    held_fragments: i64,
    held_snapshot: String,
    parked_transfers: i64,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum HeldAction {
    #[default]
    Discard,
    Release,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeEvidence {
    profile_reviewed: bool,
    isolation_reviewed: bool,
    reference: String,
    #[serde(default)]
    held_action: HeldAction,
    #[serde(default)]
    parked_count_ack: i64,
}
impl ResumeEvidence {
    fn valid(&self) -> bool {
        self.parked_count_ack >= 0
            && self.profile_reviewed
            && self.isolation_reviewed
            && !self.reference.trim().is_empty()
            && self.reference.len() <= 256
            && !self.reference.chars().any(char::is_control)
    }
}
fn target<M: Mailbox + Directory>(hub: &Hub<M>, id: &str) -> Result<Target, Failure> {
    let exists: bool = hub
        .store
        .db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='telegram_targets')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Failure::Unavailable)?;
    if !exists {
        return Err(Failure::Conflict);
    }
    let definition: Option<String> = hub
        .store
        .db
        .query_row(
            "SELECT definition FROM telegram_targets WHERE endpoint=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Failure::Unavailable)?;
    serde_json::from_str(&definition.ok_or(Failure::Conflict)?).map_err(|_| Failure::Unavailable)
}
fn backlog<M: Mailbox + Directory>(
    hub: &Hub<M>,
    id: &str,
    generation: i64,
) -> Result<(i64, String, i64), Failure> {
    hub.hold_schema().map_err(map_hub)?;
    let mut q=hub.store.db.prepare("SELECT delivery,message,created_at,expires FROM hub_holds WHERE endpoint=?1 AND generation=?2 ORDER BY delivery LIMIT 21").map_err(|_|Failure::Unavailable)?;
    let rows = q
        .query_map(rusqlite::params![id, generation], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|_| Failure::Unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Failure::Unavailable)?;
    if rows.len() > 20 {
        return Err(Failure::Unavailable);
    }
    // Metadata fingerprint only: deliberately excludes held text/body digests.
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(id, generation, &rows)).map_err(|_| Failure::Unavailable)?
        )
    );
    let parked = hub
        .store
        .db
        .query_row(
            "SELECT COUNT(*) FROM hub_parked WHERE endpoint=?1 AND generation=?2",
            rusqlite::params![id, generation],
            |r| r.get(0),
        )
        .map_err(|_| Failure::Unavailable)?;
    Ok((rows.len() as i64, fingerprint, parked))
}
/// Core queries happen outside state. A second locked snapshot refuses changes
/// during those queries; confirmation calls this again rather than trusting JSON.
fn observe<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &Owner,
    id: &str,
) -> Result<(ResumePreview, Readback), Failure> {
    let (binding, revision, expected_target, expected_backlog) = {
        let hub = shared.lock().map_err(|_| Failure::Unavailable)?;
        check_owner(owner)?;
        hub.store
            .recheck_private_storage()
            .map_err(|_| Failure::Unavailable)?;
        let v = hub.view(id).map_err(map_hub)?;
        if !v.paused {
            return Err(Failure::Conflict);
        }
        let pending = backlog(&hub, id, v.binding.generation)?;
        (v.binding, v.revision, target(&hub, id)?, pending)
    };
    let requested_at = current_time()?;
    let (pins, current_binding) = preview(
        directory,
        workspace,
        id,
        binding.generation,
        &binding.recipient,
    )?;
    if binding != current_binding {
        return Err(Failure::Conflict);
    }
    // Retain the actual Core observation, not a browser-shaped reconstruction.
    let observed = directory.metadata(id).map_err(|_| Failure::Unavailable)?;
    {
        let hub = shared.lock().map_err(|_| Failure::Unavailable)?;
        check_owner(owner)?;
        let v = hub.view(id).map_err(map_hub)?;
        if !v.paused
            || v.revision != revision
            || target(&hub, id)? != expected_target
            || backlog(&hub, id, binding.generation)? != expected_backlog
        {
            return Err(Failure::Conflict);
        }
    }
    Ok((
        ResumePreview {
            pins,
            revision,
            target: expected_target,
            held_fragments: expected_backlog.0,
            held_snapshot: expected_backlog.1,
            parked_transfers: expected_backlog.2,
        },
        Readback {
            observed: Some(observed),
            requested_at,
        },
    ))
}
pub(super) fn preview_resume<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &Owner,
    id: &str,
    activation: &Activation,
) -> Result<Outcome, Failure> {
    let (preview, _) = observe(shared, directory, workspace, owner, id)?;
    let account_ready = (activation.account_ready)(&preview.target.account_id);
    Ok(Outcome::ResumePreview {
        preview,
        activation_enabled: activation.enabled,
        account_ready,
    })
}
pub(super) fn resume<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &Owner,
    request: ResumeRequest,
    activation: &Activation,
) -> Result<Outcome, Failure> {
    let ResumeRequest {
        endpoint_id,
        expected,
        evidence,
        confirm,
    } = request;
    if !confirm || !evidence.valid() {
        return Err(Failure::Invalid);
    }
    if !activation.enabled {
        return Err(Failure::Conflict);
    }
    let (current, readback) = observe(shared, directory, workspace, owner, &endpoint_id)?;
    if current != expected || evidence.parked_count_ack != current.parked_transfers {
        return Err(Failure::Conflict);
    }
    let discard_held = evidence.held_action == HeldAction::Discard;
    let evidence=serde_json::to_string(&serde_json::json!({
        "kind":"owner_declaration_not_attestation", "profile_reviewed":true,"isolation_reviewed":true,
        "reference":evidence.reference,"pins_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&current.pins).map_err(|_|Failure::Invalid)?)),
        "target":current.target,"held_action":evidence.held_action,"held_fragments":current.held_fragments,"held_snapshot":current.held_snapshot,"parked_count_ack":evidence.parked_count_ack
    })).map_err(|_|Failure::Invalid)?;
    let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
    let now = check_owner(owner)?;
    let view = hub.view(&endpoint_id).map_err(map_hub)?;
    if !view.paused
        || view.revision != current.revision
        || target(&hub, &endpoint_id)? != current.target
        || backlog(&hub, &endpoint_id, view.binding.generation)?
            != (
                current.held_fragments,
                current.held_snapshot.clone(),
                current.parked_transfers,
            )
        || !(activation.account_ready)(&current.target.account_id)
    {
        return Err(Failure::Conflict);
    }
    let context = AuditContext::with_detail(owner.id(), now, &evidence).map_err(map_hub)?;
    hub.resume_with_context(
        &endpoint_id,
        current.revision,
        readback,
        context,
        now,
        discard_held,
    )
    .map_err(map_hub)?;
    saved(&hub, &endpoint_id)
}
pub(super) struct ResumeRequest {
    pub endpoint_id: String,
    pub expected: ResumePreview,
    pub evidence: ResumeEvidence,
    pub confirm: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::AgentMetadata,
        recovery::tests::{fixture, metadata, NoIo},
        store::{Limits, Store},
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };
    struct DirectoryFixture {
        state: SharedHub<NoIo>,
        drift: AtomicBool,
        unavailable: AtomicBool,
    }
    impl Directory for DirectoryFixture {
        fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
            assert!(self.state.try_lock().is_ok(), "Core query held hub state");
            if self.unavailable.load(Ordering::Relaxed) {
                return Err(crate::core::Error::Unavailable);
            }
            let mut m = metadata();
            m.agent_id = id.into();
            if id == "ag_recipient" {
                m.path = Some("/team/recipient".into());
            } else if self.drift.load(Ordering::Relaxed) {
                m.path = Some("/moved/endpoint".into());
            }
            Ok(m)
        }
    }
    fn setup() -> (SharedHub<NoIo>, DirectoryFixture, Owner, Activation) {
        let (hub, _) = fixture(Store::memory(Limits::default()).unwrap());
        hub.store
            .db
            .execute("UPDATE hub_bindings SET checked_at=NULL", [])
            .unwrap();
        hub.recovery_schema().unwrap();
        let state = Arc::new(Mutex::new(hub));
        let directory = DirectoryFixture {
            state: state.clone(),
            drift: false.into(),
            unavailable: false.into(),
        };
        let owner = Owner::synthetic("human_owner", current_time().unwrap() + 600);
        (
            state,
            directory,
            owner,
            Activation {
                enabled: true,
                account_ready: Arc::new(|id| id == "bot_test"),
            },
        )
    }
    fn request(
        s: &SharedHub<NoIo>,
        d: &DirectoryFixture,
        o: &Owner,
        a: &Activation,
    ) -> ResumeRequest {
        let Outcome::ResumePreview { preview, .. } =
            preview_resume(s, d, "ws_test", o, "ag_endpoint", a).unwrap()
        else {
            panic!("wrong preview")
        };
        let parked_count_ack = preview.parked_transfers;
        ResumeRequest {
            endpoint_id: "ag_endpoint".into(),
            expected: preview,
            confirm: true,
            evidence: ResumeEvidence {
                profile_reviewed: true,
                isolation_reviewed: true,
                reference: "#159 reviewed manifest fixture".into(),
                held_action: HeldAction::Discard,
                parked_count_ack,
            },
        }
    }
    #[test]
    fn default_off_and_unready_account_cannot_activate_even_with_owner_confirmation() {
        let (s, d, o, mut a) = setup();
        let r = request(&s, &d, &o, &a);
        a.enabled = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        a.enabled = true;
        let r = request(&s, &d, &o, &a);
        a.account_ready = Arc::new(|_| false);
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        assert!(s.lock().unwrap().view("ag_endpoint").unwrap().paused);
        assert!(s.lock().unwrap().owner_audit(None, 25).unwrap().is_empty());
    }
    #[test]
    fn resume_rechecks_live_pins_and_atomically_records_owner_declarations() {
        let (s, d, o, a) = setup();
        let r = request(&s, &d, &o, &a);
        let before: i64 = s
            .lock()
            .unwrap()
            .store
            .db
            .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get(0))
            .unwrap();
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Ok(Outcome::Saved {
                paused: false,
                revision: 5
            })
        ));
        let h = s.lock().unwrap();
        assert!(!h.view("ag_endpoint").unwrap().paused);
        let events = h.owner_audit(None, 25).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "resume_binding");
        assert_eq!(events[0].actor_id, "human_owner");
        let evidence: serde_json::Value = serde_json::from_str(&events[0].transfer_id).unwrap();
        assert_eq!(evidence["kind"], "owner_declaration_not_attestation");
        assert_eq!(evidence["reference"], "#159 reviewed manifest fixture");
        assert_eq!(evidence["target"]["account_id"], "bot_test");
        assert_eq!(evidence["pins_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            h.store
                .db
                .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before
        );
    }
    #[test]
    fn confirmations_snapshot_drift_expiry_and_audit_fault_all_fail_closed() {
        let (s, d, o, a) = setup();
        let mut r = request(&s, &d, &o, &a);
        r.evidence.profile_reviewed = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Invalid)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.evidence.isolation_reviewed = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Invalid)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.confirm = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Invalid)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.expected.revision += 1;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.expected.target.chat_id = -999;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        let r = request(&s, &d, &o, &a);
        d.drift.store(true, Ordering::Relaxed);
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        d.drift.store(false, Ordering::Relaxed);
        let r = request(&s, &d, &o, &a);
        d.unavailable.store(true, Ordering::Relaxed);
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Unavailable)
        ));
        d.unavailable.store(false, Ordering::Relaxed);
        let r = request(&s, &d, &o, &a);
        let expired = Owner::synthetic("human_owner", 0);
        assert!(matches!(
            resume(&s, &d, "ws_test", &expired, r, &a),
            Err(Failure::Expired)
        ));
        let r = request(&s, &d, &o, &a);
        s.lock().unwrap().store.db.execute_batch("CREATE TRIGGER reject_resume BEFORE INSERT ON hub_owner_audit WHEN NEW.action='resume_binding' BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;").unwrap();
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Unavailable)
        ));
        let h = s.lock().unwrap();
        assert!(h.view("ag_endpoint").unwrap().paused);
        assert_eq!(h.view("ag_endpoint").unwrap().revision, 4);
        assert!(h.owner_audit(None, 25).unwrap().is_empty());
    }
    fn seed_hold(s: &SharedHub<NoIo>, n: i64) {
        let h = s.lock().unwrap();
        h.hold_schema().unwrap();
        let now = current_time().unwrap();
        h.store.db.execute("INSERT INTO hub_holds(endpoint,generation,delivery,message,wire,created_at,expires) VALUES('ag_endpoint',1,?1,?2,'synthetic held text',?3,?4)",rusqlite::params![format!("del_held_{n}"),format!("msg_held_{n}"),now,now+3600]).unwrap();
    }
    #[test]
    fn held_discard_is_default_explicit_release_preserves_and_audit_records_choice() {
        for release in [false, true] {
            let (s, d, o, a) = setup();
            seed_hold(&s, 1);
            let mut r = request(&s, &d, &o, &a);
            assert_eq!(r.expected.held_fragments, 1);
            if release {
                r.evidence.held_action = HeldAction::Release;
            }
            assert!(resume(&s, &d, "ws_test", &o, r, &a).is_ok());
            let h = s.lock().unwrap();
            assert_eq!(
                h.store
                    .db
                    .query_row("SELECT COUNT(*) FROM hub_holds", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                if release { 1 } else { 0 }
            );
            assert_eq!(h.store.db.query_row("SELECT COUNT(*) FROM hub_capacity_receipts WHERE reason='owner_discarded_hold'",[],|r|r.get::<_,i64>(0)).unwrap(),if release {0}else{1});
            let e: serde_json::Value =
                serde_json::from_str(&h.owner_audit(None, 25).unwrap()[0].transfer_id).unwrap();
            assert_eq!(
                e["held_action"],
                if release { "release" } else { "discard" }
            );
            assert_eq!(e["held_fragments"], 1);
            assert_eq!(e["parked_count_ack"], 1);
        }
        let e:ResumeEvidence=serde_json::from_value(serde_json::json!({"profile_reviewed":true,"isolation_reviewed":true,"reference":"test"})).unwrap();
        assert!(e.held_action == HeldAction::Discard);
    }
    #[test]
    fn backlog_changes_unacknowledged_parks_quota_and_failed_audit_preserve_pause_and_holds() {
        let (s, d, o, a) = setup();
        seed_hold(&s, 1);
        let r = request(&s, &d, &o, &a);
        seed_hold(&s, 2);
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.evidence.parked_count_ack = 0;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        let r = request(&s, &d, &o, &a);
        s.lock().unwrap().store.db.execute_batch("CREATE TRIGGER reject_resume BEFORE INSERT ON hub_owner_audit WHEN NEW.action='resume_binding' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(resume(&s, &d, "ws_test", &o, r, &a).is_err());
        {
            let h = s.lock().unwrap();
            assert!(h.view("ag_endpoint").unwrap().paused);
            assert_eq!(
                h.store
                    .db
                    .query_row("SELECT COUNT(*) FROM hub_holds", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2
            );
            h.store
                .db
                .execute_batch("DROP TRIGGER reject_resume")
                .unwrap();
            for _ in 0..10 {
                h.store.db.execute("INSERT INTO hub_owner_audit(actor,action,endpoint,transfer,revision,at) VALUES('human_owner','resume_binding','ag_endpoint','',4,?1)",[current_time().unwrap()]).unwrap();
            }
        }
        let r = request(&s, &d, &o, &a);
        assert!(resume(&s, &d, "ws_test", &o, r, &a).is_err());
        assert!(s.lock().unwrap().view("ag_endpoint").unwrap().paused);
    }
    #[test]
    fn discarded_held_delivery_replay_does_not_readmit_text_and_id_collision_is_rejected() {
        use crate::core::{Delivery, Message, Principal};
        use crate::protocol::SenderKind;
        let (s, d, o, a) = setup();
        seed_hold(&s, 1);
        let r = request(&s, &d, &o, &a);
        resume(&s, &d, "ws_test", &o, r, &a).unwrap();
        let mut h = s.lock().unwrap();
        let before = h
            .store
            .db
            .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let mut delivery = Delivery {
            delivery_id: "del_held_1".into(),
            recipient: Principal {
                kind: SenderKind::Agent,
                id: "app_hub".into(),
            },
            message: Message {
                message_id: "msg_held_1".into(),
                sender: Principal {
                    kind: SenderKind::Agent,
                    id: "ag_endpoint".into(),
                },
                body: "synthetic discarded held text".into(),
                context_ids: vec![],
            },
        };
        h.admit_core_delivery(&delivery, current_time().unwrap())
            .unwrap();
        assert_eq!(
            h.store
                .db
                .query_row("SELECT COUNT(*) FROM fragments", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            h.store
                .db
                .query_row("SELECT COUNT(*) FROM hub_holds", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        delivery.message.message_id = "msg_collision".into();
        assert!(h
            .admit_core_delivery(&delivery, current_time().unwrap())
            .is_err());
    }
    #[test]
    fn discard_receipt_capacity_is_checked_atomically_and_evidence_is_bounded_plain_text() {
        let (s, d, o, a) = setup();
        seed_hold(&s, 1);
        s.lock().unwrap().store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO hub_capacity_receipts(endpoint,generation,delivery,message,reason,at) SELECT 'ag_endpoint',1,'del_receipt_'||x,'msg_receipt_'||x,'hold_expired',1 FROM n;").unwrap();
        let r = request(&s, &d, &o, &a);
        assert!(resume(&s, &d, "ws_test", &o, r, &a).is_err());
        {
            let h = s.lock().unwrap();
            assert!(h.view("ag_endpoint").unwrap().paused);
            assert!(h.owner_audit(None, 25).unwrap().is_empty());
            assert_eq!(
                h.store
                    .db
                    .query_row("SELECT COUNT(*) FROM hub_holds", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        for invalid in [
            "".to_owned(),
            " ".into(),
            "a".repeat(257),
            "line\nnext".into(),
            "字".repeat(100),
        ] {
            let mut r = request(&s, &d, &o, &a);
            r.evidence.reference = invalid;
            assert!(matches!(
                resume(&s, &d, "ws_test", &o, r, &a),
                Err(Failure::Invalid)
            ));
        }
    }
}
