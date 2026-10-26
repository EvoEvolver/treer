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

fn require_worker_image(reader: &mut (impl Read + std::io::Seek), expected: &str) -> Result<()> {
    reader.seek(std::io::SeekFrom::Start(0))?;
    if digest(reader, None)? != expected {
        bail!("windows_update_worker_image_refused");
    }
    Ok(())
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
    worker_attempted: bool,
    receiver_attempted: bool,
    // Supplied by the trusted admission caller, never by signed metadata.
    verifying_key: [u8; 32],
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
            worker_attempted: false,
            receiver_attempted: false,
            verifying_key: key.to_bytes(),
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
    /// Selected-runtime mint only, not a wire constructor or release permit.
    /// Scope refusal does not consume an attempt; after scope validation every
    /// receiver outcome is single-use, including a failed independent recheck.
    #[allow(dead_code)] // Internal receiver boundary; no coordinator/IPC wiring.
    pub(crate) fn receiver_descriptor<'a>(
        &'a mut self,
        selected: &'a crate::controller::ControllerRuntime,
    ) -> Result<ReceiverDescriptor<'a>> {
        self.controller_state()?
            .matches_selected(selected, self.binding())?;
        if self.0.receiver_attempted || self.0.worker_attempted {
            bail!("windows_update_receiver_consumed");
        }
        self.0.receiver_attempted = true;
        Ok(ReceiverDescriptor {
            sender: self,
            selected,
        })
    }

    pub(crate) fn installation_lock_identity(&self) -> Result<(u64, u64)> {
        self.0
            ._installed
            .lock_file_identity()
            .map_err(|_| anyhow::anyhow!("windows_update_lock_unconfirmed"))
    }

    #[cfg(test)]
    pub(crate) fn fixture_lock_stdio(&self) -> std::process::Stdio {
        self.0._installed.worker_lock_stdio().unwrap()
    }

    pub fn binding(&self) -> &Binding {
        &self.0.binding
    }

    pub(crate) fn controller_state(&self) -> Result<&crate::controller::ControllerUpdateState> {
        self.0
            .state
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("windows_update_child_runtime_missing"))
    }

    /// Test-only creator proving linear custody independently of image launch.
    #[cfg(test)]
    #[allow(dead_code)] // No production Windows worker/coordinator is wired.
    pub(crate) fn spawn_owned_child(
        self,
        selected: &crate::controller::ControllerRuntime,
        command: &mut std::process::Command,
    ) -> Result<crate::controller::ControllerUpdateChild> {
        crate::controller::ControllerUpdateChild::spawn_reserved(self, selected, command)
    }

    /// Fixed internal worker creation, not a production coordinator/capability.
    /// Runtime checks happen in the creator before this mechanical image step.
    #[allow(dead_code)]
    pub(crate) fn spawn_owned_worker(
        self,
        selected: &crate::controller::ControllerRuntime,
    ) -> Result<crate::controller::ControllerUpdateChild> {
        crate::controller::ControllerUpdateChild::spawn_verified_reserved(self, selected)
    }

    #[cfg(test)]
    pub(crate) fn spawn_worker_process(&mut self) -> Result<std::process::Child> {
        self.spawn_worker_process_for_pipe(uuid::Uuid::new_v4())
    }

    pub(crate) fn spawn_worker_process_for_pipe(
        &mut self,
        selector: uuid::Uuid,
    ) -> Result<std::process::Child> {
        if self.0.worker_attempted || self.0.receiver_attempted {
            bail!("windows_update_worker_already_attempted");
        }
        // Even an error after this point consumes this attempt. No re-spawn
        // after an uncertain CreateProcess result, and no parent publication.
        self.0.worker_attempted = true;
        if selector.get_version_num() != 4 || selector.get_variant() != uuid::Variant::RFC4122 {
            bail!("windows_update_worker_selector_refused");
        }
        self.controller_state()?.matches(self.binding())?;
        self.binding().validate()?;
        let (mut controller, _) = self.0._target.files();
        require_worker_image(&mut controller, &self.binding().controller_sha256)?;
        self.0
            ._target
            .spawn_pinned_controller(
                &self.0._installed,
                &[
                    "--controller-update-worker".into(),
                    self.binding().operation_id.to_string().into(),
                    selector.simple().to_string().into(),
                ],
            )
            .map_err(|_| anyhow::anyhow!("windows_update_worker_spawn_unknown"))
    }
}

/// Opaque borrowed descriptor. No Clone, Deserialize, public fields or arbitrary
/// expected-binding/path constructor. Sender originals, lock and runtime-owned
/// state remain alive through the receiver result. This is an in-process source
/// seam, not child-side IPC authentication or normal caller Policy admission.
#[cfg(windows)]
pub(crate) struct ReceiverDescriptor<'a> {
    sender: &'a ReservedInputs,
    selected: &'a crate::controller::ControllerRuntime,
}

#[cfg(windows)]
impl<'a> ReceiverDescriptor<'a> {
    #[allow(dead_code)] // No production worker/IPC entry yet.
    pub(crate) fn receive(
        self,
        documents: &Documents<'_>,
        independently_trusted_key: &VerifyingKey,
    ) -> Result<ReceivedInputs<'a>> {
        let refused = || anyhow::anyhow!("windows_update_receiver_refused");
        let admit = || -> Result<_> {
            self.sender
                .controller_state()?
                .matches_selected(self.selected, self.sender.binding())?;
            if independently_trusted_key.to_bytes() != self.sender.0.verifying_key {
                return Err(refused());
            }
            // Each new Root/file is independently opened while originals remain
            // borrowed/held. Public identity tuples are never taken from caller.
            let installed = self.sender.0._installed.repin_images()?;
            let target = self.sender.0._target.repin()?;
            if installed.identity()?
                != (
                    self.sender.binding().installation_volume,
                    self.sender.binding().installation_file_id,
                )
                || installed.identity()? == target.identity()?
                || installed.identity()?.0 != target.identity()?.0
            {
                return Err(refused());
            }
            let (mut old_controller, mut old_cli) = installed.files();
            let (mut controller, mut cli) = target.files();
            verify_received_inputs(
                documents,
                independently_trusted_key,
                self.sender.binding(),
                [&mut old_controller, &mut old_cli, &mut controller, &mut cli],
            )?;
            self.sender
                .controller_state()?
                .matches_selected(self.selected, self.sender.binding())?;
            Ok((installed, target))
        };
        let (installed, target) = admit().map_err(|_| refused())?;
        Ok(ReceivedInputs {
            descriptor: self,
            _installed: installed,
            _target: target,
        })
    }
}

/// Verified receiver pins plus the STILL borrowed sender. Success never grants
/// permission to release originals, publish files, create a worker, retire a
/// claim or restart anything. Runtime state/fences are retained at the sender;
/// they have not been independently re-admitted or transferred to this receiver.
#[cfg(windows)]
pub(crate) struct ReceivedInputs<'a> {
    descriptor: ReceiverDescriptor<'a>,
    _installed: treer_platform::windows_update_inputs::PinnedPair,
    _target: treer_platform::windows_update_inputs::PinnedPair,
}

#[cfg(windows)]
impl ReceivedInputs<'_> {
    #[allow(dead_code)]
    pub(crate) fn binding(&self) -> &Binding {
        self.descriptor.sender.binding()
    }
}

trait WholeInput: Read + std::io::Seek {}
impl<T: Read + std::io::Seek> WholeInput for T {}

/// All four SAME pinned readers start at zero; signed target metadata and every
/// expected binding digest are rechecked, never replaced by observed hashes.
#[cfg_attr(not(windows), allow(dead_code))]
fn verify_received_inputs(
    documents: &Documents<'_>,
    key: &VerifyingKey,
    expected: &Binding,
    readers: [&mut dyn WholeInput; 4],
) -> Result<()> {
    expected.validate()?;
    let [old_controller, old_cli, controller, cli] = readers;
    for reader in [
        &mut *old_controller,
        &mut *old_cli,
        &mut *controller,
        &mut *cli,
    ] {
        reader.seek(std::io::SeekFrom::Start(0))?;
    }
    let mut observed = expected.clone();
    verify_inputs(
        documents,
        key,
        &mut observed,
        PairReaders {
            controller: old_controller,
            cli: old_cli,
        },
        PairReaders { controller, cli },
    )?;
    if observed != *expected {
        bail!("windows_update_receiver_binding_refused");
    }
    Ok(())
}

#[cfg(test)]
#[path = "windows_update_admission_tests.rs"]
pub(crate) mod tests;

#[path = "windows_update_backup.rs"]
mod backup;
#[cfg(windows)]
pub use backup::{BackedUpInputs, FilePairPhase, PublishedInputs, StartupBackedUpInputs};

#[cfg(any(windows, test))]
#[path = "windows_update_receiver_state.rs"]
mod receiver_state;
