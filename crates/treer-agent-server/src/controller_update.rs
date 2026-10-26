//! Controller-owned #222 runtime. Browser requests carry only typed bindings;
//! artifact origin, pins and shared-installation consent come from one checked
//! local operator configuration, never Proxy command fields or environment.
#[cfg_attr(not(unix), allow(dead_code))] // Windows coordinator remains unwired.
pub(crate) mod catalog;
#[cfg(unix)]
pub(crate) mod unix;
#[cfg(windows)]
mod windows;
#[cfg(all(unix, feature = "native-update-acceptance"))]
use crate::self_update::hosted_controller_dispatch as hosted_dispatch;

use treer_protocol::{controller_update_runtime::Inspection, ProtocolError};

pub(crate) fn unsupported() -> ProtocolError {
    ProtocolError::new(
        "controller_update_unsupported",
        "Native Controller updates are unavailable on this installation",
    )
}

pub(crate) async fn inspect(workspace: &str, server: &str) -> Result<Inspection, ProtocolError> {
    #[cfg(unix)]
    {
        unix::inspect(workspace, server)
            .await
            .map_err(|_| unsupported())
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, server);
        Err(unsupported())
    }
}

/// Platform acceptance is a source gate, never an environment flag or browser
/// input. Keep false until isolated whole-worker/launcher acceptance is reviewed.
fn native_accepted() -> bool {
    false
}

pub(crate) async fn dispatch(
    workspace: &str,
    server: &str,
    binding: treer_protocol::controller_update_runtime::DispatchBinding,
) -> Result<treer_protocol::controller_update::Operation, ProtocolError> {
    if binding.workspace_id != workspace || binding.server_id != server || !capability_available() {
        return Err(unsupported());
    }
    #[cfg(unix)]
    {
        // Submission belongs to this Controller, not a websocket session. A
        // dropped response/connection must never cancel preparation or kill a
        // worker whose ownership transfer may already have completed. The Proxy
        // has durably accepted the UUID before sending this one command; native
        // EXCL admission persists its own claim before artifact IO or spawning.
        // Dropping this JoinHandle detaches, never retries, the owned task.
        tokio::spawn(async move {
            #[cfg(feature = "native-update-acceptance")]
            let prepared = if hosted_dispatch::enabled() {
                hosted_dispatch::prepare(binding).await?
            } else {
                crate::self_update::web_adapter::prepare(binding).await?
            };
            #[cfg(not(feature = "native-update-acceptance"))]
            let prepared = crate::self_update::web_adapter::prepare(binding).await?;
            tokio::task::spawn_blocking(move || prepared.handoff()).await?
        })
        .await
        .map_err(|_| unconfirmed())?
        .map_err(|_| unconfirmed())
    }
    #[cfg(not(unix))]
    {
        let _ = binding;
        Err(unsupported())
    }
}

pub(crate) fn capability_available() -> bool {
    #[cfg(all(unix, feature = "native-update-acceptance"))]
    if hosted_dispatch::enabled() {
        return true;
    }
    cfg!(any(target_os = "linux", target_os = "macos")) && native_accepted()
}

#[cfg(unix)]
fn unconfirmed() -> ProtocolError {
    ProtocolError::new(
        "controller_update_result_unconfirmed",
        "Native update requires reconciliation",
    )
}
