//! Owner declarations are audit evidence, never Core/profile/Policy attestation.
use super::*;
use crate::{
    hub::{Hub, Readback},
    owner_actions::AuditContext,
};
use rusqlite::OptionalExtension;
use std::sync::Arc;

fn conflict(_guard: &str) -> Failure {
    #[cfg(test)]
    eprintln!("test-only resume conflict: {_guard}");
    Failure::Conflict
}
pub(crate) struct Activation {
    pub(crate) enabled: bool,
    pub(crate) email_enabled: bool,
    pub(crate) member_current: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub(crate) account_ready: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}
impl Default for Activation {
    fn default() -> Self {
        Self {
            enabled: false,
            email_enabled: false,
            member_current: Arc::new(|_| false),
            account_ready: Arc::new(|_| false),
        }
    }
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ResumeTarget {
    Telegram(Target),
    Email(EmailTarget),
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmailTarget {
    pub channel: EmailChannel,
    pub contact: crate::email::Contact,
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EmailChannel {
    Email,
}
impl Activation {
    fn ready(&self, target: &ResumeTarget) -> bool {
        match target {
            ResumeTarget::Telegram(t) => (self.account_ready)(&t.account_id),
            ResumeTarget::Email(_) => self.email_enabled,
        }
    }
    fn member(&self, target: &ResumeTarget) -> bool {
        match target {
            ResumeTarget::Telegram(_) => true,
            ResumeTarget::Email(t) => (self.member_current)(&t.contact.member_id),
        }
    }
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResumePreview {
    pins: Preview,
    revision: i64,
    target: ResumeTarget,
    held_fragments: i64,
    oldest_held_at: Option<i64>,
    held_snapshot: String,
    parked_transfers: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirmation_hash: Option<String>,
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
    isolation: crate::recovery::IsolationChoice,
    reference: String,
    #[serde(default)]
    held_action: HeldAction,
    #[serde(default)]
    parked_count_ack: i64,
}
// Unicode 15.0 Cf ranges include bidi overrides/isolates and invisible format
// characters. Reject rather than allowing references to disguise audit text.
fn reference_control(c: char) -> bool {
    c.is_control()
        || matches!(c,
                '\u{ad}' |
                '\u{600}'..='\u{605}' |
                '\u{61c}' |
                '\u{6dd}' |
                '\u{70f}' |
                '\u{890}'..='\u{891}' |
                '\u{8e2}' |
                '\u{180e}' |
                '\u{200b}'..='\u{200f}' |
                '\u{202a}'..='\u{202e}' |
                '\u{2060}'..='\u{2064}' |
                '\u{2066}'..='\u{206f}' |
                '\u{feff}' |
                '\u{fff9}'..='\u{fffb}' |
                '\u{110bd}' |
                '\u{110cd}' |
                '\u{13430}'..='\u{1343f}' |
                '\u{1bca0}'..='\u{1bca3}' |
                '\u{1d173}'..='\u{1d17a}' |
                '\u{e0001}' |
                '\u{e0020}'..='\u{e007f}'
        )
}
impl ResumeEvidence {
    fn valid(&self) -> bool {
        self.parked_count_ack >= 0
            && self.profile_reviewed
            && !self.reference.trim().is_empty()
            && self.reference.len() <= 256
            && !self.reference.chars().any(reference_control)
    }
}
fn target<M: Mailbox + Directory>(hub: &Hub<M>, id: &str) -> Result<ResumeTarget, Failure> {
    if let Some(contact) =
        crate::email::contact(&hub.store, id).map_err(|_| Failure::Unavailable)?
    {
        return Ok(ResumeTarget::Email(EmailTarget {
            channel: EmailChannel::Email,
            contact,
        }));
    }
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
        return Err(conflict("target_table_absent"));
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
    serde_json::from_str(&definition.ok_or_else(|| conflict("target_absent"))?)
        .map(ResumeTarget::Telegram)
        .map_err(|_| Failure::Unavailable)
}
fn backlog<M: Mailbox + Directory>(
    hub: &Hub<M>,
    id: &str,
    generation: i64,
) -> Result<(i64, String, i64, Option<i64>), Failure> {
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
    Ok((
        rows.len() as i64,
        fingerprint,
        parked,
        rows.iter().map(|r| r.2).min(),
    ))
}
/// Core queries happen outside state. A second locked snapshot refuses changes
/// during those queries; confirmation calls this again rather than trusting JSON.
fn observe<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &BindingManager,
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
            return Err(conflict("not_paused"));
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
        return Err(conflict("binding_snapshot_changed"));
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
            return Err(conflict("state_changed_during_observe"));
        }
    }
    Ok((
        ResumePreview {
            pins,
            revision,
            target: expected_target,
            held_fragments: expected_backlog.0,
            oldest_held_at: expected_backlog.3,
            held_snapshot: expected_backlog.1,
            parked_transfers: expected_backlog.2,
            confirmation_hash: None,
        },
        Readback {
            observed: Some(observed),
            requested_at,
        },
    ))
}
fn redacted_preview<M: Mailbox + Directory>(
    shared: &SharedHub<M>,
    preview: &ResumePreview,
) -> Result<ResumePreview, Failure> {
    let bytes = serde_json::to_vec(preview).map_err(|_| Failure::Unavailable)?;
    let mut copy: ResumePreview =
        serde_json::from_slice(&bytes).map_err(|_| Failure::Unavailable)?;
    let key = crate::management::key(&shared.lock().map_err(|_| Failure::Unavailable)?.store.db)
        .map_err(|_| Failure::Unavailable)?;
    copy.confirmation_hash = Some(crate::management::commitment(
        &key,
        "resume-preview-v1",
        &bytes,
    ));
    match &mut copy.target {
        ResumeTarget::Telegram(t) => {
            t.chat_id = 0;
            t.topic_id = None;
            t.allowed_senders.clear();
        }
        ResumeTarget::Email(t) => {
            t.contact.address = "[redacted]".into();
            t.contact.member_id.clear();
        }
    }
    Ok(copy)
}
pub(super) fn preview_resume<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &BindingManager,
    id: &str,
    activation: &Activation,
) -> Result<Outcome, Failure> {
    let (preview, _) = observe(shared, directory, workspace, owner, id)?;
    let account_ready = activation.ready(&preview.target) && activation.member(&preview.target);
    let preview = if owner.kind() == "agent" {
        redacted_preview(shared, &preview)?
    } else {
        preview
    };
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
    owner: &BindingManager,
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
        return Err(conflict("activation_disabled"));
    }
    let (current, readback) = observe(shared, directory, workspace, owner, &endpoint_id)?;
    let comparison = if owner.kind() == "agent" {
        redacted_preview(shared, &current)?
    } else {
        serde_json::from_slice(&serde_json::to_vec(&current).map_err(|_| Failure::Unavailable)?)
            .map_err(|_| Failure::Unavailable)?
    };
    if comparison != expected
        || evidence.parked_count_ack != current.parked_transfers
        || !activation.member(&current.target)
    {
        return Err(conflict("preview_changed"));
    }
    let discard_held = evidence.held_action == HeldAction::Discard;
    let evidence=serde_json::to_string(&serde_json::json!({
        "kind":if owner.kind()=="agent" {"agent_declaration_not_attestation"}else{"owner_declaration_not_attestation"}, "principal_kind":owner.kind(), "profile_reviewed":true,"isolation":evidence.isolation,
        "reference":evidence.reference,"pins_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&current.pins).map_err(|_|Failure::Invalid)?)),
        "target":current.target,"held_action":evidence.held_action,"held_fragments":current.held_fragments,"oldest_held_at":current.oldest_held_at,"held_snapshot":current.held_snapshot,"parked_count_ack":evidence.parked_count_ack
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
                current.oldest_held_at,
            )
        || !activation.ready(&current.target)
    {
        return Err(conflict("state_or_readiness_changed_before_commit"));
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
    fn setup() -> (
        SharedHub<NoIo>,
        DirectoryFixture,
        BindingManager,
        Activation,
    ) {
        setup_store(Store::memory(Limits::default()).unwrap())
    }
    fn setup_store(
        store: Store,
    ) -> (
        SharedHub<NoIo>,
        DirectoryFixture,
        BindingManager,
        Activation,
    ) {
        let (hub, _) = fixture(store);
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
        let owner = BindingManager::synthetic("human_owner", current_time().unwrap() + 600);
        (
            state,
            directory,
            owner,
            Activation {
                enabled: true,
                account_ready: Arc::new(|id| id == "bot_test"),
                ..Activation::default()
            },
        )
    }
    fn request(
        s: &SharedHub<NoIo>,
        d: &DirectoryFixture,
        o: &BindingManager,
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
                isolation: crate::recovery::IsolationChoice::Reviewed,
                reference: "#159 reviewed manifest fixture".into(),
                held_action: HeldAction::Discard,
                parked_count_ack,
            },
        }
    }
    #[test]
    fn agent_resume_all_recipients_requires_reference_masks_target_and_notifies_atomically() {
        let (s, d, _, a) = setup();
        let agent = BindingManager::synthetic_agent("ag_manager", current_time().unwrap() + 600);
        crate::management::register(&s.lock().unwrap().store.db, agent.id()).unwrap();
        let mut r = request(&s, &d, &agent, &a);
        assert!(
            matches!(&r.expected.target,ResumeTarget::Telegram(t) if t.chat_id==0 && t.allowed_senders.is_empty())
        );
        assert!(r.expected.confirmation_hash.is_some());
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        r.evidence.reference.clear();
        assert!(matches!(
            resume(&s, &d, "ws_test", &agent, r, &a),
            Err(Failure::Invalid)
        ));
        let mut r = request(&s, &d, &agent, &a);
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        r.evidence.reference =
            "functional smoke, sandbox folder, same user, isolation untested".into();
        resume(&s, &d, "ws_test", &agent, r, &a).unwrap();
        let h = s.lock().unwrap();
        assert!(!h.view("ag_endpoint").unwrap().paused);
        let (actor, detail): (String, String) = h
            .store
            .db
            .query_row(
                "SELECT actor,transfer FROM hub_owner_audit WHERE action='resume_binding'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(actor, "ag_manager");
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert!(detail["target"]["chat_id"]
            .as_str()
            .unwrap()
            .starts_with("masked:"));
        assert_eq!(detail["isolation"], "unverified_accepted");
        let notice: String = h
            .store
            .db
            .query_row("SELECT recipient FROM hub_management_notices", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notice, "ag_recipient");
    }
    #[test]
    fn agent_binding_cap_and_notification_failure_roll_back_creation() {
        let (s, _, _, _) = setup();
        let mut h = s.lock().unwrap();
        crate::management::register(&h.store.db, "ag_manager").unwrap();
        let base = h.view("ag_endpoint").unwrap().binding;
        for n in 0..5 {
            let mut b = base.clone();
            b.endpoint_id = format!("ag_cap_{n}");
            h.add_binding_audited(b, "ag_manager", current_time().unwrap())
                .unwrap();
        }
        let mut b = base.clone();
        b.endpoint_id = "ag_too_many".into();
        assert!(h
            .add_binding_audited(b, "ag_manager", current_time().unwrap())
            .is_err());
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT count(*) FROM endpoints WHERE id='ag_too_many'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        crate::management::register(&h.store.db, "ag_other").unwrap();
        h.store.db.execute_batch("CREATE TRIGGER reject_management_notice BEFORE INSERT ON hub_management_notices BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        let mut b = base;
        b.endpoint_id = "ag_no_notice".into();
        assert!(h
            .add_binding_audited(b, "ag_other", current_time().unwrap())
            .is_err());
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT count(*) FROM endpoints WHERE id='ag_no_notice'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    #[test]
    fn delete_refuses_pending_or_held_work_and_preserves_replay_tombstone() {
        let (s, _, _, _) = setup();
        let mut h = s.lock().unwrap();
        crate::management::register(&h.store.db, "ag_manager").unwrap();
        let mut binding = h.view("ag_endpoint").unwrap().binding;
        binding.endpoint_id = "ag_empty".into();
        h.add_binding_audited(binding, "ag_manager", current_time().unwrap())
            .unwrap();
        let revision = h.view("ag_empty").unwrap().revision;
        let generation = h.view("ag_empty").unwrap().binding.generation;
        // Synthetic history spans generations; deletion must retire only this endpoint.
        h.store
            .db
            .execute_batch(
                "INSERT INTO telegram_reply_mappings VALUES
            ('fixture',1,0,'ag_empty',1,'ag_recipient',1,'msg_one','job_one','digest',1),
            ('fixture',1,0,'ag_empty',2,'ag_recipient',2,'msg_two','job_two','digest',2),
            ('fixture',1,0,'ag_other',1,'ag_recipient',1,'msg_other','job_other','digest',1);",
            )
            .unwrap();
        h.store.db.execute("INSERT INTO inbound(endpoint,generation,external_id,recipient,body,digest) VALUES('ag_empty',?1,'fixture','ag_recipient','synthetic',x'00')",[generation]).unwrap();
        assert!(h
            .delete_binding_audited("ag_empty", revision, "ag_manager", current_time().unwrap())
            .is_err());
        assert_eq!(
            h.store
                .db
                .query_row("SELECT count(*) FROM telegram_reply_mappings", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT retired_rows FROM telegram_reply_counters",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        h.store
            .db
            .execute("UPDATE inbound SET pending=0", [])
            .unwrap();
        // Failure after row deletion must roll back history, tombstone and revision.
        h.store
            .db
            .execute_batch(
                "CREATE TEMP TRIGGER reject_retirement BEFORE UPDATE OF retired_rows
            ON telegram_reply_counters BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",
            )
            .unwrap();
        assert!(h
            .delete_binding_audited("ag_empty", revision, "ag_manager", current_time().unwrap())
            .is_err());
        assert_eq!(h.view("ag_empty").unwrap().revision, revision);
        assert_eq!(
            h.store
                .db
                .query_row("SELECT count(*) FROM telegram_reply_mappings", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT count(*) FROM hub_retired_bindings WHERE endpoint='ag_empty'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        h.store
            .db
            .execute_batch("DROP TRIGGER reject_retirement")
            .unwrap();
        h.delete_binding_audited("ag_empty", revision, "ag_manager", current_time().unwrap())
            .unwrap();
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT count(*) FROM telegram_reply_mappings WHERE endpoint='ag_empty'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT count(*) FROM telegram_reply_mappings WHERE endpoint='ag_other'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            h.store
                .db
                .query_row(
                    "SELECT retired_rows FROM telegram_reply_counters",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert!(h.view("ag_empty").is_err());
        assert!(!h.binding_ids().unwrap().contains(&"ag_empty".into()));
        assert_eq!(
            h.store
                .db
                .query_row("SELECT count(*) FROM inbound", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(h
            .delete_binding_audited("ag_empty", revision, "ag_manager", current_time().unwrap())
            .is_err());
    }
    #[test]
    fn explicit_isolation_choices_are_required_and_unknown_values_fail_closed() {
        assert!(serde_json::from_str::<ResumeEvidence>(r#"{"profile_reviewed":true,"isolation":"reviewed","isolation":"unverified_accepted","reference":"fixture"}"#).is_err());
        for bad in [
            serde_json::json!(null),
            serde_json::json!(false),
            serde_json::json!(""),
            serde_json::json!("accepted"),
            serde_json::json!({}),
        ] {
            let value =
                serde_json::json!({"profile_reviewed":true,"isolation":bad,"reference":"fixture"});
            assert!(serde_json::from_value::<ResumeEvidence>(value).is_err());
        }
        for legacy in [
            serde_json::json!({"profile_reviewed":true,"reference":"fixture"}),
            serde_json::json!({"profile_reviewed":true,"isolation_reviewed":true,"reference":"fixture"}),
        ] {
            assert!(serde_json::from_value::<ResumeEvidence>(legacy).is_err());
        }
    }
    #[test]
    fn unverified_acceptance_is_audited_and_badge_survives_pause_restart_and_legacy() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().canonicalize().unwrap().join("state");
        std::fs::create_dir(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (s, d, o, a) = setup_store(Store::open(&path, Limits::default()).unwrap());
        assert_eq!(
            s.lock().unwrap().view("ag_endpoint").unwrap().isolation,
            None
        );
        let mut r = request(&s, &d, &o, &a);
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        r.evidence.reference = "functional smoke, same user, isolation untested <script>".into();
        resume(&s, &d, "ws_test", &o, r, &a).unwrap();
        {
            let mut h = s.lock().unwrap();
            assert_eq!(
                h.view("ag_endpoint").unwrap().isolation,
                Some(crate::recovery::IsolationChoice::UnverifiedAccepted)
            );
            let e: serde_json::Value =
                serde_json::from_str(&h.owner_audit(None, 25).unwrap()[0].transfer_id).unwrap();
            assert_eq!(e["isolation"], "unverified_accepted");
            assert_eq!(
                e["reference"],
                "functional smoke, same user, isolation untested <script>"
            );
            h.pause_audited("ag_endpoint", 5, o.id(), current_time().unwrap())
                .unwrap();
            assert_eq!(
                h.view("ag_endpoint").unwrap().isolation,
                Some(crate::recovery::IsolationChoice::UnverifiedAccepted)
            );
        }
        drop(d);
        drop(s);
        let h = Hub::new(
            Store::open(&path, Limits::default()).unwrap(),
            NoIo,
            "app_hub",
            "ws_test",
        )
        .unwrap();
        assert!(h.view("ag_endpoint").unwrap().paused);
        assert_eq!(
            h.view("ag_endpoint").unwrap().isolation,
            Some(crate::recovery::IsolationChoice::UnverifiedAccepted)
        );
        // Additive read compatibility only: old audit declarations remain owner
        // statements, not inferred risk acceptance or live isolation checks.
        h.store
            .db
            .execute(
                "UPDATE hub_owner_audit SET transfer=?1 WHERE action='resume_binding'",
                [r#"{"isolation_reviewed":true}"#],
            )
            .unwrap();
        assert_eq!(
            h.view("ag_endpoint").unwrap().isolation,
            Some(crate::recovery::IsolationChoice::Reviewed)
        );
        h.store
            .db
            .execute(
                "UPDATE hub_owner_audit SET transfer='{}' WHERE action='resume_binding'",
                [],
            )
            .unwrap();
        assert_eq!(h.view("ag_endpoint").unwrap().isolation, None);
    }
    #[test]
    fn unverified_choice_cannot_bypass_gate_profile_or_audit_commit() {
        let (s, d, o, mut a) = setup();
        let mut r = request(&s, &d, &o, &a);
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        r.evidence.profile_reviewed = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Invalid)
        ));
        let mut r = request(&s, &d, &o, &a);
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        a.enabled = false;
        assert!(matches!(
            resume(&s, &d, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        a.enabled = true;
        let mut r = request(&s, &d, &o, &a);
        r.evidence.isolation = crate::recovery::IsolationChoice::UnverifiedAccepted;
        s.lock().unwrap().store.db.execute_batch("CREATE TRIGGER fail_resume BEFORE INSERT ON hub_owner_audit BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
        assert!(resume(&s, &d, "ws_test", &o, r, &a).is_err());
        let h = s.lock().unwrap();
        assert!(h.view("ag_endpoint").unwrap().paused);
        assert_eq!(h.view("ag_endpoint").unwrap().isolation, None);
    }
    #[test]
    fn newer_worker_readback_during_owner_query_conflicts_without_weakening_freshness() {
        struct Supersede<'a> {
            inner: &'a DirectoryFixture,
            once: AtomicBool,
        }
        impl Directory for Supersede<'_> {
            fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
                let original = self.inner.metadata(id)?;
                if id == "ag_endpoint" && !self.once.swap(true, Ordering::SeqCst) {
                    // Deterministic interleaving, not CPU load: an owner query
                    // started in this second returns after a worker query in
                    // the next second has committed. No fake future timestamp.
                    let started = current_time().unwrap();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    while current_time().unwrap() == started {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "test clock did not advance"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    let at = current_time().unwrap();
                    let observed = self.inner.metadata(id)?;
                    let mut h = self.inner.state.lock().unwrap();
                    let revision = h.view(id).unwrap().revision;
                    h.record_readback(
                        id,
                        revision,
                        Readback {
                            observed: Some(observed),
                            requested_at: at,
                        },
                        at,
                    )
                    .unwrap();
                }
                Ok(original)
            }
        }
        let (s, d, o, a) = setup();
        let r = request(&s, &d, &o, &a);
        let revision = s.lock().unwrap().view("ag_endpoint").unwrap().revision;
        let supersede = Supersede {
            inner: &d,
            once: AtomicBool::new(false),
        };
        assert!(matches!(
            resume(&s, &supersede, "ws_test", &o, r, &a),
            Err(Failure::Conflict)
        ));
        let h = s.lock().unwrap();
        let view = h.view("ag_endpoint").unwrap();
        assert!(view.paused);
        assert_eq!(view.revision, revision);
        assert!(h.owner_audit(None, 25).unwrap().is_empty());
        drop(h);
        // An explicit fresh owner preview succeeds; there is no internal
        // automatic retry, overwritten observation or relaxed commit fence.
        let fresh = request(&s, &d, &o, &a);
        resume(&s, &d, "ws_test", &o, fresh, &a).unwrap();
        assert!(!s.lock().unwrap().view("ag_endpoint").unwrap().paused);
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
        assert_eq!(evidence["isolation"], "reviewed");
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
        if let ResumeTarget::Telegram(t) = &mut r.expected.target {
            t.chat_id = -999;
        } else {
            panic!("expected Telegram");
        }
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
        let expired = BindingManager::synthetic("human_owner", 0);
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
            assert!(r.expected.oldest_held_at.is_some());
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
        let e: ResumeEvidence = serde_json::from_value(
            serde_json::json!({"profile_reviewed":true,"isolation":"reviewed","reference":"test"}),
        )
        .unwrap();
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
            "hidden\u{202e}reference".into(),
            "hidden\u{2066}reference".into(),
            "hidden\u{200b}reference".into(),
            "hidden\u{feff}reference".into(),
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
    #[test]
    fn email_binding_checks_membership_outside_state_and_commits_audit_with_revision() {
        let (state, directory, owner, mut a) = setup();
        {
            let h = state.lock().unwrap();
            h.store
                .db
                .execute("DELETE FROM telegram_targets", [])
                .unwrap();
        }
        let original = state.lock().unwrap().view("ag_endpoint").unwrap().revision;
        let contact = crate::email::Contact {
            address: "PERSON@example.test".into(),
            member_id: "human_member".into(),
        };
        let command = |revision| Command::BindEmail {
            endpoint_id: "ag_endpoint".into(),
            revision,
            contact: contact.clone(),
            confirm: true,
        };
        assert!(execute_with_activation(
            &state,
            &directory,
            "ws_test",
            &owner,
            command(original),
            &a
        )
        .is_err());
        a.email_enabled = true;
        let s = state.clone();
        a.member_current = Arc::new(move |id| {
            assert!(s.try_lock().is_ok(), "membership I/O held state");
            id == "human_member"
        });
        execute_with_activation(&state, &directory, "ws_test", &owner, command(original), &a)
            .unwrap();
        let h = state.lock().unwrap();
        assert_eq!(h.view("ag_endpoint").unwrap().revision, original + 1);
        assert!(h.view("ag_endpoint").unwrap().paused);
        assert_eq!(
            crate::email::contact(&h.store, "ag_endpoint")
                .unwrap()
                .unwrap()
                .address,
            "person@example.test"
        );
        assert_eq!(h.owner_audit(None, 1).unwrap()[0].action, "bind_email");
        drop(h);
        assert!(execute_with_activation(
            &state,
            &directory,
            "ws_test",
            &owner,
            command(original),
            &a
        )
        .is_err());
        assert!(execute_with_activation(
            &state,
            &directory,
            "ws_test",
            &owner,
            command(original + 1),
            &a
        )
        .is_err());
        let telegram = crate::telegram::Target {
            account_id: "bot_test".into(),
            chat_id: -100,
            topic_id: None,
            allowed_senders: vec![42],
        };
        assert!(state
            .lock()
            .unwrap()
            .bind_telegram_audited(
                "ag_endpoint",
                original + 1,
                telegram,
                owner.id(),
                current_time().unwrap()
            )
            .is_err());
    }
}
