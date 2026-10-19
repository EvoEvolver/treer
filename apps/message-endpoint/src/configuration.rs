//! Owner-only configuration commands. No transport token, caller identity, or
//! claimed Core metadata is accepted from JSON. New endpoints remain paused.
use crate::{
    core::Core,
    hub::{Binding, Directory},
    owner_auth::Owner,
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
        endpoint_id: String,
        generation: i64,
        recipient: String,
        confirm: bool,
        expected: Preview,
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
        return Err(Failure::Invalid);
    }
    let endpoint = directory
        .metadata(endpoint_id)
        .map_err(|_| Failure::Unavailable)?;
    let recipient_meta = directory
        .metadata(recipient)
        .map_err(|_| Failure::Unavailable)?;
    if endpoint.agent_id != endpoint_id
        || recipient_meta.agent_id != recipient
        || endpoint.workspace_id != workspace
        || recipient_meta.workspace_id != workspace
        || endpoint.kind != "command"
        || recipient_meta.kind != "command"
    {
        return Err(Failure::Invalid);
    }
    let path = endpoint.path.ok_or(Failure::Invalid)?;
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
#[derive(Clone, Copy, Debug)]
pub enum Failure {
    Invalid,
    Conflict,
    Unavailable,
    Expired,
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
        crate::hub::Error::Revision | crate::hub::Error::Binding => Failure::Conflict,
        _ => Failure::Unavailable,
    }
}
fn check_owner(owner: &Owner) -> Result<i64, Failure> {
    let now = current_time()?;
    if now >= owner.expires_at() {
        return Err(Failure::Expired);
    }
    Ok(now)
}
/// Only the HTTP boundary can supply Owner, after current Proxy verification.
/// Directory calls run before taking the hub mutex; failures never install
/// caller-supplied paths or partially create an endpoint.
pub fn execute<M: Mailbox + Directory, D: Directory>(
    shared: &SharedHub<M>,
    directory: &D,
    workspace: &str,
    owner: &Owner,
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
    owner: &Owner,
    command: Command,
    activation: &Activation,
) -> Result<Outcome, Failure> {
    check_owner(owner)?;
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
            let (preview, _) = preview(directory, workspace, &endpoint_id, generation, &recipient)?;
            check_owner(owner)?;
            Ok(Outcome::Preview { preview })
        }
        Command::Create {
            endpoint_id,
            generation,
            recipient,
            confirm,
            expected,
        } => {
            if !confirm {
                return Err(Failure::Invalid);
            }
            let (current, binding) =
                preview(directory, workspace, &endpoint_id, generation, &recipient)?;
            if expected != current {
                return Err(Failure::Conflict);
            }
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.add_binding_audited(binding, owner.id(), now)
                .map_err(map_hub)?;
            saved(&hub, &endpoint_id)
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
) -> impl Fn(&Owner, Command) -> Result<Outcome, Failure> + Send + Sync {
    move |owner, command| {
        execute_with_activation(&shared, &core, &workspace, owner, command, &activation)
    }
}
