//! Durable identity and in-memory arbitration, not installer execution authority.
//! Registration alone does not authenticate a connection. Each fresh
//! registration; no gate can be reconstructed from copied Binding metadata.
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    process::Child,
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treer_platform::process_image::ProcessImage;
use uuid::Uuid;

use super::{RecordedIntent, LIMIT};
use crate::self_update::{
    handoff::{AuthenticatedPeer, LauncherFrame},
    images::digest_file,
    launcher_arbitration::{Arbitration, Binding, Decision},
};

/// Only this leaf's exclusive registration writer can mint this non-Clone
/// constructor capability. It is never returned to a caller or reconstructed.
pub(in crate::self_update) struct RegistrationGate {
    binding: Binding,
}

impl RegistrationGate {
    pub(in crate::self_update) fn into_binding(self) -> Binding {
        self.binding
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Registration {
    schema_version: u32,
    job_id: Uuid,
    operation: super::Operation,
    attempt: Uuid,
    intent_sha256: String,
    intent_device: u64,
    intent_inode: u64,
    pid: u32,
    creation_identity: String,
    session: u32,
    process_group: u32,
    image_device: u64,
    image_inode: u64,
    image_sha256: String,
}

/// Fresh exclusive registration, retaining the original intent and OS image.
/// No reconstruction, cloning, gate accessor or execution authority. Future
/// supervisor code must additionally attest the actual connected peer and
/// keep cancellation ownership sealed. Accepted below is an in-memory decision,
/// not owner ACK, durable execution authority or proof of installer dispatch.
pub struct RegisteredSupervisor<'plan, 'owner> {
    intent: RecordedIntent<'plan, 'owner>,
    image: ProcessImage,
    file: File,
    name: &'static str,
    bytes: Vec<u8>,
    image_sha256: String,
    binding: Binding,
    gate: Arbitration,
}

impl<'plan, 'owner> RecordedIntent<'plan, 'owner> {
    /// Derive identity from an unreaped, live Child actually held by the caller,
    /// never an incoming PID/image claim. This does not authenticate the child's
    /// IPC connection, approve its executable or start that process.
    /// Any existing registration requires recovery; a new attempt cannot replace
    /// it. Consume the fresh intent capability even on a persistence failure.
    pub fn register_supervisor(
        self,
        attempt: Uuid,
        child: &mut Child,
    ) -> Result<RegisteredSupervisor<'plan, 'owner>> {
        let result = (|| -> Result<_> {
            if attempt.is_nil() {
                bail!("update_recovery_required");
            }
            self.verify()?;
            require_live_child(child)?;
            let image = ProcessImage::capture(child.id())?;
            image.verify_private_session()?;
            require_live_child(child)?;
            let intent_metadata = self.file.metadata()?;
            let image_metadata = image.executable().metadata()?;
            let image_sha256 = digest_file(image.executable())?;
            let record = Registration {
                schema_version: 1,
                job_id: self.invocation.receipt.job_id,
                operation: self.invocation.operation,
                attempt,
                intent_sha256: self.sha256.clone(),
                intent_device: intent_metadata.dev(),
                intent_inode: intent_metadata.ino(),
                pid: image.pid(),
                creation_identity: image.creation_identity().to_owned(),
                // verify_private_session requires both IDs equal this PID.
                session: image.pid(),
                process_group: image.pid(),
                image_device: image_metadata.dev(),
                image_inode: image_metadata.ino(),
                image_sha256: image_sha256.clone(),
            };
            let bytes = serde_json::to_vec(&record)?;
            if bytes.len() > LIMIT {
                bail!("update_recovery_required");
            }
            let name = match self.invocation.operation {
                super::Operation::Install => "launcher-install-supervisor.json",
                super::Operation::Status => "launcher-status-supervisor.json",
            };
            // The intent participates in the registration transaction, rather
            // than only an earlier preflight. All uncertainty preserves evidence.
            self.verify()?;
            image.verify_private_session()?;
            require_live_child(child)?;
            let mut file = self.invocation.journal.directory.create_file(name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.invocation.journal.directory.sync()?;
            self.verify()?;
            image.verify_private_session()?;
            require_live_child(child)?;
            let binding = Binding::new(
                attempt,
                &self.sha256,
                &format!("{:x}", Sha256::digest(&bytes)),
            )?;
            let gate = Arbitration::registered(RegistrationGate {
                binding: binding.clone(),
            });
            let proof = RegisteredSupervisor {
                intent: self,
                image,
                file,
                name,
                bytes,
                image_sha256,
                binding,
                gate,
            };
            proof.verify()?;
            require_live_child(child)?;
            Ok(proof)
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }
}

// try_wait errors and already-exited/reaped children are uncertainty, not a
// source of a PID that may now name an unrelated process. Never signal or wait
// for a caller-supplied PID; the caller retains this actual Child's ownership.
fn require_live_child(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        bail!("update_recovery_required");
    }
    Ok(())
}

impl RegisteredSupervisor<'_, '_> {
    pub fn verify(&self) -> Result<()> {
        let result = (|| -> Result<()> {
            self.intent.verify()?;
            self.image.verify_private_session()?;
            if digest_file(self.image.executable())? != self.image_sha256 {
                bail!("update_recovery_required");
            }
            let reopened = self
                .intent
                .invocation
                .journal
                .directory
                .read_file(self.name)?;
            let current = reopened.metadata()?;
            let original = self.file.metadata()?;
            if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
                bail!("update_recovery_required");
            }
            let mut bytes = Vec::new();
            reopened.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
            if bytes != self.bytes {
                bail!("update_recovery_required");
            }
            let _: Registration = serde_json::from_slice(&bytes)?;
            self.intent.verify()?;
            self.image.verify_private_session()?;
            Ok(())
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }

    /// Verified metadata only. Copying it cannot copy the exclusive registration
    /// or construct another gate. No gate reference or reset API is exposed.
    pub fn binding(&self) -> Result<&Binding> {
        self.verify()?;
        Ok(&self.binding)
    }

    pub fn decision(&self) -> Decision {
        self.gate.decision()
    }

    fn verify_for_decision(&mut self) -> Result<()> {
        if let Err(error) = self.verify() {
            self.gate.invalidate();
            return Err(error);
        }
        Ok(())
    }

    /// The peer is the independently authenticated permit OWNER, not the
    /// registered supervisor. The future lifecycle must derive its expected
    /// image from the owned parent and keep cancellation endpoints private.
    /// No caller-provided Binding/gate can replace the one owned here.
    pub fn read_launcher_frame<'peer>(
        &mut self,
        peer: &'peer mut AuthenticatedPeer,
        cancellation: &std::os::unix::net::UnixStream,
    ) -> Result<LauncherFrame<'peer>> {
        self.verify_for_decision()?;
        peer.read_launcher_frame(&mut self.gate, cancellation)
    }

    /// Recheck the original durable registration at delayed acceptance, then
    /// consume the authenticated owner frame. Any pending uncertainty remains
    /// terminal even if evidence is subsequently restored.
    pub fn accept_launcher_frame(&mut self, frame: LauncherFrame<'_>) -> Result<()> {
        self.verify_for_decision()?;
        self.gate.accept(frame)
    }
}
