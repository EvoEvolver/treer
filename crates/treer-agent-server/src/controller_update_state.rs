//! In-process authority only. No wire constructor, environment/default resolver,
//! worker execution, restart permission or resumable operation.
use super::ControllerRuntime;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
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

    #[cfg(windows)]
    pub(crate) fn directory(&self) -> &treer_platform::windows_update_backup::BackupDirectory {
        &self.directory
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
