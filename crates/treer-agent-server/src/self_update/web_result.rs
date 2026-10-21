//! Private whole-outcome projection, written only by the consuming owned worker
//! after verified Core rollback or quiescent launcher completion + whole health.
//! A terminal receipt by itself cannot construct a browser success observation.
use super::{Journal, Phase, Receipt};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};
const NAME: &str = "web-result.json";
const LIMIT: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    schema_version: u8,
    receipt: Receipt,
    binding: DispatchBinding,
    verified_at: chrono::DateTime<chrono::Utc>,
}
fn binding(journal: &Journal) -> Result<DispatchBinding> {
    let mut bytes = Vec::new();
    journal
        .directory
        .read_file(super::web_adapter::BINDING)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("update_recovery_required");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn complete(journal: &Journal, receipt: &Receipt) -> Result<()> {
    if !matches!(receipt.phase, Phase::Complete | Phase::RolledBack) || journal.read()? != *receipt
    {
        bail!("update_recovery_required");
    }
    let binding = binding(journal)?;
    super::web_adapter::check(receipt, &binding)?;
    let result = Outcome {
        schema_version: 1,
        receipt: receipt.clone(),
        binding: binding.clone(),
        verified_at: chrono::Utc::now().max(receipt.updated_at),
    };
    let bytes = serde_json::to_vec(&result)?;
    if bytes.len() as u64 > LIMIT {
        bail!("update_recovery_required");
    }
    let mut file = journal.directory.create_file(NAME)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    journal.directory.sync()?;
    // Release only the exact claim we own, after outcome durability. Neither a
    // raw terminal label, timeout nor missing PID grants this removal.
    let root = PrivateDirectory::open(&crate::service::self_update_jobs_directory()?)?;
    let mut active = Vec::new();
    root.read_file("web-active.json")?
        .take(LIMIT + 1)
        .read_to_end(&mut active)?;
    if active != serde_json::to_vec(&binding)? || journal.read()? != *receipt {
        bail!("update_recovery_required");
    }
    root.remove_file("web-active.json")?;
    root.sync()?;
    let mut release = journal.directory.create_file("web-released.json")?;
    release.write_all(&serde_json::to_vec(&binding)?)?;
    release.sync_all()?;
    drop(release);
    journal.directory.sync()?;
    read(journal, receipt, &binding)?.ok_or_else(|| anyhow::anyhow!("update_recovery_required"))?;
    Ok(())
}

pub(super) fn read(
    journal: &Journal,
    receipt: &Receipt,
    binding: &DispatchBinding,
) -> Result<Option<wire::Operation>> {
    let file = match journal.directory.read_file(NAME) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("controller_update_receipt_invalid"),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("controller_update_receipt_invalid");
    }
    // Whole success also requires durable completion of our own active-claim
    // duty. A crash between outcome write and claim release stays unconfirmed.
    let mut released = Vec::new();
    match journal.directory.read_file("web-released.json") {
        Ok(file) => {
            file.take(LIMIT + 1).read_to_end(&mut released)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("controller_update_receipt_invalid"),
    }
    if released != serde_json::to_vec(binding)? {
        bail!("controller_update_receipt_invalid");
    }
    let result: Outcome = serde_json::from_slice(&bytes)?;
    if result.schema_version != 1
        || result.receipt != *receipt
        || result.binding != *binding
        || result.verified_at < receipt.updated_at
        || result.verified_at > chrono::Utc::now() + chrono::Duration::seconds(30)
        || !matches!(receipt.phase, Phase::Complete | Phase::RolledBack)
    {
        bail!("controller_update_receipt_invalid");
    }
    super::web_adapter::check(receipt, binding)?;
    let rolled_back = receipt.phase == Phase::RolledBack;
    let op = wire::Operation {
        operation_id: binding.operation_id.clone(),
        current: binding.current.clone(),
        target: binding.target.clone(),
        state: if rolled_back {
            wire::State::RolledBack
        } else {
            wire::State::Succeeded
        },
        phase: Some(if rolled_back {
            wire::Phase::RolledBack
        } else {
            wire::Phase::Complete
        }),
        observation: wire::Observation::Available,
        observed_identity: Some(if rolled_back {
            binding.current.clone()
        } else {
            binding.target.identity.clone()
        }),
        restored_identity: rolled_back.then(|| binding.current.clone()),
        health: wire::Health::Healthy,
        receipt_revision: Some(receipt.revision),
        created_at: binding.created_at,
        updated_at: result.verified_at,
        observed_at: Some(result.verified_at),
        finished_at: Some(receipt.updated_at),
        error_code: None,
    };
    op.validate()
        .map_err(|_| anyhow::anyhow!("controller_update_receipt_invalid"))?;
    Ok(Some(op))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_receipt_needs_matching_durable_outcome_and_claim_release() {
        for rollback in [false, true] {
            let f = super::super::selection::tests::Fixture::new();
            let jobs = PrivateDirectory::open(&f.path)
                .unwrap()
                .create_child("jobs")
                .unwrap();
            let (original, binding) = super::super::web_adapter::tests::bound();
            let journal = Journal::create(&jobs, &original).unwrap();
            let mut value = serde_json::to_value(&original).unwrap();
            value["revision"] = serde_json::json!(1);
            value["phase"] = serde_json::json!(if rollback { "rolled_back" } else { "complete" });
            for binary in ["controller", "cli"] {
                value["publications"][binary]["step"] =
                    serde_json::json!(if rollback { "restored" } else { "installed" });
            }
            let receipt: Receipt = serde_json::from_value(value).unwrap();
            receipt.validate().unwrap();
            journal.write(&receipt).unwrap();
            assert!(read(&journal, &receipt, &binding).unwrap().is_none());
            let outcome = Outcome {
                schema_version: 1,
                receipt: receipt.clone(),
                binding: binding.clone(),
                verified_at: receipt.updated_at,
            };
            let mut file = journal.directory.create_file(NAME).unwrap();
            file.write_all(&serde_json::to_vec(&outcome).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            drop(file);
            // Result durability alone cannot erase an unfinished claim duty.
            assert!(read(&journal, &receipt, &binding).unwrap().is_none());
            let mut file = journal.directory.create_file("web-released.json").unwrap();
            file.write_all(&serde_json::to_vec(&binding).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            drop(file);
            let operation = read(&journal, &receipt, &binding).unwrap().unwrap();
            assert_eq!(
                operation.state,
                if rollback {
                    wire::State::RolledBack
                } else {
                    wire::State::Succeeded
                }
            );
            operation.validate().unwrap();
            let mut foreign = binding.clone();
            foreign.target.target_id = "foreign".into();
            assert!(read(&journal, &receipt, &foreign).is_err());
            let mut changed = receipt.clone();
            changed.revision += 1;
            assert!(read(&journal, &changed, &binding).is_err());
        }
    }
}
