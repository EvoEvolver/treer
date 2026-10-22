//! Verified owner/Agent binding configuration commands. No transport token, caller identity, or
//! claimed Core metadata is accepted from JSON. New endpoints remain paused.
use crate::{
    core::Core,
    hub::{Binding, Directory},
    owner_auth::BindingManager,
    relay::Mailbox,
    scheduler::SharedHub,
    telegram::Target,
};
mod activation;
pub(crate) use activation::Activation;
pub use activation::{ResumeEvidence, ResumePreview};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    PreviewCreate {
        endpoint_id: String,
        generation: i64,
        recipient: String,
    },
    Create {
        #[serde(default)]
        idempotency_key: Option<String>,
        endpoint_id: String,
        generation: i64,
        recipient: String,
        confirm: bool,
        expected: Preview,
    },
    Delete {
        endpoint_id: String,
        revision: i64,
        confirm: bool,
    },
    Pause {
        endpoint_id: String,
        revision: i64,
        confirm: bool,
    },
    PreviewResume {
        endpoint_id: String,
    },
    Resume {
        endpoint_id: String,
        expected: ResumePreview,
        evidence: ResumeEvidence,
        confirm: bool,
    },
    ExportMetadata {
        kind: crate::archive::Kind,
        endpoint_id: Option<String>,
    },
    ClearMetadata {
        archive: crate::archive::Clear,
        archive_saved: bool,
        confirm: bool,
    },
    InspectRecovery {
        endpoint_id: String,
        before_audit: Option<i64>,
    },
    RecoverParked {
        endpoint_id: String,
        revision: i64,
        transfer_id: String,
        operation: crate::recovery::Recovery,
        confirm: bool,
    },
    InspectEmailDelivery {
        endpoint_id: String,
    },
    AbandonPreparedEmail {
        endpoint_id: String,
        revision: i64,
        intent_id: String,
        confirm: bool,
    },
    BindEmail {
        endpoint_id: String,
        revision: i64,
        contact: crate::email::Contact,
        confirm: bool,
    },
    BindTelegram {
        endpoint_id: String,
        revision: i64,
        target: Target,
        confirm: bool,
    },
}
impl Command {
    pub(crate) fn consumes_write_quota(&self) -> bool {
        !matches!(
            self,
            Self::PreviewCreate { .. }
                | Self::Pause { .. }
                | Self::InspectRecovery { .. }
                | Self::InspectEmailDelivery { .. }
                | Self::ExportMetadata { .. }
                | Self::PreviewResume { .. }
        )
    }
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    endpoint_id: String,
    generation: i64,
    recipient: String,
    expected_path: String,
    expected_cwd_sha256: String,
    expected_server_id: String,
    recipient_path: Option<String>,
}
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Preview {
        preview: Preview,
    },
    Saved {
        revision: i64,
        paused: bool,
    },
    ResumePreview {
        preview: ResumePreview,
        activation_enabled: bool,
        account_ready: bool,
    },
    MetadataArchive {
        archive: crate::archive::Export,
    },
    MetadataCleared,
    Deleted {
        endpoint_id: String,
    },
    EmailDeliveryStatus {
        endpoint_id: String,
        revision: i64,
        paused: bool,
        delivery: crate::email::delivery::DispatchStatus,
        items: Vec<crate::email::delivery::RecoveryItem>,
        truncated: bool,
    },
    RecoveryStatus {
        endpoint_id: String,
        revision: i64,
        paused: bool,
        parked: Vec<crate::recovery::Parked>,
        receipts: Vec<crate::failures::Failure>,
        audit: Vec<crate::recovery::Audit>,
        next_audit_before: Option<i64>,
    },
}
fn preview<D: Directory>(
    directory: &D,
    workspace: &str,
    endpoint_id: &str,
    generation: i64,
    recipient: &str,
) -> Result<(Preview, Binding), Failure> {
    if generation <= 0
        || endpoint_id == recipient
        || !endpoint_id.starts_with("ag_")
        || !recipient.starts_with("ag_")
        || !crate::store::valid_id(endpoint_id)
        || !crate::store::valid_id(recipient)
    {
        return Err(Failure::InvalidPreview("invalid_binding_identity"));
    }
    let endpoint = directory
        .metadata(endpoint_id)
        .map_err(|_| Failure::Unavailable)?;
    let recipient_meta = directory
        .metadata(recipient)
        .map_err(|_| Failure::Unavailable)?;
    if endpoint.agent_id != endpoint_id {
        return Err(Failure::InvalidPreview("endpoint_identity_mismatch"));
    }
    if recipient_meta.agent_id != recipient {
        return Err(Failure::InvalidPreview("recipient_identity_mismatch"));
    }
    if endpoint.workspace_id != workspace {
        return Err(Failure::InvalidPreview("endpoint_workspace_mismatch"));
    }
    if recipient_meta.workspace_id != workspace {
        return Err(Failure::InvalidPreview("recipient_workspace_mismatch"));
    }
    // The endpoint runs the fixed relay command; its kind is also checked by
    // Hub readback/resume. Recipients only need a Core mailbox, so shell and
    // other Agent runtime kinds are valid recipients.
    if endpoint.kind != "command" {
        return Err(Failure::InvalidPreview("endpoint_kind_unsupported"));
    }
    let path = endpoint
        .path
        .ok_or(Failure::InvalidPreview("endpoint_unplaced"))?;
    let preview = Preview {
        endpoint_id: endpoint_id.into(),
        generation,
        recipient: recipient.into(),
        expected_path: path.clone(),
        expected_cwd_sha256: format!("{:x}", Sha256::digest(endpoint.cwd.as_bytes())),
        expected_server_id: endpoint.server_id.clone(),
        recipient_path: recipient_meta.path,
    };
    let binding = Binding {
        endpoint_id: endpoint_id.into(),
        generation,
        recipient: recipient.into(),
        expected_path: path,
        expected_cwd: endpoint.cwd,
        expected_server_id: endpoint.server_id,
    };
    Ok((preview, binding))
}
fn saved<M: Mailbox + Directory>(hub: &crate::hub::Hub<M>, id: &str) -> Result<Outcome, Failure> {
    let view = hub.view(id).map_err(map_hub)?;
    Ok(Outcome::Saved {
        revision: view.revision,
        paused: view.paused,
    })
}
fn recovery_status<M: Mailbox + Directory>(
    hub: &crate::hub::Hub<M>,
    endpoint_id: &str,
    before_audit: Option<i64>,
) -> Result<Outcome, Failure> {
    if before_audit.is_some_and(|id| id <= 0) {
        return Err(Failure::Invalid);
    }
    let view = hub.view(endpoint_id).map_err(map_hub)?;
    let parked = hub.parked_transfers(endpoint_id, 100).map_err(map_hub)?;
    let receipts = hub
        .store
        .recent_failures(endpoint_id, view.binding.generation, 25)
        .map_err(|_| Failure::Unavailable)?;
    let audit = hub.owner_audit(before_audit, 25).map_err(map_hub)?;
    let next_audit_before =
        (audit.len() == 25).then(|| audit.last().expect("nonempty bounded page").id);
    Ok(Outcome::RecoveryStatus {
        endpoint_id: endpoint_id.into(),
        revision: view.revision,
        paused: view.paused,
        parked,
        receipts,
        audit,
        next_audit_before,
    })
}
fn email_delivery_status<M: Mailbox + Directory>(
    hub: &mut crate::hub::Hub<M>,
    endpoint: &str,
) -> Result<Outcome, Failure> {
    let view = hub.view(endpoint).map_err(map_hub)?;
    if crate::email::contact(&hub.store, endpoint)
        .map_err(|_| Failure::Unavailable)?
        .is_none()
    {
        return Err(Failure::Conflict);
    }
    let q = crate::email::Queue::new(&mut hub.store).map_err(|_| Failure::Unavailable)?;
    let delivery = q
        .proxy_delivery_status(endpoint)
        .map_err(|_| Failure::Unavailable)?;
    let (items, truncated) = q
        .proxy_recovery_items(endpoint)
        .map_err(|_| Failure::Unavailable)?;
    Ok(Outcome::EmailDeliveryStatus {
        endpoint_id: endpoint.into(),
        revision: view.revision,
        paused: view.paused,
        delivery,
        items,
        truncated,
    })
}
#[derive(Clone, Copy, Debug)]
pub enum Failure {
    Invalid,
    InvalidPreview(&'static str),
    Conflict,
    Held,
    Unavailable,
    Expired,
    Forbidden,
}
fn current_time() -> Result<i64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_secs()).ok())
        .ok_or(Failure::Unavailable)
}
fn map_hub(error: crate::hub::Error) -> Failure {
    #[cfg(test)]
    eprintln!("test-only configuration hub rejection: {error}");
    match error {
        crate::hub::Error::BindingHeld => Failure::Held,
        crate::hub::Error::Revision | crate::hub::Error::Binding => Failure::Conflict,
        _ => Failure::Unavailable,
    }
}
fn check_owner(owner: &BindingManager) -> Result<i64, Failure> {
    let now = current_time()?;
    if now >= owner.expires_at() {
        return Err(Failure::Expired);
    }
    Ok(now)
}
/// Only the HTTP boundary can supply BindingManager after current Proxy verification.
/// Directory calls run before taking the hub mutex; failures never install
/// caller-supplied paths or partially create an endpoint.
pub fn execute<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &BindingManager,
    command: Command,
) -> Result<Outcome, Failure> {
    execute_with_activation(
        shared,
        directory,
        workspace,
        owner,
        command,
        &Activation::default(),
    )
}
fn execute_with_activation<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &BindingManager,
    command: Command,
    activation: &Activation,
) -> Result<Outcome, Failure> {
    check_owner(owner)?;
    {
        let h = shared.lock().map_err(|_| Failure::Unavailable)?;
        h.store
            .recheck_private_storage()
            .map_err(|_| Failure::Unavailable)?;
        if owner.kind() == "agent" {
            crate::management::register(&h.store.db, owner.id())
                .map_err(|_| Failure::Unavailable)?;
        }
    }
    if owner.kind() == "agent"
        && !matches!(
            &command,
            Command::Delete { .. }
                | Command::PreviewCreate { .. }
                | Command::Create { .. }
                | Command::Pause { .. }
                | Command::PreviewResume { .. }
                | Command::Resume { .. }
                | Command::BindEmail { .. }
                | Command::BindTelegram { .. }
        )
    {
        return Err(Failure::Forbidden);
    }
    match command {
        Command::PreviewResume { endpoint_id } => activation::preview_resume(
            shared,
            directory,
            workspace,
            owner,
            &endpoint_id,
            activation,
        ),
        Command::Resume {
            endpoint_id,
            expected,
            evidence,
            confirm,
        } => activation::resume(
            shared,
            directory,
            workspace,
            owner,
            activation::ResumeRequest {
                endpoint_id,
                expected,
                evidence,
                confirm,
            },
            activation,
        ),
        Command::PreviewCreate {
            endpoint_id,
            generation,
            recipient,
        } => {
            shared
                .lock()
                .map_err(|_| Failure::Unavailable)?
                .ensure_not_terminal_held(&endpoint_id)
                .map_err(map_hub)?;
            let (preview, _) = preview(directory, workspace, &endpoint_id, generation, &recipient)?;
            check_owner(owner)?;
            Ok(Outcome::Preview { preview })
        }
        Command::Create {
            idempotency_key,
            endpoint_id,
            generation,
            recipient,
            confirm,
            expected,
        } => {
            if !confirm {
                return Err(Failure::Invalid);
            }
            if owner.kind() == "agent" && idempotency_key.is_none() {
                return Err(Failure::Invalid);
            }
            if idempotency_key
                .as_deref()
                .is_some_and(|k| k.len() > 128 || !crate::store::valid_id(k))
            {
                return Err(Failure::Invalid);
            }
            shared
                .lock()
                .map_err(|_| Failure::Unavailable)?
                .ensure_not_terminal_held(&endpoint_id)
                .map_err(map_hub)?;
            let fingerprint=format!("{:x}",Sha256::digest(serde_json::to_vec(&serde_json::json!({"endpoint":endpoint_id,"generation":generation,"recipient":recipient,"expected":expected})).map_err(|_|Failure::Invalid)?));
            if let Some(key) = &idempotency_key {
                use rusqlite::OptionalExtension;
                let h = shared.lock().map_err(|_| Failure::Unavailable)?;
                let old: Option<String> = h
                    .store
                    .db
                    .query_row(
                        "SELECT fingerprint FROM hub_create_requests WHERE actor=?1 AND key=?2",
                        rusqlite::params![owner.id(), key],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|_| Failure::Unavailable)?;
                if let Some(old) = old {
                    return if old == fingerprint {
                        Ok(Outcome::Saved {
                            revision: 1,
                            paused: true,
                        })
                    } else {
                        Err(Failure::Conflict)
                    };
                }
            }
            let (current, binding) =
                preview(directory, workspace, &endpoint_id, generation, &recipient)?;
            if expected != current {
                return Err(Failure::Conflict);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.ensure_not_terminal_held(&endpoint_id)
                .map_err(map_hub)?;
            if let Some(key) = idempotency_key.as_deref() {
                // A concurrent identical request may have committed while the
                // Core metadata preview ran outside the hub mutex.
                use rusqlite::OptionalExtension;
                let committed: Option<String> = hub
                    .store
                    .db
                    .query_row(
                        "SELECT fingerprint FROM hub_create_requests WHERE actor=?1 AND key=?2",
                        rusqlite::params![owner.id(), key],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|_| Failure::Unavailable)?;
                if let Some(committed) = committed {
                    return if committed == fingerprint {
                        Ok(Outcome::Saved {
                            revision: 1,
                            paused: true,
                        })
                    } else {
                        Err(Failure::Conflict)
                    };
                }
                hub.add_binding_idempotent(binding, owner.id(), now, key, &fingerprint)
                    .map_err(map_hub)?;
            } else {
                hub.add_binding_audited(binding, owner.id(), now)
                    .map_err(map_hub)?;
            }
            saved(&hub, &endpoint_id)
        }
        Command::Delete {
            endpoint_id,
            revision,
            confirm,
        } => {
            if !confirm || revision < 1 {
                return Err(Failure::Invalid);
            }
            let mut h = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            h.delete_binding_audited(&endpoint_id, revision, owner.id(), now)
                .map_err(map_hub)?;
            Ok(Outcome::Deleted { endpoint_id })
        }
        Command::Pause {
            endpoint_id,
            revision,
            confirm,
        } => {
            if !confirm || revision < 1 {
                return Err(Failure::Invalid);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.pause_audited(&endpoint_id, revision, owner.id(), now)
                .map_err(map_hub)?;
            saved(&hub, &endpoint_id)
        }
        Command::ExportMetadata { kind, endpoint_id } => {
            let hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            check_owner(owner)?;
            let archive = hub
                .metadata_archive(kind, endpoint_id.as_deref())
                .map_err(map_hub)?;
            Ok(Outcome::MetadataArchive { archive })
        }
        Command::ClearMetadata {
            archive,
            archive_saved,
            confirm,
        } => {
            if !archive_saved || !confirm {
                return Err(Failure::Invalid);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.clear_metadata_archive(&archive, owner.id(), now)
                .map_err(map_hub)?;
            Ok(Outcome::MetadataCleared)
        }
        Command::InspectRecovery {
            endpoint_id,
            before_audit,
        } => {
            let hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            check_owner(owner)?;
            recovery_status(&hub, &endpoint_id, before_audit)
        }
        Command::RecoverParked {
            endpoint_id,
            revision,
            transfer_id,
            operation,
            confirm,
        } => {
            if !confirm || revision < 1 || !crate::store::valid_id(&transfer_id) {
                return Err(Failure::Invalid);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.recover_parked(
                &endpoint_id,
                revision,
                &transfer_id,
                operation,
                owner.id(),
                now,
            )
            .map_err(map_hub)?;
            recovery_status(&hub, &endpoint_id, None)
        }
        Command::InspectEmailDelivery { endpoint_id } => {
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            check_owner(owner)?;
            email_delivery_status(&mut hub, &endpoint_id)
        }
        Command::AbandonPreparedEmail {
            endpoint_id,
            revision,
            intent_id,
            confirm,
        } => {
            if !confirm {
                return Err(Failure::Invalid);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            check_owner(owner)?;
            hub.abandon_prepared_email_audited_now(&endpoint_id, revision, &intent_id, owner.id())
                .map_err(|error| match error {
                    crate::email::Error::Invalid => Failure::Invalid,
                    crate::email::Error::Conflict | crate::email::Error::Denied => {
                        Failure::Conflict
                    }
                    _ => Failure::Unavailable,
                })?;
            email_delivery_status(&mut hub, &endpoint_id)
        }
        Command::BindEmail {
            endpoint_id,
            revision,
            contact,
            confirm,
        } => {
            let contact = contact.normalized().map_err(|_| Failure::Invalid)?;
            if !confirm || revision < 1 {
                return Err(Failure::Invalid);
            }
            // This callback performs fresh Core directory I/O outside the state lock.
            if !activation.email_enabled || !(activation.member_current)(&contact.member_id) {
                return Err(Failure::Unavailable);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.ensure_not_terminal_held(&endpoint_id)
                .map_err(map_hub)?;
            hub.bind_email_audited(&endpoint_id, revision, contact, owner.id(), now)
                .map_err(|_| Failure::Conflict)?;
            saved(&hub, &endpoint_id)
        }
        Command::BindTelegram {
            endpoint_id,
            revision,
            target,
            confirm,
        } => {
            if !confirm || revision < 1 {
                return Err(Failure::Invalid);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.ensure_not_terminal_held(&endpoint_id)
                .map_err(map_hub)?;
            let view = hub.view(&endpoint_id).map_err(map_hub)?;
            if view.revision != revision || !view.paused {
                return Err(Failure::Conflict);
            }
            hub.bind_telegram_audited(&endpoint_id, revision, target, owner.id(), now)
                .map_err(|e| match e {
                    crate::telegram::Error::Hub(e) => map_hub(e),
                    crate::telegram::Error::Conflict => Failure::Conflict,
                    crate::telegram::Error::Invalid => Failure::Invalid,
                    _ => Failure::Unavailable,
                })?;
            saved(&hub, &endpoint_id)
        }
    }
}
pub(crate) fn core_executor(
    shared: SharedHub<Core>,
    core: Core,
    workspace: String,
    activation: Activation,
) -> impl Fn(&BindingManager, Command) -> Result<Outcome, Failure> + Send + Sync {
    move |owner, command| {
        execute_with_activation(&shared, &core, &workspace, owner, command, &activation)
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::core::AgentMetadata;

    struct Metadata {
        endpoint: AgentMetadata,
        recipient: AgentMetadata,
    }
    impl Directory for Metadata {
        fn metadata(&self, id: &str) -> Result<AgentMetadata, crate::core::Error> {
            Ok(if id == "ag_endpoint" {
                self.endpoint.clone()
            } else {
                self.recipient.clone()
            })
        }
    }
    fn fixture() -> Metadata {
        let mut endpoint = crate::recovery::tests::metadata();
        endpoint.agent_id = "ag_endpoint".into();
        let mut recipient = endpoint.clone();
        recipient.agent_id = "ag_recipient".into();
        Metadata {
            endpoint,
            recipient,
        }
    }
    #[test]
    fn recipients_with_shell_or_other_runtime_kinds_have_core_mailboxes() {
        let mut d = fixture();
        for kind in ["shell", "command", "app", "future_runtime"] {
            d.recipient.kind = kind.into();
            assert!(preview(
                &d,
                &d.endpoint.workspace_id,
                "ag_endpoint",
                1,
                "ag_recipient"
            )
            .is_ok());
        }
    }
    #[test]
    fn preview_errors_are_specific_and_keep_identity_workspace_and_relay_checks() {
        for (field, code) in [
            ("endpoint_id", "endpoint_identity_mismatch"),
            ("recipient_id", "recipient_identity_mismatch"),
            ("endpoint_workspace", "endpoint_workspace_mismatch"),
            ("recipient_workspace", "recipient_workspace_mismatch"),
            ("endpoint_kind", "endpoint_kind_unsupported"),
            ("endpoint_path", "endpoint_unplaced"),
        ] {
            let mut d = fixture();
            let workspace = d.endpoint.workspace_id.clone();
            match field {
                "endpoint_id" => d.endpoint.agent_id = "ag_wrong".into(),
                "recipient_id" => d.recipient.agent_id = "ag_wrong".into(),
                "endpoint_workspace" => d.endpoint.workspace_id = "ws_other".into(),
                "recipient_workspace" => d.recipient.workspace_id = "ws_other".into(),
                "endpoint_kind" => d.endpoint.kind = "shell".into(),
                "endpoint_path" => d.endpoint.path = None,
                _ => unreachable!(),
            }
            assert!(matches!(
                preview(&d, &workspace, "ag_endpoint", 1, "ag_recipient"),
                Err(Failure::InvalidPreview(actual)) if actual == code
            ));
        }
    }
}
