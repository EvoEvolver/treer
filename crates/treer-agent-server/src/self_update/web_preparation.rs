//! Fresh #222 submission preparation. This module cannot publish installed
//! names, restart services, transfer locks, or authorize a launcher. The returned
//! owner retains both locks until the separately verified worker handoff.
use std::io::Write;

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::controller_update_runtime::DispatchBinding;

use super::{
    activator::ActivatorImage,
    artifacts::{Binary, Distributor},
    handoff::Endpoint,
    health::Monitor,
    host::Observation,
    publication::Publications,
    selection::{Selected, SelectionLocks},
    verified_image::VerifiedImage,
    Journal, Phase, Receipt, Source,
};
use crate::controller_update::unix::Catalog;

/// A fresh owner, never reconstructed from a receipt. Dropping it releases OS
/// locks but does not remove partial evidence or authorize a second submission.
/// No public fields or Clone: callers cannot manufacture prepared ownership.
pub struct PreparedSubmission {
    selected: Selected,
    locks: SelectionLocks,
    journal: Journal,
    activator: ActivatorImage,
    job_directory: std::path::PathBuf,
}
impl PreparedSubmission {
    pub fn receipt(&self) -> Result<Receipt> {
        // Keep the exact staged activator and original lock ownership live.
        self.activator.verify(&self.journal)?;
        for binary in [Binary::Controller, Binary::Cli] {
            self.locks.directory(&self.selected, binary)?;
        }
        let receipt = self.journal.read()?;
        if receipt.phase != Phase::Prepared
            || self
                .job_directory
                .file_name()
                .and_then(|name| name.to_str())
                != Some(receipt.job_id.to_string().as_str())
        {
            bail!("update_recovery_required");
        }
        Ok(receipt)
    }
}

pub(super) async fn prepare(binding: DispatchBinding) -> Result<PreparedSubmission> {
    if !binding.validate() || binding.plan_digest != binding.expected_plan_digest()? {
        bail!("controller_update_preview_changed");
    }
    // Reject remote/future or stale admission before touching local state. This
    // is an acceptance window, never a lease that permits replay after expiry.
    let now = chrono::Utc::now();
    if binding.created_at > now + chrono::Duration::seconds(30)
        || binding.created_at < now - chrono::Duration::minutes(2)
    {
        bail!("controller_update_preview_changed");
    }
    let catalog = Catalog::local()?;
    let (selected, locks) = catalog.select(&binding.workspace_id, &binding.server_id)?;
    let current = catalog.current(&selected, &locks).await?;
    let target = catalog.target(&binding, current)?;
    let distributor = Distributor::new(
        target.artifact_base.clone(),
        selected.proxy(),
        target.allow_artifact_origin,
    )?;
    // Resolve only fixed Controller-owned roots. Existing private parents may
    // be opened, but an existing UUID is never adopted, overwritten or resumed.
    let state_path = crate::service::state_dir()?;
    let state = PrivateDirectory::open(&state_path)?;
    let updates = child(&state, "self-update")?;
    let jobs = child(&updates, "jobs")?;
    let receipt = Receipt {
        schema_version: 1,
        job_id: super::job_id(&binding.operation_id)?,
        revision: 0,
        workspace_id: binding.workspace_id.clone(),
        server_id: binding.server_id.clone(),
        release_sha: target.target.identity.release_sha.clone(),
        artifact_source_sha: target.target.identity.artifact_source_sha.clone(),
        controller_sha256: target.target.identity.controller_sha256.clone(),
        cli_sha256: target.target.identity.cli_sha256.clone(),
        publications: Publications::new(
            binding.current.controller_sha256.clone(),
            binding.current.cli_sha256.clone(),
            &target.target.identity.controller_sha256,
            &target.target.identity.cli_sha256,
        ),
        source: Source::Pin {
            sums_sha256: target.sums_sha256.clone(),
            allow_pinned: target.allow_pinned,
            allow_artifact_origin: target.allow_artifact_origin,
            allow_same: false,
            allow_downgrade: false,
        },
        phase: Phase::Preparing,
        created_at: binding.created_at,
        updated_at: now.max(binding.created_at),
    };
    let journal = create_bound(&jobs, &receipt, &binding)?;
    // Preserve the local trust/ordering evidence before any download. The
    // original worker must revalidate these exact catalog bytes under its SAME
    // transferred locks before publication; a Proxy plan is not that authority.
    catalog.admission(&binding)?.persist(&journal)?;
    let checksums = distributor.pinned_checksums(&target.sums_sha256).await?;
    if checksums.digest(Binary::Controller) != receipt.controller_sha256
        || checksums.digest(Binary::Cli) != receipt.cli_sha256
    {
        bail!("controller_update_target_unavailable");
    }
    let controller = distributor
        .stage(
            &journal.directory,
            Binary::Controller,
            &receipt.controller_sha256,
            None,
        )
        .await?;
    let cli = distributor
        .stage(&journal.directory, Binary::Cli, &receipt.cli_sha256, None)
        .await?;
    let host = Observation::capture(&selected).await?;
    let initial = Monitor::new()?
        .observe_initial(&selected, &locks, &journal, &host)
        .await?;
    host.save_initial(&selected, &locks, &journal, 0, &initial)
        .await?;
    super::startup_backup::capture(&state, &journal, 0)?;
    let activator = ActivatorImage::prepare(&selected, &locks, &journal)?;
    for artifact in [&controller, &cli] {
        super::images::prepare(
            locks.directory(&selected, artifact.binary())?,
            &journal,
            0,
            artifact.binary(),
            artifact.file(),
        )?;
    }
    // Retain BOTH authenticated staged executable handles across BOTH probes.
    let controller_path = staged_path(&selected, &receipt, Binary::Controller)?;
    let cli_path = staged_path(&selected, &receipt, Binary::Cli)?;
    let controller = VerifiedImage::open(
        locks.directory(&selected, Binary::Controller)?,
        &controller_path,
        &receipt.controller_sha256,
    )?;
    let cli = VerifiedImage::open(
        locks.directory(&selected, Binary::Cli)?,
        &cli_path,
        &receipt.cli_sha256,
    )?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let a = super::probe::version_retained(&controller, Binary::Controller, deadline).await?;
    let b = super::probe::version_retained(&cli, Binary::Cli, deadline).await?;
    controller.verify()?;
    cli.verify()?;
    super::probe::consistent(&a, &b)?;
    if a.source_sha != receipt.artifact_source_sha {
        bail!("controller_update_target_unavailable");
    }
    drop(cli);
    drop(controller);
    host.verify().await?;
    // Reattest the original pair after all potentially slow IO/probes.
    super::web_adapter::inspect_pair(&selected, &locks, &binding.current).await?;
    let job_directory = state_path
        .join("self-update/jobs")
        .join(&binding.operation_id);
    Endpoint::mark_prepared(&journal, &job_directory)?;
    let prepared = PreparedSubmission {
        selected,
        locks,
        journal,
        activator,
        job_directory,
    };
    prepared.receipt()?;
    Ok(prepared)
}
fn child(parent: &PrivateDirectory, name: &str) -> Result<PrivateDirectory> {
    // An unsafe existing entry is never removed/replaced to make this succeed.
    Ok(parent
        .open_child(name)
        .or_else(|_| parent.create_child(name))?)
}
fn staged_path(
    selected: &Selected,
    receipt: &Receipt,
    binary: Binary,
) -> Result<std::path::PathBuf> {
    let installed = match binary {
        Binary::Controller => selected.controller_path().to_owned(),
        Binary::Cli => selected.cli_path(),
    };
    if receipt.publications.step(binary) == super::publication::FileStep::Unchanged {
        return Ok(installed);
    }
    Ok(installed
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?
        .join(super::images::stage(receipt, binary, false)))
}

/// No existing job can become a new submission, even if the exact binding is
/// repeated after a lost response or process restart. Observation uses GET.
fn create_bound(
    jobs: &PrivateDirectory,
    receipt: &Receipt,
    binding: &DispatchBinding,
) -> Result<Journal> {
    super::web_adapter::check(receipt, binding)?;
    if binding.plan_digest != binding.expected_plan_digest()? {
        bail!("controller_update_binding_invalid");
    }
    let bytes = serde_json::to_vec(binding)?;
    if bytes.len() > 64 * 1024 {
        bail!("controller_update_binding_invalid");
    }
    // OS locks do not survive owner death. This installation-wide private
    // claim does: a new UUID must not race a potentially surviving old child.
    // No timeout, receipt label or missing PID removes it. Whole-health and
    // quiescence reconciliation must explicitly release it in the executor.
    let mut active = jobs.create_file("web-active.json")?;
    active.write_all(&bytes)?;
    active.sync_all()?;
    drop(active);
    jobs.sync()?;
    let mut persisted = Vec::new();
    use std::io::Read;
    jobs.read_file("web-active.json")?
        .take(64 * 1024 + 1)
        .read_to_end(&mut persisted)?;
    if persisted != bytes {
        bail!("update_recovery_required");
    }
    let journal = Journal::create(jobs, receipt)?;
    // The durable complete binding precedes artifact IO or executable staging.
    // If any later step fails, preserve this UUID and all evidence; do not retry.
    let mut file = journal.directory.create_file(super::web_adapter::BINDING)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    journal.directory.sync()?;
    if super::web_adapter::read(&journal, &binding.workspace_id, &binding.server_id)?.operation_id
        != binding.operation_id
    {
        bail!("controller_update_binding_invalid");
    }
    Ok(journal)
}

impl PreparedSubmission {
    /// Consumes the fresh submission and offers the SAME lock descriptions once.
    /// Lost acknowledgments never cause another child or handoff attempt.
    pub fn handoff(self) -> Result<treer_protocol::controller_update::Operation> {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        use treer_platform::process_image::ProcessImage;
        self.receipt()?;
        super::handoff::parent::save(&self.journal, &self.selected, &self.locks)?;
        let endpoint = Endpoint::bind(&self.journal, &self.job_directory)?;
        let home = nix::unistd::User::from_uid(nix::unistd::geteuid())?
            .map(|user| user.dir)
            .ok_or_else(|| anyhow::anyhow!("update_worker_account_unavailable"))?;
        let home = home.canonicalize()?;
        let state = crate::service::state_dir()?;
        let config_home = self.selected.launcher_config_home()?;
        let mut command = Command::new(self.job_directory.join("activator"));
        command
            .arg("--controller-update-worker")
            .arg(self.journal.read()?.job_id.to_string())
            .current_dir(&self.job_directory)
            .env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config_home)
            .env("TREER_STATE_DIR", state)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.activator.verify(&self.journal)?;
        // A failed spawn is not retried, and an uncertain spawned child is not
        // killed by dropping a future/response. It owns its bounded bootstrap.
        let mut child = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let peer = loop {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                bail!("update_recovery_required");
            }
            // OS observations may wait for exec/session establishment; this
            // loop never repeats process creation, descriptor transfer or ACK.
            if let Ok(image) = ProcessImage::capture(child.id()) {
                if image.matches_file(self.activator.image())? {
                    if let Some(peer) =
                        endpoint.accept(&image, self.activator.image(), self.activator.digest())?
                    {
                        break peer;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let accepted = peer.offer_ownership(&self.journal, &self.selected, &self.locks)?;
        if accepted.prepared().job_id != self.journal.read()?.job_id {
            bail!("update_recovery_required");
        }
        // Ordinary Child drop does NOT kill a worker. The submitting Controller
        // may be replaced immediately; no normal runtime/metrics helper starts
        // in the private worker, and no parent lifetime grants a retry.
        drop(endpoint);
        let operation = super::web_adapter::read(
            &self.journal,
            self.selected.workspace(),
            self.selected.server_id(),
        )?;
        std::thread::Builder::new()
            .name("controller-update-reap".into())
            .spawn(move || {
                let _ = child.wait();
            })?;
        Ok(operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_protocol::controller_update as wire;

    #[test]
    fn fresh_submission_is_exclusive_and_readable_after_restart_without_replay() {
        let f = super::super::selection::tests::Fixture::new();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let receipt = super::super::tests::receipt();
        let mut binding = DispatchBinding {
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
                target_id: "release-b".into(),
                identity: wire::Identity {
                    release_sha: receipt.release_sha.clone(),
                    artifact_source_sha: receipt.artifact_source_sha.clone(),
                    controller_sha256: receipt.controller_sha256.clone(),
                    cli_sha256: receipt.cli_sha256.clone(),
                },
                platform: wire::Platform::LinuxX86_64,
                version_label: "fixture".into(),
            },
            catalog_revision: 2,
            plan_digest: "0".repeat(64),
            created_at: receipt.created_at,
        };
        // A mismatched plan is refused before creating any UUID directory.
        assert!(create_bound(&jobs, &receipt, &binding).is_err());
        assert!(Journal::open(&jobs, &binding.operation_id).is_err());
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        let journal = create_bound(&jobs, &receipt, &binding).unwrap();
        assert!(create_bound(&jobs, &receipt, &binding).is_err());
        let before =
            super::super::web_adapter::read(&journal, &binding.workspace_id, &binding.server_id)
                .unwrap();
        drop(journal);
        let journal = Journal::open(&jobs, &binding.operation_id).unwrap();
        assert_eq!(
            super::super::web_adapter::read(&journal, &binding.workspace_id, &binding.server_id)
                .unwrap(),
            before
        );
        assert_eq!(journal.read().unwrap(), receipt);
        let mut next = binding.clone();
        next.operation_id = uuid::Uuid::new_v4().to_string();
        let mut next_receipt = receipt.clone();
        next_receipt.job_id = uuid::Uuid::parse_str(&next.operation_id).unwrap();
        // Even after reopening, a different UUID cannot use now-free OS locks
        // to forget an uncertain earlier native worker or installer.
        assert!(create_bound(&jobs, &next_receipt, &next).is_err());
        assert!(Journal::open(&jobs, &next.operation_id).is_err());
        let mut foreign = binding.clone();
        foreign.server_id = "srv_foreign".into();
        assert!(create_bound(&jobs, &receipt, &foreign).is_err());
        assert_eq!(journal.read().unwrap(), receipt);
    }
}
