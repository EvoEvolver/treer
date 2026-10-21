//! Direct owned launcher completion. The original transferred-lock worker owns
//! each child itself; there is no detached supervisor or permit/EOF adoption.
//! An exclusive durable intent precedes each spawn. Any ambiguous child outcome
//! stays unresolved forever unless explicitly reconciled; it cannot be retried.
use anyhow::{bail, Result};

use super::{CoreProgress, OwnedWorker};
use crate::self_update::{
    artifacts::Binary,
    health::Monitor,
    host::Observation,
    launcher_invocation::{Invocation, Operation},
    launcher_temporary::LauncherTemporary,
    safety::{ControllerExpectation, HostSnapshot},
    verified_image::VerifiedImage,
    Phase, Receipt,
};

impl OwnedWorker {
    /// Consumes the original worker. Only its fresh successful Core run can
    /// enter this path. Receipt files or copied metadata cannot mint ownership.
    /// Errors never restart a child or attempt rollback after launcher mutation.
    pub async fn finish_launcher(self) -> Result<()> {
        if self.progress != CoreProgress::LauncherRequired || !self.core_started {
            bail!("update_recovery_required");
        }
        let result = self.launcher_once().await;
        if result.is_err() {
            // Preserve uncertainty and installation-wide active claim. Partial
            // does not assert child quiescence, success, or permission to retry.
            if let Ok(receipt) = self.journal.read() {
                if matches!(
                    receipt.phase,
                    Phase::CoreHealthy | Phase::LauncherInstalling
                ) {
                    let _ = self.journal.advance(receipt.revision, Phase::Partial);
                }
            }
        }
        result.map_err(|_| anyhow::anyhow!("update_recovery_required"))
    }

    async fn launcher_once(&self) -> Result<()> {
        crate::self_update::launcher_preflight::check(&self.selected, &self.owned, &self.journal)
            .await?;
        let root = crate::service::self_update_jobs_directory()?;
        let job = root.join(self.owned.prepared().job_id.to_string());
        let temporary =
            LauncherTemporary::create(&self.selected, &self.owned, &self.journal, &job)?;
        let install = Invocation::prepare(
            &self.selected,
            &self.owned,
            &self.journal,
            &temporary,
            Operation::Install,
        )?;
        install.check_node_version().await?;
        let install_output = execute(&install).await?;
        let status = Invocation::prepare(
            &self.selected,
            &self.owned,
            &self.journal,
            &temporary,
            Operation::Status,
        )?;
        let status_output = execute(&status).await?;
        let receipt = self.journal.read()?;
        let report = crate::self_update::launcher_report::compare(
            install_output.stdout(),
            status_output.stdout(),
            &receipt.artifact_source_sha,
        )?;
        if report.source_commit() != receipt.artifact_source_sha {
            bail!("update_launcher_report_mismatch");
        }
        // Recheck the SAME live Host/workloads and replacement Controller image,
        // epoch and CLI after both complete, quiescent launcher invocations.
        let (host, previous_epoch) =
            Observation::recover(&self.selected, self.owned.locks(), &self.journal).await?;
        let (_, saved_epoch, _) = HostSnapshot::load(&self.journal)?;
        if previous_epoch != saved_epoch {
            bail!("update_original_baseline_mismatch");
        }
        let expected = ControllerExpectation {
            workspace: receipt.workspace_id.clone(),
            server: receipt.server_id.clone(),
            previous_epoch,
            artifact_source_sha: receipt.artifact_source_sha.clone(),
            executable_sha256: receipt.controller_sha256.clone(),
        };
        Monitor::new()?
            .observe_live(&self.selected, self.owned.locks(), &expected, &host)
            .await?;
        crate::self_update::launcher_preflight::check(&self.selected, &self.owned, &self.journal)
            .await?;
        temporary.verify()?;
        install.verify_binding()?;
        status.verify_binding()?;
        // CoreHealthy remains the last validated checkpoint during child work.
        // No intermediate phase change may revoke the held invocation recipe.
        // Only after quiescence and whole health do we publish final checkpoints.
        let installing = self
            .journal
            .advance(receipt.revision, Phase::LauncherInstalling)?;
        let complete = self.journal.advance(installing.revision, Phase::Complete)?;
        crate::self_update::web_result::complete(&self.journal, &complete)?;
        Ok(())
    }

    /// Core's verified rollback outcome already includes original image, epoch,
    /// Host and workload health. This consumes the one original owned worker.
    pub fn finish_rollback(self) -> Result<()> {
        if self.progress != CoreProgress::RolledBack || !self.core_started {
            bail!("update_recovery_required");
        }
        let receipt = self.journal.read()?;
        if receipt.phase != Phase::RolledBack {
            bail!("update_recovery_required");
        }
        crate::self_update::web_result::complete(&self.journal, &receipt)
    }
}

pub(in crate::self_update) async fn execute(
    invocation: &Invocation<'_>,
) -> Result<treer_platform::bounded_child::QuiescentOutput> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let receipt: Receipt = invocation.bound_receipt()?;
    let selected = invocation.selected();
    let locks = invocation.owner().locks();
    let cli_path = selected.cli_path();
    // Retain both exact authenticated inodes throughout child execution and
    // observed group quiescence, not merely a preflight hash/path comparison.
    let controller = VerifiedImage::open(
        locks.directory(selected, Binary::Controller)?,
        selected.controller_path(),
        &receipt.controller_sha256,
    )?;
    let cli = VerifiedImage::open(
        locks.directory(selected, Binary::Cli)?,
        &cli_path,
        &receipt.cli_sha256,
    )?;
    let intent = invocation.record_start_intent()?;
    intent.verify()?;
    #[cfg(test)]
    death_checkpoint("intent");
    controller.verify()?;
    cli.verify()?;
    let mut command = std::process::Command::new(invocation.program());
    command
        .args(invocation.arguments())
        .current_dir(invocation.current_directory())
        .env_clear()
        .envs(invocation.environment());
    // This is lifecycle proof for trusted cooperating installers, not a sandbox:
    // descendants must retain their inherited process group and stay signalable.
    // Failure/cancellation yields no quiescent witness and cannot be a retry.
    let mut child_record = None;
    let output = treer_platform::bounded_child::output_quiescent_observed_until(
        &mut command,
        deadline,
        4 * 1024 * 1024,
        |child| {
            #[cfg(test)]
            death_checkpoint("spawned");
            let result = (|| -> Result<_> {
                intent.verify()?;
                let image = treer_platform::process_image::ProcessImage::capture(child.id())?;
                controller.verify_process(&image)?;
                let record = intent.record_child(image)?;
                controller.verify()?;
                cli.verify()?;
                Ok(record)
            })();
            match result {
                Ok(record) => {
                    child_record = Some(record);
                    #[cfg(test)]
                    death_checkpoint("child_record");
                    Ok(())
                }
                Err(_) => Err(std::io::Error::other("update_recovery_required")),
            }
        },
    )
    .await?;
    child_record
        .ok_or_else(|| anyhow::anyhow!("update_recovery_required"))?
        .verify()?;
    intent.verify()?;
    controller.verify()?;
    cli.verify()?;
    if !output.status().success() {
        bail!("update_launcher_failed");
    }
    if tokio::time::Instant::now() >= deadline {
        bail!("update_recovery_required");
    }
    Ok(output)
}

// Test-only synchronization in the real executor, never a production switch.
#[cfg(test)]
pub(in crate::self_update) fn death_checkpoint(stage: &str) {
    if std::env::var("TREER_TEST_LAUNCHER_DEATH_STAGE").as_deref() != Ok(stage) {
        return;
    }
    let ready = std::env::var_os("TREER_TEST_LAUNCHER_DEATH_READY").unwrap();
    std::fs::write(ready, b"ready").unwrap();
    loop {
        std::thread::park();
    }
}
