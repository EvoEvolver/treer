//! Private parent bootstrap, not a public receipt or authorization boundary.
//! The child obtains its PID relationship from the OS, never argv/environment.
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};

use super::*;
use crate::self_update::{artifacts::Binary, selection::SelectionLocks};

const NAME: &str = "handoff-parent.json";
const LIMIT: u64 = 96 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Parent {
    schema_version: u32,
    prepared: Receipt,
    pid: u32,
    creation: String,
    device: u64,
    inode: u64,
}

fn checked(journal: &Journal, selected: &Selected) -> Result<Receipt> {
    let receipt = journal.read()?;
    if receipt.phase != Phase::Prepared
        || receipt.workspace_id != selected.workspace()
        || receipt.server_id != selected.server_id()
    {
        bail!("update_handoff_parent_binding_mismatch");
    }
    Ok(receipt)
}

/// Before spawning the private activator, bind the live submitting process to
/// the selected installed old Controller under both selection locks. Exclusive
/// creation retains existing/partial evidence instead of adopting or replacing
/// a record from an earlier attempt. No path, argv or credential is serialized.
pub fn save(journal: &Journal, selected: &Selected, locks: &SelectionLocks) -> Result<()> {
    let prepared = checked(journal, selected)?;
    let directory = locks.directory(selected, Binary::Controller)?;
    let installed = directory.read_file(Binary::Controller.name())?;
    let process = ProcessImage::capture(std::process::id())
        .map_err(|_| anyhow::anyhow!("update_handoff_parent_unavailable"))?;
    let identity = installed.metadata()?;
    if !process.matches_file(&installed)?
        || digest_file(&installed)? != prepared.publications.original_digest(Binary::Controller)
        || digest_file(process.executable())?
            != prepared.publications.original_digest(Binary::Controller)
    {
        bail!("update_handoff_parent_image_mismatch");
    }
    process
        .verify_current()
        .map_err(|_| anyhow::anyhow!("update_handoff_parent_changed"))?;
    let record = Parent {
        schema_version: 1,
        prepared: prepared.clone(),
        pid: process.pid(),
        creation: process.creation_identity().to_owned(),
        device: identity.dev(),
        inode: identity.ino(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| anyhow::anyhow!("invalid_update_handoff_parent"))?;
    if bytes.len() as u64 > LIMIT || journal.read()? != prepared {
        bail!("update_handoff_parent_binding_mismatch");
    }
    let mut file = journal.directory.create_file(NAME)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    journal.directory.sync()?;
    process
        .verify_current()
        .map_err(|_| anyhow::anyhow!("update_handoff_parent_changed"))?;
    if journal.read()? != prepared {
        bail!("update_handoff_parent_binding_mismatch");
    }
    Ok(())
}

/// Synchronous child bootstrap, after RunningActivator::enter and before any
/// connect/runtime. A dead/reparented submitter, reused PID, exec, changed image
/// or changed full receipt is fatal. The returned proof still MUST authenticate
/// the exact connected socket peer before any message or descriptor exchange.
pub fn load(journal: &Journal, selected: &Selected) -> Result<ProcessImage> {
    let prepared = checked(journal, selected)?;
    let file = journal.directory.read_file(NAME)?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("invalid_update_handoff_parent");
    }
    let record: Parent = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid_update_handoff_parent"))?;
    if record.schema_version != 1
        || record.prepared != prepared
        || record.pid == 0
        || record.pid != nix::unistd::getppid().as_raw() as u32
    {
        bail!("update_handoff_parent_binding_mismatch");
    }
    let process = ProcessImage::capture(record.pid)
        .map_err(|_| anyhow::anyhow!("update_handoff_parent_unavailable"))?;
    let identity = process.executable().metadata()?;
    if process.creation_identity() != record.creation
        || (identity.dev(), identity.ino()) != (record.device, record.inode)
        || digest_file(process.executable())?
            != prepared.publications.original_digest(Binary::Controller)
    {
        bail!("update_handoff_parent_image_mismatch");
    }
    process
        .verify_current()
        .map_err(|_| anyhow::anyhow!("update_handoff_parent_changed"))?;
    if record.pid != nix::unistd::getppid().as_raw() as u32 || journal.read()? != prepared {
        bail!("update_handoff_parent_binding_mismatch");
    }
    Ok(process)
}
