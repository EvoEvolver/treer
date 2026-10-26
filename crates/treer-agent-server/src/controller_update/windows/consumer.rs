//! One confirmed command owner on the existing authenticated Proxy client.
//! Browser response loss does not cancel its task. No new authorization surface.
use super::super::{catalog::Catalog, consumption};
use super::preparation::{Preparation, SelectedCustody};
use crate::controller::ControllerRuntime;
use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex};
use treer_protocol::{
    controller_update as wire, controller_update_runtime::DispatchBinding, ProtocolError,
};

fn refused() -> ProtocolError {
    ProtocolError::new(
        "controller_update_result_unconfirmed",
        "Native update requires reconciliation",
    )
}

#[derive(Clone)]
pub(crate) struct Consumer {
    runtime: ControllerRuntime,
    slot: Arc<Mutex<Option<Arc<Owned>>>>,
}
struct Owned {
    binding: DispatchBinding,
    report: Mutex<wire::Operation>,
    custody: tokio::sync::Mutex<Custody>,
    // Normal local source is physically held across network IO and all phases.
    _directory: treer_platform::windows_update_preparation::ConfigDirectory,
    _catalog: treer_platform::windows_update_preparation::ConfigSnapshot,
}
#[derive(Default)]
struct Custody {
    preparation: Option<Preparation>,
    _closed: Option<SelectedCustody>,
}

impl Consumer {
    pub(crate) fn new(runtime: ControllerRuntime) -> Self {
        Self {
            runtime,
            slot: Arc::new(Mutex::new(None)),
        }
    }
    /// Called from the SAME authenticated/capability-checked command handler.
    /// No environment switch or acceptance helper enables the Windows product.
    pub(crate) fn dispatch(
        &self,
        workspace: &str,
        server: &str,
        binding: DispatchBinding,
    ) -> Result<wire::Operation, ProtocolError> {
        if !super::super::capability_available() {
            return Err(super::super::unsupported());
        }
        self.accept_confirmed(workspace, server, binding)
    }

    /// Internal confirmed consumer, not an alternate login/Policy/confirmation
    /// route. Tests call this seam after their closed fixture setup, never
    /// claiming real human authorization or public capability acceptance.
    pub(crate) fn accept_confirmed(
        &self,
        workspace: &str,
        server: &str,
        binding: DispatchBinding,
    ) -> Result<wire::Operation, ProtocolError> {
        self.runtime
            .check_update_command_scope(workspace, server)
            .map_err(|_| refused())?;
        if !consumption::validate(workspace, server, &binding) {
            return Err(refused());
        }
        let mut slot = self.slot.lock().map_err(|_| refused())?;
        if let Some(owned) = slot.as_ref() {
            if owned.binding != binding {
                return Err(refused());
            }
            return owned
                .report
                .lock()
                .map(|op| op.clone())
                .map_err(|_| refused());
        }
        let (directory, snapshot, settings, shared) =
            configuration(&binding).map_err(|_| refused())?;
        let accepted = consumption::projection(&binding, false);
        let owned = Arc::new(Owned {
            binding,
            report: Mutex::new(accepted.clone()),
            custody: tokio::sync::Mutex::new(Custody::default()),
            _directory: directory,
            _catalog: snapshot,
        });
        // Consume before spawning/awaiting; same binding observes, never reruns.
        *slot = Some(owned.clone());
        let runtime = self.runtime.clone();
        tokio::spawn(async move {
            let _terminal = Terminal(&owned);
            let _ = owned.run(&runtime, &settings, &shared).await;
        });
        Ok(accepted)
    }
    pub(crate) fn observe(
        &self,
        workspace: &str,
        server: &str,
        id: &str,
    ) -> Result<wire::Operation, ProtocolError> {
        self.runtime
            .check_update_command_scope(workspace, server)
            .map_err(|_| refused())?;
        let slot = self.slot.lock().map_err(|_| refused())?;
        let owned = slot
            .as_ref()
            .filter(|o| o.binding.operation_id == id)
            .ok_or_else(refused)?;
        let report = owned
            .report
            .lock()
            .map(|op| op.clone())
            .map_err(|_| refused());
        report
    }
    #[cfg(test)]
    pub(crate) fn closed_for_test(&self) -> bool {
        self.slot
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|o| o.custody.try_lock().is_ok_and(|c| c._closed.is_some()))
    }
}

struct Terminal<'a>(&'a Owned);
impl Drop for Terminal<'_> {
    fn drop(&mut self) {
        // Also applies to task unwind/cancellation, without retry or teardown
        // authority. Session close is NOT an update completion receipt.
        if let Ok(mut report) = self.0.report.lock() {
            *report = consumption::projection(&self.0.binding, true);
        }
    }
}

impl Owned {
    async fn run(
        &self,
        runtime: &ControllerRuntime,
        settings: &crate::UpdateArgs,
        shared: &[String],
    ) -> Result<()> {
        let mut custody = self.custody.lock().await;
        let prepared =
            Preparation::download(runtime, self.binding.clone(), settings, shared).await?;
        // Store BEFORE transport awaits: errors/Unknown retain the original owner.
        custody.preparation = Some(prepared);
        let prepared = custody.preparation.as_mut().unwrap();
        prepared.start_downloaded(runtime).await?;
        prepared.inspect(runtime).await?;
        prepared.close(runtime).await?;
        let closed = prepared.retain_closed(runtime)?;
        custody._closed = Some(closed);
        Ok(())
    }
}

fn configuration(
    binding: &DispatchBinding,
) -> Result<(
    treer_platform::windows_update_preparation::ConfigDirectory,
    treer_platform::windows_update_preparation::ConfigSnapshot,
    crate::UpdateArgs,
    Vec<String>,
)> {
    let root = crate::service::controller_update_config_directory()?;
    let directory = treer_platform::windows_update_preparation::ConfigDirectory::open(&root)?;
    let snapshot = directory.read("controller-update-catalog.json")?;
    let mut catalog = Catalog::parse(snapshot.bytes())?;
    catalog.select_plan(binding)?;
    let settings = catalog
        .signed_release
        .take()
        .ok_or_else(|| anyhow!("controller_update_signed_configuration_missing"))?;
    Ok((
        directory,
        snapshot,
        crate::UpdateArgs {
            release_base_url: settings.base_url,
            channel: settings.channel,
            public_key: settings.public_key,
        },
        settings.confirmed_shared_servers,
    ))
}
