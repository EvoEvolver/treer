//! One-shot Core execution while the caller retains the owned worker.
//! No launcher work, production dispatch or worker-death recovery entry.
use anyhow::{bail, Result};

use super::OwnedWorker;
use crate::self_update::{activation, batch::same_job, Phase, Receipt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "Core progress is not whole-update completion; retain ownership and handle it"]
pub enum CoreProgress {
    RecoveryRequired,
    LauncherRequired,
    RolledBack,
}

impl CoreProgress {
    pub fn label(self) -> &'static str {
        match self {
            Self::RecoveryRequired => "update_recovery_required",
            Self::LauncherRequired => "launcher_required",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// Borrows the owner, so dropping/cancelling this run does not itself drop the
/// transferred locks. The owner already reports recovery-required before any
/// asynchronous work; cancellation never authorizes another activation attempt.
#[must_use = "dropping an uncompleted Core run leaves update_recovery_required"]
pub struct CoreRun<'a> {
    worker: &'a mut OwnedWorker,
}

impl OwnedWorker {
    /// Synchronous and one-shot, before constructing/polling the async run.
    /// Even a dropped, never-polled run cannot reset this attempt or release
    /// ownership. The future main entry must handle progress before exiting.
    pub fn begin_core(&mut self) -> Result<CoreRun<'_>> {
        if self.core_started {
            bail!("update_recovery_required");
        }
        self.core_started = true;
        if !self
            .journal
            .read()
            .is_ok_and(|current| current == *self.owned.prepared())
        {
            bail!("update_recovery_required");
        }
        Ok(CoreRun { worker: self })
    }
}

// Private seam: callers cannot inject an ACK-only or fabricated health engine.
trait Engine {
    async fn activate(&self, worker: &OwnedWorker) -> Result<activation::CoreActivation>;
    async fn rollback(&self, worker: &OwnedWorker) -> Result<Receipt>;
}

struct Live;

impl Engine for Live {
    async fn activate(&self, worker: &OwnedWorker) -> Result<activation::CoreActivation> {
        activation::activate_core(&worker.selected, &worker.owned, &worker.journal).await
    }

    async fn rollback(&self, worker: &OwnedWorker) -> Result<Receipt> {
        activation::rollback_core(&worker.selected, &worker.owned, &worker.journal).await
    }
}

impl CoreRun<'_> {
    pub async fn run(self) -> CoreProgress {
        self.drive(&Live).await
    }

    fn checked(&self, receipt: &Receipt, phase: Phase) -> bool {
        receipt.phase == phase
            && same_job(receipt, self.worker.owned.prepared())
            && self
                .worker
                .journal
                .read()
                .is_ok_and(|current| current == *receipt)
    }

    async fn drive(self, engine: &impl Engine) -> CoreProgress {
        let progress = match engine.activate(self.worker).await {
            Ok(activation::CoreActivation::CoreHealthy(receipt))
                if self.checked(&receipt, Phase::CoreHealthy) =>
            {
                CoreProgress::LauncherRequired
            }
            Ok(activation::CoreActivation::ImagesRestored(receipt))
                if self.checked(&receipt, Phase::RollingBack) =>
            {
                // Only this explicit, bound restoration outcome authorizes the
                // rollback coordinator. An error/uncertain result never retries
                // activation or performs an extra restart/rollback attempt.
                match engine.rollback(self.worker).await {
                    Ok(receipt) if self.checked(&receipt, Phase::RolledBack) => {
                        CoreProgress::RolledBack
                    }
                    _ => CoreProgress::RecoveryRequired,
                }
            }
            _ => CoreProgress::RecoveryRequired,
        };
        self.worker.progress = progress;
        progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{batch::tests::fixture, batch::ImageBatch};
    use std::cell::Cell;

    struct Fake {
        outcome: &'static str,
        calls: Cell<(usize, usize)>,
    }

    impl Engine for Fake {
        async fn activate(&self, worker: &OwnedWorker) -> Result<activation::CoreActivation> {
            let (a, r) = self.calls.get();
            self.calls.set((a + 1, r));
            assert!(worker.selected.lock().is_err());
            if self.outcome == "setup_error" {
                bail!("secret/path/diagnostic");
            }
            if self.outcome == "pending" {
                return std::future::pending().await;
            }
            if self.outcome == "unproven_prepared" {
                return Ok(activation::CoreActivation::CoreHealthy(
                    worker.journal.read()?,
                ));
            }
            if self.outcome == "unproven_restore" {
                return Ok(activation::CoreActivation::ImagesRestored(
                    worker.journal.read()?,
                ));
            }
            let published =
                ImageBatch::begin(&worker.selected, &worker.owned, &worker.journal)?.publish()?;
            let crate::self_update::batch::PublicationResult::Published(receipt) = published else {
                panic!("fixture publication failed")
            };
            if self.outcome == "pending_after_publish" {
                return std::future::pending().await;
            }
            if self.outcome.starts_with("rollback") || self.outcome == "foreign_restore" {
                let mut receipt = crate::self_update::batch::restore_images(
                    &worker.selected,
                    &worker.owned,
                    &worker.journal,
                )?;
                if self.outcome == "foreign_restore" {
                    receipt.server_id = "srv_foreign".into();
                }
                return Ok(activation::CoreActivation::ImagesRestored(receipt));
            }
            if self.outcome == "post_publish_error" {
                bail!("secret/path/diagnostic");
            }
            let mut receipt = worker
                .journal
                .advance(receipt.revision, Phase::CoreHealthy)?;
            if self.outcome == "foreign_result" {
                receipt.server_id = "srv_foreign".into();
            }
            if self.outcome == "stale_result" {
                worker.journal.advance(receipt.revision, Phase::Partial)?;
            }
            Ok(activation::CoreActivation::CoreHealthy(receipt))
        }

        async fn rollback(&self, worker: &OwnedWorker) -> Result<Receipt> {
            let (a, r) = self.calls.get();
            self.calls.set((a, r + 1));
            assert!(worker.selected.lock().is_err());
            if self.outcome == "rollback_error" {
                bail!("secret/path/diagnostic");
            }
            let receipt = worker.journal.read()?;
            let mut result = worker.journal.advance(
                receipt.revision,
                if self.outcome == "rollback_failed" {
                    Phase::RollbackFailed
                } else {
                    Phase::RolledBack
                },
            )?;
            if self.outcome == "rollback_foreign" {
                result.server_id = "srv_foreign".into();
            }
            Ok(result)
        }
    }

    fn fake(outcome: &'static str) -> Fake {
        Fake {
            outcome,
            calls: Cell::new((0, 0)),
        }
    }

    fn owned() -> (crate::self_update::selection::tests::Fixture, OwnedWorker) {
        let (f, selected, owned, journal) = fixture();
        (
            f,
            OwnedWorker {
                selected,
                owned,
                journal,
                core_started: false,
                progress: CoreProgress::RecoveryRequired,
            },
        )
    }

    #[tokio::test]
    async fn complete_outcome_matrix_retains_ownership_and_never_retries() {
        for (outcome, expected, phase, rollbacks) in [
            (
                "healthy",
                CoreProgress::LauncherRequired,
                Phase::CoreHealthy,
                0,
            ),
            (
                "setup_error",
                CoreProgress::RecoveryRequired,
                Phase::Prepared,
                0,
            ),
            (
                "unproven_prepared",
                CoreProgress::RecoveryRequired,
                Phase::Prepared,
                0,
            ),
            (
                "unproven_restore",
                CoreProgress::RecoveryRequired,
                Phase::Prepared,
                0,
            ),
            (
                "post_publish_error",
                CoreProgress::RecoveryRequired,
                Phase::Activating,
                0,
            ),
            (
                "foreign_result",
                CoreProgress::RecoveryRequired,
                Phase::CoreHealthy,
                0,
            ),
            (
                "stale_result",
                CoreProgress::RecoveryRequired,
                Phase::Partial,
                0,
            ),
            (
                "foreign_restore",
                CoreProgress::RecoveryRequired,
                Phase::RollingBack,
                0,
            ),
            (
                "rollback_ok",
                CoreProgress::RolledBack,
                Phase::RolledBack,
                1,
            ),
            (
                "rollback_error",
                CoreProgress::RecoveryRequired,
                Phase::RollingBack,
                1,
            ),
            (
                "rollback_failed",
                CoreProgress::RecoveryRequired,
                Phase::RollbackFailed,
                1,
            ),
            (
                "rollback_foreign",
                CoreProgress::RecoveryRequired,
                Phase::RolledBack,
                1,
            ),
        ] {
            let (_f, mut worker) = owned();
            let runtime = fake(outcome);
            assert_eq!(worker.begin_core().unwrap().drive(&runtime).await, expected);
            assert_eq!(worker.progress(), expected);
            assert_eq!(worker.journal.read().unwrap().phase, phase);
            assert!(worker.selected.lock().is_err());
            assert!(worker.begin_core().is_err());
            assert_eq!(runtime.calls.get(), (1, rollbacks));
            if expected == CoreProgress::RecoveryRequired {
                assert_eq!(worker.progress().label(), "update_recovery_required");
            }
        }
    }

    #[tokio::test]
    async fn unpolled_or_cancelled_run_retains_locks_and_fixed_recovery_status() {
        for (polled, outcome, phase) in [
            (false, "pending", Phase::Prepared),
            (true, "pending", Phase::Prepared),
            (true, "pending_after_publish", Phase::Activating),
        ] {
            let (_f, mut worker) = owned();
            let prepared = worker.journal.read().unwrap();
            let runtime = fake(outcome);
            let run = worker.begin_core().unwrap();
            if polled {
                assert!(tokio::time::timeout(
                    std::time::Duration::from_millis(20),
                    run.drive(&runtime)
                )
                .await
                .is_err());
            } else {
                let _unpolled = run.run();
            }
            assert_eq!(worker.progress(), CoreProgress::RecoveryRequired);
            assert!(worker.selected.lock().is_err());
            assert_eq!(worker.journal.read().unwrap().phase, phase);
            if phase == Phase::Prepared {
                assert_eq!(worker.journal.read().unwrap(), prepared);
            }
            assert!(worker.begin_core().is_err());
            assert_eq!(runtime.calls.get(), (usize::from(polled), 0));
        }
    }

    #[test]
    fn changed_receipt_cannot_start_and_failure_is_permanently_one_shot() {
        let (_f, mut worker) = owned();
        let prepared = worker.journal.read().unwrap();
        let failed = worker
            .journal
            .advance(prepared.revision, Phase::Failed)
            .unwrap();
        assert_eq!(
            worker.begin_core().err().unwrap().to_string(),
            "update_recovery_required"
        );
        assert!(worker.begin_core().is_err());
        assert_eq!(worker.progress(), CoreProgress::RecoveryRequired);
        assert_eq!(worker.journal.read().unwrap(), failed);
        assert!(worker.selected.lock().is_err());
    }

    #[tokio::test]
    async fn real_engine_missing_baseline_reports_recovery_without_publication() {
        let (f, mut worker) = owned();
        let prepared = worker.journal.read().unwrap();
        assert_eq!(worker.progress(), CoreProgress::RecoveryRequired);
        assert_eq!(
            worker.begin_core().unwrap().run().await,
            CoreProgress::RecoveryRequired
        );
        assert_eq!(worker.journal.read().unwrap(), prepared);
        assert!(worker.selected.lock().is_err());
        for (name, bytes) in [
            ("treer-agent-server", b"controller".as_slice()),
            ("treer", b"cli".as_slice()),
        ] {
            assert_eq!(std::fs::read(f.bin.join(name)).unwrap(), bytes);
        }
        assert!(worker.begin_core().is_err());
    }
}
