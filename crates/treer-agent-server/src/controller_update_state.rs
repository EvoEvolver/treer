//! In-process state and created-Child custody only. No wire constructor,
//! environment/default resolver, admitted worker, restart or resumable operation.
use super::ControllerRuntime;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
#[cfg(test)]
use std::process::Command;
use std::process::{Child, ExitStatus};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControllerStateEvidence {
    controller_instance_id: Uuid,
    host_epoch_sha256: String,
}

/// Not Clone or Deserialize. Keeping the runtime and physical directory alive
/// does not freeze startup-store writes or confer Host/process ownership.
pub(crate) struct ControllerUpdateState {
    runtime: ControllerRuntime,
    operation_id: Uuid,
    workspace_id: String,
    server_id: String,
    evidence: ControllerStateEvidence,
    #[cfg(windows)]
    directory: treer_platform::windows_update_backup::BackupDirectory,
}

fn validate_path(path: &Path, server_id: &str) -> Result<()> {
    let filename = format!("{server_id}-agent-startup.json");
    if !path.is_absolute()
        || path.file_name() != Some(std::ffi::OsStr::new(&filename))
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        || path.parent().is_none()
    {
        bail!("windows_update_runtime_state_path_refused");
    }
    Ok(())
}

impl ControllerRuntime {
    /// Called only after normal caller authentication/Policy and operation
    /// selection. No exposed update command/capability is introduced here.
    #[allow(dead_code)] // Production Windows coordinator is not wired yet.
    pub(crate) fn windows_update_state_for_operation(
        &self,
        operation_id: Uuid,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<ControllerUpdateState> {
        if operation_id.get_version_num() != 4
            || operation_id.get_variant() != uuid::Variant::RFC4122
            || workspace_id != self.inner.workspace_id
            || server_id != self.inner.server_id
            || !crate::windows_update_intent::identifier(workspace_id, "ws_")
            || !crate::windows_update_intent::identifier(server_id, "srv_")
            || self.inner.host_epoch.is_empty()
        {
            bail!("windows_update_runtime_scope_refused");
        }
        // This is the SAME store loaded by from_sync, not caller-supplied config
        // or a second resolution under the updater's ambient environment.
        let path = self.inner.startup_store.configured_path();
        validate_path(path, server_id)?;
        Ok(ControllerUpdateState {
            runtime: self.clone(),
            operation_id,
            workspace_id: workspace_id.into(),
            server_id: server_id.into(),
            evidence: ControllerStateEvidence {
                controller_instance_id: self.inner.update_instance_id,
                host_epoch_sha256: format!(
                    "{:x}",
                    Sha256::digest(self.inner.host_epoch.as_bytes())
                ),
            },
            #[cfg(windows)]
            directory: treer_platform::windows_update_backup::BackupDirectory::open(
                path.parent().unwrap(),
            )?,
        })
    }
}

impl ControllerUpdateState {
    pub(crate) fn scope(&self) -> (Uuid, String, String) {
        (
            self.operation_id,
            self.workspace_id.clone(),
            self.server_id.clone(),
        )
    }

    pub(crate) fn matches(&self, binding: &crate::windows_update_intent::Binding) -> Result<()> {
        binding.validate()?;
        if self.scope()
            != (
                binding.operation_id,
                binding.workspace_id.clone(),
                binding.server_id.clone(),
            )
            || self.runtime.inner.update_instance_id != self.evidence.controller_instance_id
            || self.runtime.inner.workspace_id != self.workspace_id
            || self.runtime.inner.server_id != self.server_id
        {
            bail!("windows_update_runtime_binding_refused");
        }
        Ok(())
    }

    pub(crate) fn evidence(&self) -> &ControllerStateEvidence {
        &self.evidence
    }

    fn matches_selected(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        self.matches(binding)?;
        if selected.inner.update_instance_id != self.evidence.controller_instance_id
            || format!("{:x}", Sha256::digest(selected.inner.host_epoch.as_bytes()))
                != self.evidence.host_epoch_sha256
            || selected.inner.workspace_id != self.workspace_id
            || selected.inner.server_id != self.server_id
        {
            bail!("windows_update_child_runtime_refused");
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn directory(&self) -> &treer_platform::windows_update_backup::BackupDirectory {
        &self.directory
    }
}

/// Linear custody of the Child returned by this creator's spawn, never a PID,
/// ProcessObservation or deserialized receipt. No image authentication, Job
/// breakaway, restart authority or readiness claim follows from this object.
/// The Windows reservation retains pins/lease/state while this owner exists;
/// dropping after unknown teardown releases custody without proving child exit.
pub(crate) struct ControllerUpdateChild {
    child: Child,
    runtime: ControllerRuntime,
    evidence: ControllerStateEvidence,
    binding: crate::windows_update_intent::Binding,
    stop_attempted: bool,
    #[cfg(windows)]
    _reservation: Option<crate::windows_update_admission::ReservedInputs>,
}

#[allow(dead_code)] // Internal primitive; production worker admission is not wired.
impl ControllerUpdateChild {
    #[cfg(all(windows, test))]
    pub(crate) fn spawn_reserved(
        reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
        command: &mut Command,
    ) -> Result<Self> {
        let state = reservation.controller_state()?;
        state.matches_selected(selected, reservation.binding())?;
        let runtime = state.runtime.clone();
        let evidence = state.evidence.clone();
        let binding = reservation.binding().clone();
        // There is no fallible operation after spawn before Child is owned.
        let child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("windows_update_child_spawn_failed"))?;
        Ok(Self {
            child,
            runtime,
            evidence,
            binding,
            stop_attempted: false,
            _reservation: Some(reservation),
        })
    }

    #[cfg(windows)]
    pub(crate) fn spawn_verified_reserved(
        mut reservation: crate::windows_update_admission::ReservedInputs,
        selected: &ControllerRuntime,
    ) -> Result<Self> {
        let state = reservation.controller_state()?;
        state.matches_selected(selected, reservation.binding())?;
        let runtime = state.runtime.clone();
        let evidence = state.evidence.clone();
        let binding = reservation.binding().clone();
        let child = reservation.spawn_worker_process()?;
        Ok(Self {
            child,
            runtime,
            evidence,
            binding,
            stop_attempted: false,
            _reservation: Some(reservation),
        })
    }

    #[cfg(test)]
    pub(super) fn spawn_fixture(
        state: &ControllerUpdateState,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
        command: &mut Command,
    ) -> Result<Self> {
        state.matches_selected(selected, binding)?;
        let child = command.spawn()?;
        Ok(Self {
            child,
            runtime: state.runtime.clone(),
            evidence: state.evidence.clone(),
            binding: binding.clone(),
            stop_attempted: false,
            #[cfg(windows)]
            _reservation: None,
        })
    }

    fn matches(
        &self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<()> {
        binding.validate()?;
        if binding != &self.binding
            || selected.inner.update_instance_id != self.evidence.controller_instance_id
            || self.runtime.inner.update_instance_id != self.evidence.controller_instance_id
            || selected.inner.workspace_id != binding.workspace_id
            || selected.inner.server_id != binding.server_id
            || format!("{:x}", Sha256::digest(selected.inner.host_epoch.as_bytes()))
                != self.evidence.host_epoch_sha256
        {
            bail!("windows_update_child_scope_refused");
        }
        Ok(())
    }

    pub(crate) fn poll(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<Option<ExitStatus>> {
        self.matches(selected, binding)?;
        if self.stop_attempted {
            bail!("windows_update_child_stop_already_attempted");
        }
        match self.child.try_wait() {
            Ok(status) => Ok(status),
            Err(_) => {
                self.stop_attempted = true;
                bail!("windows_update_child_observation_unknown");
            }
        }
    }

    /// One teardown attempt, on the original Child handle only. Set the latch
    /// BEFORE querying/signalling: an error must not cause Drop or a caller to
    /// repeat an uncertain lifecycle mutation. No PID reopen or Host operation.
    pub(crate) fn stop(
        &mut self,
        selected: &ControllerRuntime,
        binding: &crate::windows_update_intent::Binding,
    ) -> Result<ExitStatus> {
        self.matches(selected, binding)?;
        if self.stop_attempted {
            bail!("windows_update_child_stop_already_attempted");
        }
        self.stop_attempted = true;
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => {
                self.child
                    .kill()
                    .map_err(|_| anyhow::anyhow!("windows_update_child_stop_unknown"))?;
                self.child
                    .wait()
                    .map_err(|_| anyhow::anyhow!("windows_update_child_wait_unknown"))
            }
            Err(_) => bail!("windows_update_child_observation_unknown"),
        }
    }

    #[cfg(test)]
    pub(super) fn finish_fixture_input(&mut self) {
        use std::io::Write;
        self.child.stdin.take().unwrap().write_all(b"x").unwrap();
    }
}

impl Drop for ControllerUpdateChild {
    fn drop(&mut self) {
        if !self.stop_attempted {
            self.stop_attempted = true;
            // Only an owned child which has had no prior teardown attempt.
            // Unknown query/kill/wait outcomes never get an automatic retry.
            if matches!(self.child.try_wait(), Ok(None)) && self.child.kill().is_ok() {
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_update_state_path_requires_exact_configured_server_file() {
        let root = std::env::temp_dir();
        validate_path(&root.join("srv_fixture-agent-startup.json"), "srv_fixture").unwrap();
        for path in [
            root.join("other.json"),
            root.join("nested/../srv_fixture-agent-startup.json"),
            "srv_fixture-agent-startup.json".into(),
        ] {
            assert!(validate_path(&path, "srv_fixture").is_err());
        }
    }
}
