//! Read-only #222 projection of a native transaction with a durable submission
//! binding. Native preparation writes the binding under both selection locks.
//! A raw receipt is never proof of whole-update health. The production
//! submitter must create the exclusive binding while it still owns selection
//! locks; observation grants no execution, recovery, replay or lock authority.
use std::io::Read;

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

use super::{artifacts::Binary, Journal, Phase, Receipt};
pub(super) const BINDING: &str = "web-submission.json";
const LIMIT: u64 = 64 * 1024;

pub use super::web_preparation::PreparedSubmission;

/// Fresh submission only. This prepares private artifacts and retains both
/// locks; it grants no activation or launcher authority. The browser capability
/// remains unavailable until its complete executor is wired and verified.
pub async fn prepare(binding: DispatchBinding) -> Result<PreparedSubmission> {
    super::web_preparation::prepare(binding).await
}

pub(crate) fn installed_pair(
    selected: &super::selection::Selected,
    locks: &super::selection::SelectionLocks,
) -> Result<(String, String)> {
    let controller = locks.directory(selected, Binary::Controller)?;
    let cli = locks.directory(selected, Binary::Cli)?;
    Ok((
        super::images::digest_file(&controller.read_file(Binary::Controller.name())?)?,
        super::images::digest_file(&cli.read_file(Binary::Cli.name())?)?,
    ))
}

/// The caller obtains these pins from the checked LOCAL catalog before this
/// function executes either image. Both authentication handles survive BOTH
/// probes, so a later probe cannot replace an earlier image unnoticed.
pub(crate) async fn inspect_pair(
    selected: &super::selection::Selected,
    locks: &super::selection::SelectionLocks,
    trusted: &wire::Identity,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let controller_dir = locks.directory(selected, Binary::Controller)?;
    let cli_dir = locks.directory(selected, Binary::Cli)?;
    let controller = super::verified_image::VerifiedImage::open(
        controller_dir,
        selected.controller_path(),
        &trusted.controller_sha256,
    )?;
    let cli_path = selected.cli_path();
    let cli = super::verified_image::VerifiedImage::open(cli_dir, &cli_path, &trusted.cli_sha256)?;
    let a = super::probe::version_retained(&controller, Binary::Controller, deadline).await?;
    let b = super::probe::version_retained(&cli, Binary::Cli, deadline).await?;
    controller.verify()?;
    cli.verify()?;
    super::probe::consistent(&a, &b)?;
    if a.source_sha != trusted.artifact_source_sha {
        bail!("controller_update_current_unknown");
    }
    Ok(())
}

pub(super) fn check(receipt: &Receipt, binding: &DispatchBinding) -> Result<()> {
    receipt.validate()?;
    if !binding.validate()
        || receipt.job_id.to_string() != binding.operation_id
        || receipt.workspace_id != binding.workspace_id
        || receipt.server_id != binding.server_id
        || receipt.release_sha != binding.target.identity.release_sha
        || receipt.artifact_source_sha != binding.target.identity.artifact_source_sha
        || receipt.controller_sha256 != binding.target.identity.controller_sha256
        || receipt.cli_sha256 != binding.target.identity.cli_sha256
        || receipt.publications.original_digest(Binary::Controller)
            != binding.current.controller_sha256
        || receipt.publications.original_digest(Binary::Cli) != binding.current.cli_sha256
        || receipt.created_at != binding.created_at
        || receipt.updated_at > chrono::Utc::now() + chrono::Duration::seconds(30)
    {
        bail!("controller_update_receipt_invalid");
    }
    Ok(())
}

/// Read exactly one fixed, private job. No directory creation, receipt changes,
/// service calls, process probes, installation locks or retry are permitted.
/// Old #111 jobs without the #222 submission binding are not adopted.
pub fn operation(workspace: &str, server: &str, id: &str) -> Result<wire::Operation> {
    super::job_id(id)?;
    let root = crate::service::self_update_jobs_directory()?;
    let jobs = PrivateDirectory::open(&root)?;
    let journal = Journal::open(&jobs, id)?;
    read(&journal, workspace, server)
        .map_err(|_| anyhow::anyhow!("controller_update_receipt_invalid"))
}

pub(super) fn read(journal: &Journal, workspace: &str, server: &str) -> Result<wire::Operation> {
    let receipt = journal.read()?;
    let mut bytes = Vec::new();
    journal
        .directory
        .read_file(BINDING)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("controller_update_receipt_invalid");
    }
    let binding: DispatchBinding = serde_json::from_slice(&bytes)?;
    if binding.workspace_id != workspace || binding.server_id != server {
        bail!("controller_update_operation_not_found");
    }
    check(&receipt, &binding)?;
    // Read back after the binding; concurrent publication is retried only by a
    // later GET, never by looping here or assuming the old receipt is current.
    if journal.read()? != receipt {
        bail!("controller_update_receipt_invalid");
    }
    if let Some(operation) = super::web_result::read(journal, &receipt, &binding)? {
        return Ok(operation);
    }
    // A pending final-evidence write must not publish an unconfirmed terminal
    // revision then conflict with success at that SAME revision on a later GET.
    // Refusing this observation preserves the Proxy's previous validated phase.
    if matches!(receipt.phase, Phase::Complete | Phase::RolledBack) {
        bail!("controller_update_receipt_unconfirmed");
    }
    project(&receipt, &binding)
}

fn project(receipt: &Receipt, binding: &DispatchBinding) -> Result<wire::Operation> {
    check(receipt, binding)?;
    let phase = match receipt.phase {
        Phase::Preparing => wire::Phase::Preparing,
        Phase::Prepared => wire::Phase::Prepared,
        Phase::Activating => wire::Phase::Activating,
        Phase::CoreHealthy => wire::Phase::CoreHealthy,
        Phase::LauncherInstalling => wire::Phase::LauncherInstalling,
        Phase::RollingBack => wire::Phase::RollingBack,
        Phase::Complete => wire::Phase::Complete,
        Phase::Failed => wire::Phase::Failed,
        Phase::RolledBack => wire::Phase::RolledBack,
        Phase::RollbackFailed => wire::Phase::RollbackFailed,
        Phase::Partial => wire::Phase::Partial,
    };
    // Terminal receipt labels alone cannot release the machine claim. Even
    // Complete/RolledBack need independently correlated whole-health evidence,
    // which the complete worker/launcher executor must publish independently.
    let terminal = receipt.phase.terminal();
    let op = wire::Operation {
        operation_id: binding.operation_id.clone(),
        current: binding.current.clone(),
        target: binding.target.clone(),
        state: match receipt.phase {
            Phase::Partial => wire::State::Partial,
            Phase::RollbackFailed => wire::State::RollbackFailed,
            _ if terminal => wire::State::Unconfirmed,
            _ => wire::State::Updating,
        },
        phase: Some(phase),
        observation: wire::Observation::Available,
        observed_identity: None,
        restored_identity: None,
        health: wire::Health::Unknown,
        receipt_revision: Some(receipt.revision),
        created_at: binding.created_at,
        updated_at: receipt.updated_at,
        observed_at: None,
        // A stopped failed worker is not proof of child quiescence. These
        // distinct recovery states retain the machine slot just like unknown.
        finished_at: matches!(receipt.phase, Phase::Partial | Phase::RollbackFailed)
            .then_some(receipt.updated_at),
        error_code: terminal.then_some(
            if matches!(receipt.phase, Phase::Partial | Phase::RollbackFailed) {
                wire::UpdateError::RecoveryRequired
            } else {
                wire::UpdateError::ResultUnconfirmed
            },
        ),
    };
    op.validate()
        .map_err(|_| anyhow::anyhow!("controller_update_receipt_invalid"))?;
    Ok(op)
}

/// Synchronous pre-runtime branch for the job-local original-image worker.
/// A plain UUID, stale receipt, guessed argv or copied executable grants no
/// ownership; native OS image/parent/peer checks and same-lock transfer follow.
pub fn enter_worker(selector: &str) -> Result<super::worker::OwnedWorker> {
    super::job_id(selector)?;
    let jobs = PrivateDirectory::open(&crate::service::self_update_jobs_directory()?)?;
    let journal = Journal::open(&jobs, selector)?;
    let receipt = journal.read()?;
    let _ = read(&journal, &receipt.workspace_id, &receipt.server_id)?;
    let mut active = Vec::new();
    jobs.read_file("web-active.json")?
        .take(LIMIT + 1)
        .read_to_end(&mut active)?;
    if active.len() as u64 > LIMIT {
        bail!("update_recovery_required");
    }
    let active: DispatchBinding = serde_json::from_slice(&active)?;
    check(&receipt, &active)?;
    if active.operation_id != selector {
        bail!("update_recovery_required");
    }
    // Receipt projection checked the fixed binding; also require exact claim
    // equality so another job/session's active claim cannot authorize startup.
    let mut bytes = Vec::new();
    journal
        .directory
        .read_file(BINDING)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let binding: DispatchBinding = serde_json::from_slice(&bytes)?;
    if binding != active {
        bail!("update_recovery_required");
    }
    super::worker::WorkerResources::open(selector)?.enter()
}

pub fn run_worker(mut worker: super::worker::OwnedWorker) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        match worker.begin_core()?.run().await {
            super::worker::CoreProgress::LauncherRequired => worker.finish_launcher().await,
            super::worker::CoreProgress::RolledBack => worker.finish_rollback(),
            super::worker::CoreProgress::RecoveryRequired => bail!("update_recovery_required"),
        }
    })
}

/// Holds every local installation touched by the legacy CLI updater, so it
/// cannot bypass a browser update's original installation locks/active claim.
/// CLI invocation already explicitly updates shared local binaries; it receives
/// no catalog or browser execution authority from this guard.
pub struct LegacyUpdateGuard {
    _held: Vec<(super::selection::Selected, super::selection::SelectionLocks)>,
}
pub fn legacy_update_guard(
    services: &[(std::path::PathBuf, String)],
    cli: &std::path::Path,
) -> Result<LegacyUpdateGuard> {
    legacy_guard(
        &super::selection::Inventory::local()?,
        services,
        cli,
        &crate::service::self_update_jobs_directory()?,
    )
}
fn legacy_guard(
    inventory: &super::selection::Inventory,
    services: &[(std::path::PathBuf, String)],
    cli: &std::path::Path,
    jobs_root: &std::path::Path,
) -> Result<LegacyUpdateGuard> {
    let mut installations = std::collections::BTreeMap::new();
    for (index, (controller, workspace)) in services.iter().enumerate() {
        let selected = inventory.select(Some(workspace), true)?;
        if selected.controller_path() != controller || (index == 0 && selected.cli_path() != cli) {
            bail!("update_installation_binding_mismatch");
        }
        let parent = selected
            .controller_path()
            .parent()
            .ok_or_else(|| anyhow::anyhow!("update_installation_binding_mismatch"))?
            .to_owned();
        installations.entry(parent).or_insert(selected);
    }
    if installations.is_empty() {
        bail!("update_installation_binding_mismatch");
    }
    let mut held = Vec::new();
    for selected in installations.into_values() {
        let locks = selected.lock()?;
        held.push((selected, locks));
    }
    match PrivateDirectory::open(jobs_root) {
        Ok(jobs) => match jobs.read_file("web-active.json") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => bail!("update_recovery_required"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => bail!("update_recovery_required"),
    }
    Ok(LegacyUpdateGuard { _held: held })
}

#[cfg(test)]
pub(in crate::self_update) mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::DirBuilderExt;

    pub(in crate::self_update) fn bound() -> (Receipt, DispatchBinding) {
        let receipt = super::super::tests::receipt();
        let binding = DispatchBinding {
            operation_id: receipt.job_id.to_string(),
            workspace_id: receipt.workspace_id.clone(),
            server_id: receipt.server_id.clone(),
            current: wire::Identity {
                release_sha: "0".repeat(40),
                artifact_source_sha: "0".repeat(40),
                controller_sha256: "f".repeat(64),
                cli_sha256: "0".repeat(64),
            },
            target: wire::Target {
                target_id: "release-a".into(),
                identity: wire::Identity {
                    release_sha: receipt.release_sha.clone(),
                    artifact_source_sha: receipt.artifact_source_sha.clone(),
                    controller_sha256: receipt.controller_sha256.clone(),
                    cli_sha256: receipt.cli_sha256.clone(),
                },
                platform: wire::Platform::LinuxX86_64,
                version_label: "fixture".into(),
            },
            catalog_revision: 1,
            plan_digest: "a".repeat(64),
            created_at: receipt.created_at,
        };
        (receipt, binding)
    }

    #[test]
    fn real_private_journal_is_readonly_and_binding_survives_reopen() {
        let (receipt, binding) = bound();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("treer-web-update-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let jobs = PrivateDirectory::open(&root).unwrap();
        let journal = Journal::create(&jobs, &receipt).unwrap();
        assert!(read(&journal, &binding.workspace_id, &binding.server_id).is_err());
        // Exercise the fixed binding reader independently of preparation.
        // Writing these fixture bytes grants no activation ownership.
        let mut file = journal.directory.create_file(BINDING).unwrap();
        file.write_all(&serde_json::to_vec(&binding).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        journal.directory.sync().unwrap();
        let op = read(&journal, &binding.workspace_id, &binding.server_id).unwrap();
        assert_eq!(op.state, wire::State::Updating);
        assert_eq!(op.phase, Some(wire::Phase::Preparing));
        assert!(read(&journal, "ws_other", &binding.server_id).is_err());
        drop(journal);
        let reopened = Journal::open(&jobs, &binding.operation_id).unwrap();
        assert_eq!(
            read(&reopened, &binding.workspace_id, &binding.server_id).unwrap(),
            op
        );
        assert_eq!(reopened.read().unwrap(), receipt);
        drop(reopened);
        drop(jobs);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_target_original_images_or_job_cannot_be_projected() {
        let (receipt, binding) = bound();
        for field in 0..5 {
            let mut changed = binding.clone();
            match field {
                0 => changed.target.identity.controller_sha256 = "e".repeat(64),
                1 => changed.current.cli_sha256 = "e".repeat(64),
                2 => changed.operation_id = uuid::Uuid::new_v4().to_string(),
                3 => changed.workspace_id = "ws_other".into(),
                _ => changed.created_at += chrono::Duration::seconds(1),
            }
            assert!(project(&receipt, &changed).is_err());
        }
    }

    #[test]
    fn raw_complete_receipt_is_never_success_or_machine_slot_release() {
        let (mut receipt, binding) = bound();
        // Construct a structurally valid terminal journal projection without any
        // health authority, using installed publication labels only.
        let mut data = serde_json::to_value(&receipt).unwrap();
        data["phase"] = serde_json::json!("complete");
        data["revision"] = serde_json::json!(1);
        data["publications"]["controller"]["step"] = serde_json::json!("installed");
        data["publications"]["cli"]["step"] = serde_json::json!("installed");
        receipt = serde_json::from_value(data).unwrap();
        receipt.validate().unwrap();
        let op = project(&receipt, &binding).unwrap();
        assert_eq!(op.state, wire::State::Unconfirmed);
        assert_eq!(op.health, wire::Health::Unknown);
        assert!(op.finished_at.is_none());
        assert!(op.observed_identity.is_none());
    }
    #[test]
    fn partial_and_failed_rollback_stay_distinct_without_health_or_slot_release() {
        let (base, binding) = bound();
        for (phase, state) in [
            (Phase::Partial, wire::State::Partial),
            (Phase::RollbackFailed, wire::State::RollbackFailed),
        ] {
            let mut data = serde_json::to_value(&base).unwrap();
            data["phase"] = serde_json::to_value(phase).unwrap();
            data["revision"] = serde_json::json!(1);
            if phase == Phase::Partial {
                data["publications"]["controller"]["step"] = serde_json::json!("installed");
                data["publications"]["cli"]["step"] = serde_json::json!("installed");
            }
            let receipt: Receipt = serde_json::from_value(data).unwrap();
            receipt.validate().unwrap();
            let op = project(&receipt, &binding).unwrap();
            assert_eq!(op.state, state);
            assert_eq!(op.health, wire::Health::Unknown);
            assert!(op.observed_identity.is_none());
            assert!(op.restored_identity.is_none());
            assert_eq!(op.error_code, Some(wire::UpdateError::RecoveryRequired));
        }
    }
    #[test]
    fn legacy_cli_holds_shared_installation_and_refuses_uncertain_browser_claim() {
        let f = super::super::selection::tests::Fixture::standard_config();
        let controller = f.bin.join(Binary::Controller.name());
        f.enrollment("srv_a", "ws_a", &controller);
        f.enrollment("srv_b", "ws_b", &controller);
        let services = vec![
            (controller.clone(), "ws_a".into()),
            (controller, "ws_b".into()),
        ];
        let cli = f.bin.join(Binary::Cli.name());
        let inventory = f.inventory();
        let jobs_path = f.path.join("jobs");
        let guard = legacy_guard(&inventory, &services, &cli, &jobs_path).unwrap();
        assert!(inventory
            .select(Some("ws_a"), true)
            .unwrap()
            .lock()
            .is_err());
        assert!(inventory
            .select(Some("ws_b"), true)
            .unwrap()
            .lock()
            .is_err());
        drop(guard);
        assert!(inventory.select(Some("ws_a"), true).unwrap().lock().is_ok());
        assert!(legacy_guard(
            &inventory,
            &services,
            &f.path.join("ambient-cli"),
            &jobs_path
        )
        .is_err());
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        // Even corrupt/empty uncertain claims are not treated as absent.
        jobs.create_file("web-active.json")
            .unwrap()
            .sync_all()
            .unwrap();
        assert!(legacy_guard(&inventory, &services, &cli, &jobs_path).is_err());
        assert!(jobs.read_file("web-active.json").is_ok());
        assert!(inventory.select(Some("ws_a"), true).unwrap().lock().is_ok());
    }
}
