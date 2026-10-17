//! Read-only installed-image preflight for the future launcher executor.
//! No installer execution, journal transition or reusable execution authority.
use anyhow::{bail, Result};
use std::time::Duration;

use super::{
    artifacts::Binary, batch::same_job, handoff::OwnedActivation, probe, publication::FileStep,
    selection::Selected, verified_image::VerifiedImage, Journal, Phase, Receipt,
};

const BUDGET: Duration = Duration::from_secs(20);

fn checked(selected: &Selected, owned: &OwnedActivation, journal: &Journal) -> Result<Receipt> {
    let receipt = journal
        .read()
        .map_err(|_| anyhow::anyhow!("update_launcher_binding_mismatch"))?;
    if receipt.phase != Phase::CoreHealthy
        || !same_job(&receipt, owned.prepared())
        || receipt.workspace_id != selected.workspace()
        || receipt.server_id != selected.server_id()
        || [Binary::Controller, Binary::Cli].into_iter().any(|binary| {
            !matches!(
                receipt.publications.step(binary),
                FileStep::Installed | FileStep::Unchanged
            )
        })
    {
        bail!("update_launcher_binding_mismatch");
    }
    Ok(receipt)
}

/// Only probes fixed --version commands under the original transferred locks.
/// Both digests and the expected native source come from the SAME owned job,
/// never from argv, ambient environment, PATH, or the image's own output.
/// Success is a point-in-time preflight, NOT permission to spawn an installer:
/// that executor must revalidate at spawn/completion, persist child-start/identity
/// evidence and keep ownership through strict quiescence and final live health.
pub async fn check(selected: &Selected, owned: &OwnedActivation, journal: &Journal) -> Result<()> {
    let deadline = tokio::time::Instant::now() + BUDGET;
    check_until(selected, owned, journal, deadline).await
}

async fn check_until(
    selected: &Selected,
    owned: &OwnedActivation,
    journal: &Journal,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let receipt = probe::within_budget(deadline, || checked(selected, owned, journal))?;
    let (controller_directory, cli_directory) = probe::within_budget(deadline, || {
        Ok((
            owned.locks().directory(selected, Binary::Controller)?,
            owned.locks().directory(selected, Binary::Cli)?,
        ))
    })?;
    let cli_path = selected.cli_path();
    // Authenticate BOTH before executing either. Retain these exact inodes
    // across both probes, so the CLI cannot replace the already-probed Controller
    // (even with identical bytes) and still produce a successful preflight.
    let controller_image = probe::within_budget(deadline, || {
        VerifiedImage::open(
            controller_directory,
            selected.controller_path(),
            &receipt.controller_sha256,
        )
    })?;
    let cli_image = probe::within_budget(deadline, || {
        VerifiedImage::open(cli_directory, &cli_path, &receipt.cli_sha256)
    })?;
    let controller = probe::version_until(
        controller_directory,
        selected.controller_path(),
        Binary::Controller,
        &receipt.controller_sha256,
        deadline,
    )
    .await?;
    let cli = probe::version_until(
        cli_directory,
        &cli_path,
        Binary::Cli,
        &receipt.cli_sha256,
        deadline,
    )
    .await?;
    probe::within_budget(deadline, || {
        controller_image.verify()?;
        cli_image.verify()?;
        probe::consistent(&controller, &cli)?;
        if controller.source_sha != receipt.artifact_source_sha {
            bail!("update_launcher_source_mismatch");
        }
        if checked(selected, owned, journal)? != receipt {
            bail!("update_launcher_binding_mismatch");
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::Publications, selection::tests::Fixture, tests::receipt,
    };
    use sha2::{Digest, Sha256};
    use std::{fs, os::unix::fs::PermissionsExt};
    use treer_platform::private_directory::PrivateDirectory;

    fn fixture(
        cli_version: &str,
        cli_source: &str,
        cli_extra: impl FnOnce(&Fixture) -> String,
    ) -> (Fixture, Selected, OwnedActivation, Journal) {
        fixture_with_source(cli_version, cli_source, &"b".repeat(40), cli_extra)
    }

    fn fixture_with_source(
        cli_version: &str,
        cli_source: &str,
        receipt_source: &str,
        cli_extra: impl FnOnce(&Fixture) -> String,
    ) -> (Fixture, Selected, OwnedActivation, Journal) {
        let f = Fixture::new();
        let source = "b".repeat(40);
        let controller = format!(
            "#!/bin/sh\nprintf ran > controller-ran\nprintf 'treer-agent-server 1.2.3 ({source})\\n'\n"
        );
        let cli = format!(
            "#!/bin/sh\n{}\nprintf ran > cli-ran\nprintf 'treer {cli_version} ({cli_source})\\n'\n",
            cli_extra(&f)
        );
        for (binary, script) in [(Binary::Controller, &controller), (Binary::Cli, &cli)] {
            fs::write(f.bin.join(binary.name()), script).unwrap();
            fs::set_permissions(f.bin.join(binary.name()), fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let mut receipt = receipt();
        receipt.workspace_id = "ws_a".into();
        receipt.server_id = "srv_a".into();
        receipt.artifact_source_sha = receipt_source.into();
        receipt.controller_sha256 = format!("{:x}", Sha256::digest(controller.as_bytes()));
        receipt.cli_sha256 = format!("{:x}", Sha256::digest(cli.as_bytes()));
        // Exercise the valid same-image update case without inventing a live
        // Host/health result. This is an isolated preflight fixture only.
        receipt.publications = Publications::new(
            receipt.controller_sha256.clone(),
            receipt.cli_sha256.clone(),
            &receipt.controller_sha256,
            &receipt.cli_sha256,
        );
        let journal = Journal::create(&jobs, &receipt).unwrap();
        let prepared = journal.prepare_fixture().unwrap();
        let activating = journal
            .advance(prepared.revision, Phase::Activating)
            .unwrap();
        journal
            .advance(activating.revision, Phase::CoreHealthy)
            .unwrap();
        (
            f,
            selected,
            OwnedActivation::fixture(locks, prepared),
            journal,
        )
    }

    #[tokio::test]
    async fn expired_preflight_budget_does_not_execute_either_image() {
        let (f, selected, owned, journal) = fixture("1.2.3", &"b".repeat(40), |_| String::new());
        assert_eq!(
            check_until(&selected, &owned, &journal, tokio::time::Instant::now())
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_image_probe_timeout"
        );
        assert!(!f.bin.join("controller-ran").exists());
        assert!(!f.bin.join("cli-ran").exists());
    }

    #[tokio::test]
    async fn both_images_bind_native_source_not_wrapper_and_preserve_receipt_and_locks() {
        let (f, selected, owned, journal) = fixture("1.2.3", &"b".repeat(40), |_| String::new());
        let before = journal.read().unwrap();
        assert_ne!(before.release_sha, before.artifact_source_sha);
        check(&selected, &owned, &journal).await.unwrap();
        assert!(f.bin.join("controller-ran").exists());
        assert!(f.bin.join("cli-ran").exists());
        assert_eq!(journal.read().unwrap(), before);
        assert!(selected.lock().is_err());
    }

    #[tokio::test]
    async fn bad_cli_digest_rejects_before_either_probe_runs() {
        let (f, selected, owned, journal) = fixture("1.2.3", &"b".repeat(40), |_| String::new());
        fs::write(f.bin.join(Binary::Cli.name()), b"changed").unwrap();
        assert!(check(&selected, &owned, &journal).await.is_err());
        assert!(!f.bin.join("controller-ran").exists());
        assert!(!f.bin.join("cli-ran").exists());
        assert!(selected.lock().is_err());
    }

    #[tokio::test]
    async fn matching_digests_cannot_bless_wrong_native_source_or_version() {
        for (version, source) in [("1.2.3", "a".repeat(40)), ("1.2.4", "b".repeat(40))] {
            let (_f, selected, owned, journal) = fixture(version, &source, |_| String::new());
            assert!(check(&selected, &owned, &journal).await.is_err());
            assert_eq!(journal.read().unwrap().phase, Phase::CoreHealthy);
        }
        // Even two mutually consistent images must match authenticated source
        // metadata, not merely agree with each other or the wrapper release SHA.
        let (_f, selected, owned, journal) =
            fixture_with_source("1.2.3", &"b".repeat(40), &"a".repeat(40), |_| String::new());
        assert_eq!(
            check(&selected, &owned, &journal)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_launcher_source_mismatch"
        );
    }

    #[tokio::test]
    async fn changed_phase_or_foreign_owned_job_rejects_without_execution() {
        for foreign in [false, true] {
            let (f, selected, owned, journal) =
                fixture("1.2.3", &"b".repeat(40), |_| String::new());
            if foreign {
                // A different otherwise-valid CoreHealthy job is not covered
                // by the sealed ownership of this job, even at the same paths.
                let jobs = PrivateDirectory::open(&f.path.join("jobs")).unwrap();
                let mut other = owned.prepared().clone();
                other.job_id = uuid::Uuid::new_v4();
                other.revision = 0;
                other.phase = Phase::Preparing;
                let other_journal = Journal::create(&jobs, &other).unwrap();
                let prepared = other_journal.prepare_fixture().unwrap();
                let activating = other_journal
                    .advance(prepared.revision, Phase::Activating)
                    .unwrap();
                other_journal
                    .advance(activating.revision, Phase::CoreHealthy)
                    .unwrap();
                assert!(check(&selected, &owned, &other_journal).await.is_err());
            } else {
                let current = journal.read().unwrap();
                journal.advance(current.revision, Phase::Partial).unwrap();
                assert!(check(&selected, &owned, &journal).await.is_err());
            }
            assert!(!f.bin.join("controller-ran").exists());
            assert!(!f.bin.join("cli-ran").exists());
            assert!(selected.lock().is_err());
            drop(owned);
        }
    }

    #[tokio::test]
    async fn cli_cannot_hide_changed_journal_behind_valid_version_output() {
        let (f, selected, owned, journal) = fixture("1.2.3", &"b".repeat(40), |_| {
            "for receipt in ../jobs/*/receipt.json; do printf invalid > \"$receipt\"; done".into()
        });
        assert_eq!(
            check(&selected, &owned, &journal)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_launcher_binding_mismatch"
        );
        assert!(f.bin.join("controller-ran").exists());
        assert!(f.bin.join("cli-ran").exists());
        assert!(selected.lock().is_err());
    }

    #[tokio::test]
    async fn cli_cannot_replace_the_previously_probed_controller_with_identical_bytes() {
        let (f, selected, owned, journal) = fixture("1.2.3", &"b".repeat(40), |_| {
            "/bin/cp treer-agent-server replacement\n/bin/chmod 700 replacement\n/bin/mv replacement treer-agent-server".into()
        });
        let result = check(&selected, &owned, &journal).await;
        assert_eq!(result.err().unwrap().to_string(), "update_image_conflict");
        assert!(f.bin.join("controller-ran").exists());
        assert!(f.bin.join("cli-ran").exists());
        assert_eq!(journal.read().unwrap().phase, Phase::CoreHealthy);
        assert!(selected.lock().is_err());
    }
}
