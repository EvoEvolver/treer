//! Durable intent only: never evidence that a child started or stopped.
//! A leftover/partial record always requires reconciliation, never replay.
use std::{
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

impl Invocation<'_> {
    /// Exclusively persist a bounded, private launch intent before any future
    /// installer dispatch. No spawn/phase transition or child-start authority.
    /// Never remove/rewrite an uncertain marker, even when persistence fails.
    pub fn record_start_intent(&self) -> Result<()> {
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
        let result = (|| -> Result<()> {
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
            self.verify_binding()
        })();
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }
}
