//! Forward Controller activation under transferred ownership. No Host restart,
//! launcher installation, startup-store write or terminal success is performed.
use std::time::Duration;

use anyhow::{bail, Result};
use tokio::time::{sleep, timeout};

use super::{
    batch::{restore_images, ImageBatch, PublicationResult},
    handoff::OwnedActivation,
    health::Monitor,
    host::Observation,
    safety::ControllerExpectation,
    selection::Selected,
    Journal, Phase, Receipt,
};

const HEALTH_BUDGET: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Only known Controller startup transitions may heal during this observation.
/// Host/workload/output/binding failures and new/unknown errors fail closed.
/// Compare fixed internal diagnostics only; never log or return arbitrary text.
fn retryable_observation(error: &anyhow::Error) -> bool {
    matches!(
        error.to_string().as_str(),
        "update_running_controller_unproven"
            | "update_controller_identity_changed"
            | "update_controller_identity_unavailable"
            | "update_health_unavailable"
            | "update_controller_health_mismatch"
    )
}

/// Neither outcome completes the update. CoreHealthy still needs launcher work;
/// ImagesRestored still needs old-Controller activation and independent health.
pub enum CoreActivation {
    CoreHealthy(Receipt),
    ImagesRestored(Receipt),
}

// Private seam: the production caller cannot substitute an ACK/HTTP-only check
// for the complete image, epoch, Host parent and workload observation.
trait Runtime {
    async fn restart_once(&self, receipt: &Receipt) -> Result<()>;
    async fn observe(&self, expected: &ControllerExpectation) -> Result<()>;
}

struct Live<'a> {
    selected: &'a Selected,
    owned: &'a OwnedActivation,
    journal: &'a Journal,
    host: Observation,
    monitor: Monitor,
}

impl Runtime for Live<'_> {
    async fn restart_once(&self, receipt: &Receipt) -> Result<()> {
        self.host
            .request_restart_once(
                self.selected,
                self.owned.locks(),
                self.journal,
                receipt.revision,
            )
            .await
    }

    async fn observe(&self, expected: &ControllerExpectation) -> Result<()> {
        self.monitor
            .observe_live(self.selected, self.owned.locks(), expected, &self.host)
            .await
            .map(|_| ())
    }
}

/// New execution only, never a crash-recovery/retry entry point. Reconnect to the
/// saved SAME Host baseline before publishing, without capturing a new baseline.
/// The caller retains OwnedActivation across this call and subsequent rollback
/// or launcher work. Dropping this future is not permission to start another job.
pub async fn activate_core(
    selected: &Selected,
    owned: &OwnedActivation,
    journal: &Journal,
) -> Result<CoreActivation> {
    // Refuse a consumed/changed job before opening another Host connection.
    if journal.read()? != *owned.prepared() {
        bail!("update_activation_binding_mismatch");
    }
    let monitor = Monitor::new()?;
    let (host, previous_epoch) = Observation::recover(selected, owned.locks(), journal).await?;
    let live = Live {
        selected,
        owned,
        journal,
        host,
        monitor,
    };
    drive(
        selected,
        owned,
        journal,
        &live,
        previous_epoch,
        HEALTH_BUDGET,
    )
    .await
}

async fn drive(
    selected: &Selected,
    owned: &OwnedActivation,
    journal: &Journal,
    runtime: &impl Runtime,
    previous_epoch: String,
    budget: Duration,
) -> Result<CoreActivation> {
    let published = match ImageBatch::begin(selected, owned, journal)?.publish()? {
        PublicationResult::Published(receipt) => receipt,
        PublicationResult::Restored(receipt) => {
            return Ok(CoreActivation::ImagesRestored(receipt));
        }
    };
    let expected = ControllerExpectation {
        workspace: published.workspace_id.clone(),
        server: published.server_id.clone(),
        previous_epoch,
        artifact_source_sha: published.artifact_source_sha.clone(),
        executable_sha256: published.controller_sha256.clone(),
    };
    // A lost/error ACK may follow acceptance. Never retry the mutation; observe
    // the actual result. ACK itself never advances the journal toward health.
    let _ = runtime.restart_once(&published).await;
    let healthy = timeout(budget, async {
        loop {
            if journal.read()? != published {
                bail!("update_activation_binding_mismatch");
            }
            match runtime.observe(&expected).await {
                Ok(()) => {
                    if journal.read()? != published {
                        bail!("update_activation_binding_mismatch");
                    }
                    return Ok::<(), anyhow::Error>(());
                }
                Err(error) if retryable_observation(&error) => {}
                Err(_) => bail!("update_activation_observation_failed"),
            }
            sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    if matches!(healthy, Ok(Ok(()))) {
        return journal
            .advance(published.revision, Phase::CoreHealthy)
            .map(CoreActivation::CoreHealthy);
    }
    // Includes ACK followed by failed health, lost ACK without proven health,
    // and a stalled observation. Only read-only observations are cancelled.
    restore_images(selected, owned, journal).map(CoreActivation::ImagesRestored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{artifacts::Binary, batch::tests::fixture};
    use std::cell::{Cell, RefCell};

    struct Fake {
        ack: bool,
        healthy: bool,
        pending: bool,
        failure: &'static str,
        failures_before_healthy: usize,
        restarts: Cell<usize>,
        observations: Cell<usize>,
        expectation: RefCell<Option<ControllerExpectation>>,
    }

    impl Runtime for Fake {
        async fn restart_once(&self, receipt: &Receipt) -> Result<()> {
            assert_eq!(receipt.phase, Phase::Activating);
            self.restarts.set(self.restarts.get() + 1);
            if self.ack {
                Ok(())
            } else {
                bail!("test_lost_ack")
            }
        }

        async fn observe(&self, expected: &ControllerExpectation) -> Result<()> {
            self.observations.set(self.observations.get() + 1);
            self.expectation.replace(Some(ControllerExpectation {
                workspace: expected.workspace.clone(),
                server: expected.server.clone(),
                previous_epoch: expected.previous_epoch.clone(),
                artifact_source_sha: expected.artifact_source_sha.clone(),
                executable_sha256: expected.executable_sha256.clone(),
            }));
            if self.pending {
                std::future::pending::<()>().await;
            }
            if self.healthy && self.observations.get() > self.failures_before_healthy {
                Ok(())
            } else {
                bail!(self.failure)
            }
        }
    }

    #[tokio::test]
    async fn preflight_failure_never_requests_restart_or_observes_health() {
        let (f, selected, owned, journal) = fixture();
        let prepared = journal.read().unwrap();
        std::fs::write(
            f.bin
                .join(format!(".treer-update-{}-treer-install", prepared.job_id)),
            b"corrupted stage",
        )
        .unwrap();
        let runtime = Fake {
            ack: true,
            healthy: true,
            pending: false,
            failure: "update_health_unavailable",
            failures_before_healthy: 0,
            restarts: Cell::new(0),
            observations: Cell::new(0),
            expectation: RefCell::new(None),
        };
        assert!(drive(
            &selected,
            &owned,
            &journal,
            &runtime,
            "old-observed-epoch".into(),
            Duration::from_millis(20),
        )
        .await
        .is_err());
        assert_eq!(journal.read().unwrap(), prepared);
        assert_eq!(runtime.restarts.get(), 0);
        assert_eq!(runtime.observations.get(), 0);
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"controller"
        );
        assert!(selected.lock().is_err());
    }

    #[tokio::test]
    async fn consumed_job_is_rejected_before_any_host_connection() {
        let (_f, selected, owned, journal) = fixture();
        let _batch = ImageBatch::begin(&selected, &owned, &journal).unwrap();
        assert_eq!(
            activate_core(&selected, &owned, &journal)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_activation_binding_mismatch"
        );
        assert_eq!(journal.read().unwrap().phase, Phase::Activating);
    }

    #[tokio::test]
    async fn health_not_ack_decides_forward_activation_and_failed_health_restores() {
        for (ack, healthy, pending) in [
            (true, true, false),
            (false, true, false),
            (true, false, false),
            (false, false, false),
            (true, true, true),
        ] {
            let (f, selected, owned, journal) = fixture();
            let runtime = Fake {
                ack,
                healthy,
                pending,
                failure: "update_health_unavailable",
                failures_before_healthy: 0,
                restarts: Cell::new(0),
                observations: Cell::new(0),
                expectation: RefCell::new(None),
            };
            let result = drive(
                &selected,
                &owned,
                &journal,
                &runtime,
                "old-observed-epoch".into(),
                Duration::from_millis(20),
            )
            .await
            .unwrap();
            let receipt = journal.read().unwrap();
            if healthy && !pending {
                assert!(matches!(result, CoreActivation::CoreHealthy(_)));
                assert_eq!(receipt.phase, Phase::CoreHealthy);
                assert_eq!(
                    std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
                    b"new-controller"
                );
            } else {
                assert!(matches!(result, CoreActivation::ImagesRestored(_)));
                assert_eq!(receipt.phase, Phase::RollingBack);
                assert_eq!(
                    std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
                    b"controller"
                );
                assert_eq!(
                    std::fs::read(f.bin.join(Binary::Cli.name())).unwrap(),
                    b"cli"
                );
            }
            assert_eq!(runtime.restarts.get(), 1);
            assert_eq!(runtime.observations.get(), 1);
            {
                let expectation = runtime.expectation.borrow();
                let expectation = expectation.as_ref().unwrap();
                assert_eq!(expectation.previous_epoch, "old-observed-epoch");
                assert_eq!(expectation.artifact_source_sha, receipt.artifact_source_sha);
                assert_eq!(expectation.executable_sha256, receipt.controller_sha256);
                assert_eq!(expectation.workspace, selected.workspace());
                assert_eq!(expectation.server, selected.server_id());
            }
            assert!(selected.lock().is_err());
            assert!(drive(
                &selected,
                &owned,
                &journal,
                &runtime,
                "old-observed-epoch".into(),
                Duration::from_millis(20),
            )
            .await
            .is_err());
            assert_eq!(runtime.restarts.get(), 1);
        }
    }

    #[test]
    fn only_known_startup_errors_are_retryable() {
        for code in [
            "update_running_controller_unproven",
            "update_controller_identity_changed",
            "update_controller_identity_unavailable",
            "update_health_unavailable",
            "update_controller_health_mismatch",
        ] {
            assert!(retryable_observation(&anyhow::anyhow!(code)), "{code}");
        }
        for code in [
            "update_host_changed",
            "update_host_identity_changed",
            "update_host_disconnected",
            "update_workload_changed",
            "update_output_continuity_unproven",
            "update_health_binding_mismatch",
            "update_controller_identity_binding_mismatch",
            "update_enrollment_changed",
            "update_controller_image_mismatch",
            "update_multiple_running_controllers",
            "update_health_unsupported",
            "update_health_too_large",
            "unknown failure with private text",
        ] {
            assert!(!retryable_observation(&anyhow::anyhow!(code)), "{code}");
        }
    }

    #[tokio::test]
    async fn permanent_observation_failure_restores_without_waiting_for_health_budget() {
        for failure in [
            "update_host_changed",
            "update_workload_changed",
            "update_output_continuity_unproven",
            "unknown_observation_failure",
        ] {
            let (f, selected, owned, journal) = fixture();
            let runtime = Fake {
                ack: true,
                healthy: false,
                pending: false,
                failure,
                failures_before_healthy: 0,
                restarts: Cell::new(0),
                observations: Cell::new(0),
                expectation: RefCell::new(None),
            };
            let result = timeout(
                Duration::from_secs(2),
                drive(
                    &selected,
                    &owned,
                    &journal,
                    &runtime,
                    "old-observed-epoch".into(),
                    HEALTH_BUDGET,
                ),
            )
            .await
            .expect("permanent failure must not wait out the 30s health budget")
            .unwrap();
            assert!(matches!(result, CoreActivation::ImagesRestored(_)));
            assert_eq!(journal.read().unwrap().phase, Phase::RollingBack);
            assert_eq!(runtime.restarts.get(), 1);
            assert_eq!(runtime.observations.get(), 1);
            assert_eq!(
                std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
                b"controller"
            );
            assert_eq!(
                std::fs::read(f.bin.join(Binary::Cli.name())).unwrap(),
                b"cli"
            );
            assert!(selected.lock().is_err());
        }
    }

    #[tokio::test]
    async fn startup_transition_retries_observation_without_resending_restart() {
        let (_f, selected, owned, journal) = fixture();
        let runtime = Fake {
            ack: true,
            healthy: true,
            pending: false,
            failure: "update_running_controller_unproven",
            failures_before_healthy: 1,
            restarts: Cell::new(0),
            observations: Cell::new(0),
            expectation: RefCell::new(None),
        };
        let result = drive(
            &selected,
            &owned,
            &journal,
            &runtime,
            "old-observed-epoch".into(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(matches!(result, CoreActivation::CoreHealthy(_)));
        assert_eq!(runtime.restarts.get(), 1);
        assert_eq!(runtime.observations.get(), 2);
        assert!(selected.lock().is_err());
    }
}
