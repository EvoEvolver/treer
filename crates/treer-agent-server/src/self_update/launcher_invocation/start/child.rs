//! Durable direct-child identity, never a resumable child or noexec proof.
use super::*;
use treer_platform::process_image::ProcessImage;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema_version: u8,
    receipt: Receipt,
    operation: Operation,
    intent_sha256: String,
    pid: u32,
    creation: String,
    parent_pid: u32,
    parent_creation: String,
    controller_sha256: String,
}

/// Holds the original intent, exact record inode and OS image observation.
/// Re-reading these bytes cannot reconstruct ownership or authorize a retry.
pub(in crate::self_update) struct RecordedChild<'intent, 'plan, 'owner> {
    intent: &'intent RecordedIntent<'plan, 'owner>,
    file: File,
    name: &'static str,
    bytes: Vec<u8>,
    _image: ProcessImage,
}

impl<'plan, 'owner> RecordedIntent<'plan, 'owner> {
    pub(in crate::self_update) fn record_child<'intent>(
        &'intent self,
        image: ProcessImage,
    ) -> Result<RecordedChild<'intent, 'plan, 'owner>> {
        self.verify()?;
        let parent = ProcessImage::capture(std::process::id())?;
        image.verify_parent(&parent)?;
        image.verify_private_session()?;
        let record = Identity {
            schema_version: 1,
            receipt: self.invocation.receipt.clone(),
            operation: self.invocation.operation,
            intent_sha256: self.sha256()?.to_owned(),
            pid: image.pid(),
            creation: image.creation_identity().to_owned(),
            parent_pid: parent.pid(),
            parent_creation: parent.creation_identity().to_owned(),
            controller_sha256: self.invocation.receipt.controller_sha256.clone(),
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() > LIMIT {
            bail!("update_recovery_required");
        }
        let name = match record.operation {
            Operation::Install => "launcher-install-child.json",
            Operation::Status => "launcher-status-child.json",
        };
        let mut file = self.invocation.journal.directory.create_file(name)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.invocation.journal.directory.sync()?;
        image.verify_parent(&parent)?;
        image.verify_private_session()?;
        let recorded = RecordedChild {
            intent: self,
            file,
            name,
            bytes,
            _image: image,
        };
        recorded.verify()?;
        Ok(recorded)
    }
}

impl RecordedChild<'_, '_, '_> {
    /// File integrity only after completion; never a saved-PID liveness test.
    pub(in crate::self_update) fn verify(&self) -> Result<()> {
        self.intent.verify()?;
        let file = self
            .intent
            .invocation
            .journal
            .directory
            .read_file(self.name)?;
        let original = self.file.metadata()?;
        let current = file.metadata()?;
        if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
            bail!("update_recovery_required");
        }
        let mut bytes = Vec::new();
        file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        if bytes != self.bytes {
            bail!("update_recovery_required");
        }
        self.intent.verify()
    }
}
