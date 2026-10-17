//! Durable identity metadata only. This does not authenticate a connection,
//! mint a permit, create an arbitration gate or authorize installer execution.
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treer_platform::process_image::ProcessImage;
use uuid::Uuid;

use super::{RecordedIntent, LIMIT};
use crate::self_update::{images::digest_file, launcher_arbitration::Binding};

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
/// supervisor code must additionally attest the actual connected peer and own
/// exactly one gate; this value alone cannot select Accepted or noexec.
pub struct RegisteredSupervisor<'plan, 'owner> {
    intent: RecordedIntent<'plan, 'owner>,
    image: ProcessImage,
    file: File,
    name: &'static str,
    bytes: Vec<u8>,
    image_sha256: String,
    binding: Binding,
}

impl<'plan, 'owner> RecordedIntent<'plan, 'owner> {
    /// `image` must originate from the coordinator's owned child, never an
    /// incoming PID claim. This verifies its private session but does not prove
    /// child ownership, authenticate its IPC connection or start that process.
    /// Any existing registration requires recovery; a new attempt cannot replace
    /// it. Consume the fresh intent capability even on a persistence failure.
    pub fn register_supervisor(
        self,
        attempt: Uuid,
        image: ProcessImage,
    ) -> Result<RegisteredSupervisor<'plan, 'owner>> {
        let result = (|| -> Result<_> {
            if attempt.is_nil() {
                bail!("update_recovery_required");
            }
            self.verify()?;
            image.verify_private_session()?;
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
            let mut file = self.invocation.journal.directory.create_file(name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.invocation.journal.directory.sync()?;
            self.verify()?;
            image.verify_private_session()?;
            let binding = Binding::new(
                attempt,
                &self.sha256,
                &format!("{:x}", Sha256::digest(&bytes)),
            )?;
            let proof = RegisteredSupervisor {
                intent: self,
                image,
                file,
                name,
                bytes,
                image_sha256,
                binding,
            };
            proof.verify()?;
            Ok(proof)
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }
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
    /// or grant another gate authority; gate ownership remains to be wired.
    pub fn binding(&self) -> Result<&Binding> {
        self.verify()?;
        Ok(&self.binding)
    }
}
