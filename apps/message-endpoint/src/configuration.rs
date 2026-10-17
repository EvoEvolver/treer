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
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Create {
        endpoint_id: String,
        generation: i64,
        recipient: String,
        confirm: bool,
    },
    Pause {
        endpoint_id: String,
        revision: i64,
        confirm: bool,
    },
    BindTelegram {
        endpoint_id: String,
        revision: i64,
        target: Target,
        confirm: bool,
    },
}
#[derive(Clone, Copy)]
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
) -> Result<(), Failure> {
    check_owner(owner)?;
    match command {
        Command::Create {
            endpoint_id,
            generation,
            recipient,
            confirm,
        } => {
            if !confirm
                || generation <= 0
                || endpoint_id == recipient
                || !endpoint_id.starts_with("ag_")
                || !recipient.starts_with("ag_")
                || !crate::store::valid_id(&endpoint_id)
                || !crate::store::valid_id(&recipient)
            {
                return Err(Failure::Invalid);
            }
            let endpoint = directory
                .metadata(&endpoint_id)
                .map_err(|_| Failure::Unavailable)?;
            let recipient_meta = directory
                .metadata(&recipient)
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
            let binding = Binding {
                endpoint_id,
                generation,
                recipient,
                expected_path: endpoint.path.ok_or(Failure::Invalid)?,
                expected_cwd: endpoint.cwd,
                expected_server_id: endpoint.server_id,
            };
            let mut hub = shared.lock().map_err(|_| Failure::Unavailable)?;
            let now = check_owner(owner)?;
            hub.add_binding_audited(binding, owner.id(), now)
                .map_err(map_hub)
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
                .map_err(map_hub)
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
                })
        }
    }
}
pub fn core_executor(
    shared: SharedHub<Core>,
    core: Core,
    workspace: String,
) -> impl Fn(&Owner, Command) -> Result<(), Failure> + Send + Sync {
    move |owner, command| execute(&shared, &core, &workspace, owner, command)
}
