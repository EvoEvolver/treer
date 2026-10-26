//! Signed input admission and internal linear custody. No exposed execution or
//! capability activation. Freshness/high-water protection is a separate layer.
use std::io::Read;

use anyhow::{bail, Result};
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use crate::{release_update, windows_update_intent::Binding};

pub struct Documents<'a> {
    pub channel: &'a [u8],
    pub channel_signature: &'a [u8],
    pub manifest: &'a [u8],
    pub manifest_signature: &'a [u8],
    pub channel_name: &'a str,
}

struct PairReaders<'a> {
    controller: &'a mut dyn Read,
    cli: &'a mut dyn Read,
}

fn digest(reader: &mut dyn Read, expected: Option<&release_update::Artifact>) -> Result<String> {
    const LIMIT: u64 = 128 * 1024 * 1024;
    let limit = expected.map_or(LIMIT, |artifact| artifact.size as u64);
    let mut reader = reader.take(limit + 1);
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        if count > limit {
            bail!("windows_update_input_size_refused");
        }
        hash.update(&buffer[..read]);
    }
    let digest = format!("{:x}", hash.finalize());
    if count == 0 || expected.is_some_and(|artifact| count != limit || digest != artifact.sha256) {
        bail!("windows_update_input_digest_refused");
    }
    Ok(digest)
}

fn verify_inputs(
    documents: &Documents<'_>,
    key: &VerifyingKey,
    binding: &mut Binding,
    old: PairReaders<'_>,
    target: PairReaders<'_>,
) -> Result<()> {
    let release = release_update::authenticate_documents(
        documents.channel,
        documents.channel_signature,
        documents.manifest,
        documents.manifest_signature,
        documents.channel_name,
        key,
        "windows-x86_64",
    )?;
    binding.controller_sha256 = digest(target.controller, Some(&release.controller))?;
    binding.cli_sha256 = digest(target.cli, Some(&release.cli))?;
    binding.old_controller_sha256 = digest(old.controller, None)?;
    binding.old_cli_sha256 = digest(old.cli, None)?;
    binding.signed_manifest_sha256 = release.manifest_sha256;
    binding.validate()
}

/// Keeps the physically checked directories and all four no-write/no-delete-
/// sharing image handles alive. Fields cannot be constructed from digest claims.
/// The installed pair retains its exclusive installation lease. Worker handoff
/// and image publication are NOT supplied here.
#[cfg(windows)]
pub struct AdmittedInputs {
    binding: Binding,
    _installed: treer_platform::windows_update_inputs::LockedInstallation,
    _target: treer_platform::windows_update_inputs::PinnedPair,
    state: Option<crate::controller::ControllerUpdateState>,
}

#[cfg(windows)]
impl AdmittedInputs {
    /// Only a selected live Controller can mint the opaque state authority.
    /// Validation and physical pinning precede any installation claim/intent.
    #[allow(dead_code)] // No production Windows coordinator/activation entry yet.
    pub(crate) fn prepare_for_controller(
        installed: treer_platform::windows_update_inputs::LockedInstallation,
        target: treer_platform::windows_update_inputs::PinnedPair,
        documents: &Documents<'_>,
        key: &VerifyingKey,
        state: crate::controller::ControllerUpdateState,
    ) -> Result<Self> {
        let (operation, workspace, server) = state.scope();
        let mut admitted = Self::prepare_inner(
            installed, target, documents, key, operation, workspace, server,
        )?;
        state.matches(&admitted.binding)?;
        let identity = state.directory().identity()?;
        if identity.0 != admitted.binding.installation_volume
            || identity == admitted._installed.identity()?
            || identity == admitted._target.identity()?
        {
            bail!("windows_update_startup_directory_refused");
        }
        admitted.state = Some(state);
        Ok(admitted)
    }

    /// Caller identity/Policy and separately configured trust must be resolved
    /// before this internal API. No key is accepted from the signed documents.
    #[cfg(test)]
    pub fn prepare(
        installed: treer_platform::windows_update_inputs::LockedInstallation,
        target: treer_platform::windows_update_inputs::PinnedPair,
        documents: &Documents<'_>,
        key: &VerifyingKey,
        operation_id: uuid::Uuid,
        workspace_id: String,
        server_id: String,
    ) -> Result<Self> {
        Self::prepare_inner(
            installed,
            target,
            documents,
            key,
            operation_id,
            workspace_id,
            server_id,
        )
    }

    fn prepare_inner(
        installed: treer_platform::windows_update_inputs::LockedInstallation,
        target: treer_platform::windows_update_inputs::PinnedPair,
        documents: &Documents<'_>,
        key: &VerifyingKey,
        operation_id: uuid::Uuid,
        workspace_id: String,
        server_id: String,
    ) -> Result<Self> {
        let (volume, file_id) = installed.identity()?;
        if target.identity()? == (volume, file_id) {
            bail!("windows_update_stage_is_installation");
        }
        let mut binding = Binding {
            operation_id,
            workspace_id,
            server_id,
            installation_volume: volume,
            installation_file_id: file_id,
            old_controller_sha256: String::new(),
            old_cli_sha256: String::new(),
            controller_sha256: String::new(),
            cli_sha256: String::new(),
            signed_manifest_sha256: String::new(),
        };
        let (mut old_controller, mut old_cli) = installed.files();
        let (mut controller, mut cli) = target.files();
        // A caller may have inspected these handles already. Hash the whole
        // pinned files, never a suffix beginning at a caller-controlled offset.
        use std::io::{Seek, SeekFrom};
        for file in [&mut old_controller, &mut old_cli, &mut controller, &mut cli] {
            file.seek(SeekFrom::Start(0))?;
        }
        verify_inputs(
            documents,
            key,
            &mut binding,
            PairReaders {
                controller: &mut old_controller,
                cli: &mut old_cli,
            },
            PairReaders {
                controller: &mut controller,
                cli: &mut cli,
            },
        )?;
        Ok(Self {
            binding,
            _installed: installed,
            _target: target,
            state: None,
        })
    }

    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    /// A matching claim is only evidence for reconciliation. It cannot be
    /// adopted, resumed, cleared or used to bypass the single-use reservation.
    pub fn inspect_installation_claim(&self) -> Result<()> {
        crate::windows_update_intent::inspect(&self._installed.read_claim()?, &self.binding)
    }

    /// Reservation does not release pinned images; the caller must retain this
    /// object. First consume the installation-wide claim, then the job intent.
    /// Any error may leave either/both records and never grants permission to
    /// retry under another job root. There is no claim retirement API yet.
    /// It grants no permission to replace, execute or resume the job.
    pub fn reserve(
        self,
        store: treer_platform::windows_update_store::UpdateStore,
    ) -> Result<ReservedInputs> {
        if let Some(state) = &self.state {
            state.matches(&self.binding)?;
        }
        self._installed
            .claim(&crate::windows_update_intent::encode(&self.binding)?)?;
        crate::windows_update_intent::reserve(store, &self.binding)?;
        Ok(ReservedInputs(self))
    }
}

/// A consumed reservation with pinned inputs and no second-reservation method.
/// Its internal child-custody transfer is not authenticated worker admission or
/// a successful update receipt.
#[cfg(windows)]
pub struct ReservedInputs(AdmittedInputs);

#[cfg(windows)]
impl ReservedInputs {
    pub fn binding(&self) -> &Binding {
        &self.0.binding
    }

    pub(crate) fn controller_state(&self) -> Result<&crate::controller::ControllerUpdateState> {
        self.0
            .state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("windows_update_child_runtime_missing"))
    }

    /// Internal custody primitive, not an admitted worker execution entry. The
    /// future coordinator must authenticate the executable and command before
    /// invoking this; normal caller Policy and capability checks remain above it.
    #[allow(dead_code)] // No production Windows worker/coordinator is wired.
    pub(crate) fn spawn_owned_child(
        self,
        selected: &crate::controller::ControllerRuntime,
        command: &mut std::process::Command,
    ) -> Result<crate::controller::ControllerUpdateChild> {
        crate::controller::ControllerUpdateChild::spawn_reserved(self, selected, command)
    }
}

#[cfg(test)]
#[path = "windows_update_admission_tests.rs"]
pub(crate) mod tests;

#[path = "windows_update_backup.rs"]
mod backup;
#[cfg(windows)]
pub use backup::{BackedUpInputs, FilePairPhase, PublishedInputs, StartupBackedUpInputs};
