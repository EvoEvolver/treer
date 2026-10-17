//! Durable intent only: never evidence that a child started or stopped.
//! A leftover/partial record always requires reconciliation, never replay.
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::{Invocation, Operation, Receipt};

const LIMIT: usize = 64 * 1024;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema_version: u32,
    receipt: Receipt,
    operation: Operation,
    operation_id: String,
}

/// Freshly persisted intent, retaining both its inode and the original locked
/// invocation. No constructor, file accessor, Clone or deserialization path.
/// This is NOT permission to spawn or proof that a supervisor registered.
pub struct RecordedIntent<'plan, 'owner> {
    invocation: &'plan Invocation<'owner>,
    file: File,
    name: &'static str,
    bytes: Vec<u8>,
    sha256: String,
}

impl RecordedIntent<'_, '_> {
    /// Recheck the same locked job, recipe, private directory entry and exact
    /// bytes. Reopening an old marker never constructs this capability.
    pub fn verify(&self) -> Result<()> {
        let result = (|| -> Result<()> {
            self.invocation.verify_binding()?;
            let reopened = self.invocation.journal.directory.read_file(self.name)?;
            let original = self.file.metadata()?;
            let current = reopened.metadata()?;
            if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
                bail!("update_recovery_required");
            }
            let mut persisted = Vec::new();
            reopened
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut persisted)?;
            if persisted != self.bytes {
                bail!("update_recovery_required");
            }
            self.invocation.verify_binding()
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }

    /// Digest of exact synced/read-back bytes, revalidated at access. Metadata
    /// only: downstream registration still needs its own durable authority.
    pub fn sha256(&self) -> Result<&str> {
        self.verify()?;
        Ok(&self.sha256)
    }
}

impl<'owner> Invocation<'owner> {
    /// Exclusively persist a bounded, private launch intent before any future
    /// installer dispatch. No spawn/phase transition or child-start authority.
    /// Never remove/rewrite an uncertain marker, even when persistence fails.
    pub fn record_start_intent(&self) -> Result<RecordedIntent<'_, 'owner>> {
        self.verify_binding()?;
        // Only public fully prepared recipes may record intent. The private
        // data-builder seam cannot accidentally authorize incomplete recipes.
        if self.node.is_none() || self.temporary.is_none() {
            bail!("update_launcher_binding_mismatch");
        }
        let (name, operation) = match self.operation {
            Operation::Install => ("launcher-install-intent.json", "install"),
            Operation::Status => ("launcher-status-intent.json", "status"),
        };
        let intent = Intent {
            schema_version: 1,
            receipt: self.receipt.clone(),
            operation: self.operation,
            operation_id: format!("update-{}-launcher-{operation}", self.receipt.job_id),
        };
        let bytes = serde_json::to_vec(&intent)
            .map_err(|_| anyhow::anyhow!("update_launcher_start_intent_unavailable"))?;
        if bytes.len() > LIMIT {
            bail!("update_launcher_start_intent_unavailable");
        }
        // EXCL rejects every existing entry, including empty/corrupt records,
        // symlinks and directories. Any IO failure is conservatively uncertain.
        // File sync precedes directory sync; readback never grants replay.
        let result = (|| -> Result<RecordedIntent<'_, 'owner>> {
            let mut file = self.journal.directory.create_file(name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.journal.directory.sync()?;
            self.verify_binding()?;
            let reopened = self.journal.directory.read_file(name)?;
            let original = file.metadata()?;
            let current = reopened.metadata()?;
            if (original.dev(), original.ino()) != (current.dev(), current.ino()) {
                bail!("update_recovery_required");
            }
            let mut persisted = Vec::new();
            reopened
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut persisted)?;
            if persisted.len() > LIMIT {
                bail!("update_recovery_required");
            }
            let observed: Intent = serde_json::from_slice(&persisted)?;
            if observed != intent {
                bail!("update_recovery_required");
            }
            self.verify_binding()?;
            use sha2::{Digest, Sha256};
            let proof = RecordedIntent {
                invocation: self,
                file,
                name,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                bytes,
            };
            proof.verify()?;
            Ok(proof)
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }
}
