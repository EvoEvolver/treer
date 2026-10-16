//! Image publication under the worker's transferred locks. No process start,
//! Host restart, health claim, startup-store restore or launcher work occurs.
use anyhow::{bail, Result};

use super::{
    artifacts::Binary, handoff::OwnedActivation, images, selection::Selected, Journal, Phase,
    Receipt,
};

/// Both outcomes require subsequent Controller/Host/workload health checks.
/// Restored images remain RollingBack, never prematurely RolledBack.
pub enum PublicationResult {
    Published(Receipt),
    Restored(Receipt),
}

pub struct ImageBatch<'a> {
    selected: &'a Selected,
    owned: &'a OwnedActivation,
    journal: &'a Journal,
    activating: Receipt,
}

fn same_job(receipt: &Receipt, prepared: &Receipt) -> bool {
    let mut expected = prepared.clone();
    expected.phase = receipt.phase;
    expected.revision = receipt.revision;
    expected.updated_at = receipt.updated_at;
    expected.publications = receipt.publications.clone();
    receipt == &expected
        && receipt.revision >= prepared.revision
        && receipt.updated_at >= prepared.updated_at
        && [Binary::Controller, Binary::Cli].into_iter().all(|binary| {
            receipt.publications.original_digest(binary)
                == prepared.publications.original_digest(binary)
        })
}

impl<'a> ImageBatch<'a> {
    /// Both images must preflight before recording Activating or renaming
    /// either. Only a matching, successfully handed-off Prepared job may begin.
    pub fn begin(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
    ) -> Result<Self> {
        let prepared = journal.read()?;
        if &prepared != owned.prepared()
            || prepared.phase != Phase::Prepared
            || prepared.workspace_id != selected.workspace()
            || prepared.server_id != selected.server_id()
        {
            bail!("update_batch_binding_mismatch");
        }
        for binary in [Binary::Controller, Binary::Cli] {
            images::preflight(
                owned.locks().directory(selected, binary)?,
                journal,
                prepared.revision,
                binary,
            )?;
        }
        if journal.read()? != prepared {
            bail!("update_batch_binding_mismatch");
        }
        let activating = journal.advance(prepared.revision, Phase::Activating)?;
        Ok(Self {
            selected,
            owned,
            journal,
            activating,
        })
    }

    /// Consumed once, with durable per-file intent/completion. A publication
    /// failure immediately attempts verified image restoration in reverse order.
    /// The caller retains OwnedActivation throughout and must not resubmit a job
    /// on error. This is not the explicit post-crash recovery entry point.
    pub fn publish(self) -> Result<PublicationResult> {
        if self.journal.read()? != self.activating {
            bail!("update_batch_binding_mismatch");
        }
        let result = (|| -> Result<Receipt> {
            let mut receipt = self.activating;
            for binary in [Binary::Controller, Binary::Cli] {
                receipt = images::install(
                    self.owned.locks().directory(self.selected, binary)?,
                    self.journal,
                    receipt.revision,
                    binary,
                )?;
            }
            Ok(receipt)
        })();
        match result {
            Ok(receipt) => Ok(PublicationResult::Published(receipt)),
            Err(_) => restore_images(self.selected, self.owned, self.journal)
                .map(PublicationResult::Restored),
        }
    }
}

/// Restore only this owned job's verified backups. Never mark RolledBack here:
/// the old Controller must still be activated and independently proven healthy.
pub fn restore_images(
    selected: &Selected,
    owned: &OwnedActivation,
    journal: &Journal,
) -> Result<Receipt> {
    let mut receipt = journal.read()?;
    if !same_job(&receipt, owned.prepared())
        || receipt.workspace_id != selected.workspace()
        || receipt.server_id != selected.server_id()
    {
        bail!("update_batch_binding_mismatch");
    }
    if matches!(receipt.phase, Phase::Activating | Phase::CoreHealthy) {
        receipt = journal.advance(receipt.revision, Phase::RollingBack)?;
    } else if receipt.phase != Phase::RollingBack {
        bail!("update_revision_or_phase_conflict");
    }
    let result = (|| -> Result<Receipt> {
        for binary in [Binary::Cli, Binary::Controller] {
            receipt = images::restore(
                owned.locks().directory(selected, binary)?,
                journal,
                receipt.revision,
                binary,
            )?;
        }
        Ok(receipt)
    })();
    if result.is_err() {
        let current = journal.read()?;
        if current.phase == Phase::RollingBack {
            journal.advance(current.revision, Phase::RollbackFailed)?;
        }
        bail!("update_images_rollback_failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::{FileStep, Publications},
        selection::tests::Fixture,
        tests::receipt,
    };
    use sha2::{Digest, Sha256};
    use std::{io::Write, os::unix::fs::PermissionsExt};
    use treer_platform::private_directory::PrivateDirectory;

    fn fixture() -> (Fixture, Selected, OwnedActivation, Journal) {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        for binary in [Binary::Controller, Binary::Cli] {
            std::fs::set_permissions(
                f.bin.join(binary.name()),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let mut receipt = receipt();
        receipt.workspace_id = "ws_a".into();
        receipt.server_id = "srv_a".into();
        receipt.controller_sha256 = format!("{:x}", Sha256::digest(b"new-controller"));
        receipt.cli_sha256 = format!("{:x}", Sha256::digest(b"new-cli"));
        receipt.publications = Publications::new(
            format!("{:x}", Sha256::digest(b"controller")),
            format!("{:x}", Sha256::digest(b"cli")),
            &receipt.controller_sha256,
            &receipt.cli_sha256,
        );
        let journal = Journal::create(&jobs, &receipt).unwrap();
        for (binary, name, bytes) in [
            (
                Binary::Controller,
                "new-controller",
                b"new-controller".as_slice(),
            ),
            (Binary::Cli, "new-cli", b"new-cli".as_slice()),
        ] {
            let mut source = journal.directory.create_file(name).unwrap();
            source.write_all(bytes).unwrap();
            drop(source);
            images::prepare(
                locks.directory(&selected, binary).unwrap(),
                &journal,
                0,
                binary,
                &journal.directory.read_file(name).unwrap(),
            )
            .unwrap();
        }
        let prepared = journal.prepare_fixture().unwrap();
        (
            f,
            selected,
            OwnedActivation::fixture(locks, prepared),
            journal,
        )
    }

    #[test]
    fn publish_then_restore_retains_locks_and_never_claims_health() {
        let (f, selected, owned, journal) = fixture();
        let outcome = ImageBatch::begin(&selected, &owned, &journal)
            .unwrap()
            .publish()
            .unwrap();
        let PublicationResult::Published(receipt) = outcome else {
            panic!("unexpected restoration")
        };
        assert_eq!(receipt.phase, Phase::Activating);
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"new-controller"
        );
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Cli.name())).unwrap(),
            b"new-cli"
        );
        assert!(selected.lock().is_err());
        assert!(ImageBatch::begin(&selected, &owned, &journal).is_err());
        let receipt = restore_images(&selected, &owned, &journal).unwrap();
        assert_eq!(receipt.phase, Phase::RollingBack);
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

    fn corrupt_cli_stage(f: &Fixture, journal: &Journal) {
        let id = journal.read().unwrap().job_id;
        std::fs::write(
            f.bin.join(format!(".treer-update-{id}-treer-install")),
            b"changed",
        )
        .unwrap();
    }

    fn reopen(f: &Fixture, journal: &Journal) -> Journal {
        let jobs = PrivateDirectory::open(&f.path.join("jobs")).unwrap();
        Journal::open(&jobs, &journal.read().unwrap().job_id.to_string()).unwrap()
    }

    fn assert_old_images(f: &Fixture) {
        for (binary, bytes) in [
            (Binary::Controller, b"controller".as_slice()),
            (Binary::Cli, b"cli".as_slice()),
        ] {
            let path = f.bin.join(binary.name());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
    }

    #[test]
    fn interrupted_publication_restores_from_every_durable_boundary_without_replay() {
        // Exact durable prefixes, not process-kill acceptance: Prepared ownership
        // and its locks remain held while only the journal handle is reopened.
        // Seven boundaries: Activating, then intent/rename/completion per image.
        for boundary in 0..=6 {
            let (f, selected, owned, journal) = fixture();
            let _batch = ImageBatch::begin(&selected, &owned, &journal).unwrap();
            let mut receipt = journal.read().unwrap();
            let mut operation = 0;
            for binary in [Binary::Controller, Binary::Cli] {
                if operation == boundary {
                    break;
                }
                receipt = journal
                    .record_file(receipt.revision, binary, FileStep::InstallIntent)
                    .unwrap();
                operation += 1;
                if operation == boundary {
                    break;
                }
                let staged = format!(".treer-update-{}-{}-install", receipt.job_id, binary.name());
                owned
                    .locks()
                    .directory(&selected, binary)
                    .unwrap()
                    .replace_executable(&staged, binary.name())
                    .unwrap();
                operation += 1;
                if operation == boundary {
                    break;
                }
                receipt = journal
                    .record_file(receipt.revision, binary, FileStep::Installed)
                    .unwrap();
                operation += 1;
            }
            assert_eq!(operation, boundary);
            let journal = reopen(&f, &journal);
            assert!(ImageBatch::begin(&selected, &owned, &journal).is_err());
            // Restoration must not consume a still-unpublished new image.
            let remaining_stages: Vec<_> = [Binary::Controller, Binary::Cli]
                .into_iter()
                .map(|binary| {
                    f.bin.join(format!(
                        ".treer-update-{}-{}-install",
                        receipt.job_id,
                        binary.name()
                    ))
                })
                .filter(|path| path.exists())
                .collect();
            let restored = restore_images(&selected, &owned, &journal).unwrap();
            assert_eq!(restored.phase, Phase::RollingBack);
            assert_old_images(&f);
            assert!(remaining_stages.iter().all(|path| path.exists()));
            assert!(selected.lock().is_err());
            // Reconciliation is idempotent, including revision and timestamp.
            assert_eq!(
                restore_images(&selected, &owned, &journal).unwrap(),
                restored
            );
        }
    }

    #[test]
    fn interrupted_reverse_restoration_reconciles_both_sides_of_each_rename() {
        for binary in [Binary::Cli, Binary::Controller] {
            for renamed in [false, true] {
                let (f, selected, owned, journal) = fixture();
                assert!(matches!(
                    ImageBatch::begin(&selected, &owned, &journal)
                        .unwrap()
                        .publish()
                        .unwrap(),
                    PublicationResult::Published(_)
                ));
                let receipt = journal.read().unwrap();
                let mut receipt = journal
                    .advance(receipt.revision, Phase::RollingBack)
                    .unwrap();
                if matches!(binary, Binary::Controller) {
                    receipt = images::restore(
                        owned.locks().directory(&selected, Binary::Cli).unwrap(),
                        &journal,
                        receipt.revision,
                        Binary::Cli,
                    )
                    .unwrap();
                }
                journal
                    .record_file(receipt.revision, binary, FileStep::RestoreIntent)
                    .unwrap();
                if renamed {
                    // Native replacement before the completion record, with the
                    // original permissions, as in images::restore.
                    let installed = owned.locks().directory(&selected, binary).unwrap();
                    let staged =
                        format!(".treer-update-{}-{}-restore", receipt.job_id, binary.name());
                    let mut file = installed.create_file(&staged).unwrap();
                    let bytes: &[u8] = match binary {
                        Binary::Controller => b"controller",
                        Binary::Cli => b"cli",
                    };
                    file.write_all(bytes).unwrap();
                    file.set_permissions(std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    file.sync_all().unwrap();
                    drop(file);
                    installed
                        .replace_executable(&staged, binary.name())
                        .unwrap();
                }
                let journal = reopen(&f, &journal);
                let restored = restore_images(&selected, &owned, &journal).unwrap();
                assert_eq!(restored.phase, Phase::RollingBack);
                assert_old_images(&f);
                for binary in [Binary::Controller, Binary::Cli] {
                    assert_eq!(restored.publications.step(binary), FileStep::Restored);
                }
                assert!(selected.lock().is_err());
                assert_eq!(
                    restore_images(&selected, &owned, &journal).unwrap(),
                    restored
                );
            }
        }
    }

    #[test]
    fn both_images_preflight_before_first_publication() {
        let (f, selected, owned, journal) = fixture();
        let prepared = journal.read().unwrap();
        corrupt_cli_stage(&f, &journal);
        assert!(ImageBatch::begin(&selected, &owned, &journal).is_err());
        assert_eq!(journal.read().unwrap(), prepared);
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"controller"
        );
    }

    #[test]
    fn preflight_rejects_changed_original_mode_without_mutation() {
        let (f, selected, owned, journal) = fixture();
        let prepared = journal.read().unwrap();
        std::fs::set_permissions(
            f.bin.join(Binary::Cli.name()),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(ImageBatch::begin(&selected, &owned, &journal).is_err());
        assert_eq!(journal.read().unwrap(), prepared);
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"controller"
        );
    }

    #[test]
    fn rollback_binding_keeps_original_targets_source_and_creation_immutable() {
        let (_fixture, _selected, owned, _journal) = fixture();
        let prepared = owned.prepared();
        let mut later = prepared.clone();
        later.phase = Phase::RollingBack;
        later.revision += 1;
        assert!(same_job(&later, prepared));
        later.controller_sha256 = "f".repeat(64);
        assert!(!same_job(&later, prepared));
        later = prepared.clone();
        later.created_at += chrono::Duration::seconds(1);
        assert!(!same_job(&later, prepared));
        later = prepared.clone();
        later.source = super::super::Source::Pin {
            sums_sha256: "f".repeat(64),
            allow_pinned: true,
            allow_artifact_origin: false,
            allow_same: true,
            allow_downgrade: false,
        };
        assert!(!same_job(&later, prepared));
    }

    #[test]
    fn second_image_failure_restores_first_without_restarting_anything() {
        let (f, selected, owned, journal) = fixture();
        let batch = ImageBatch::begin(&selected, &owned, &journal).unwrap();
        corrupt_cli_stage(&f, &journal);
        let PublicationResult::Restored(receipt) = batch.publish().unwrap() else {
            panic!("unexpected publication")
        };
        assert_eq!(receipt.phase, Phase::RollingBack);
        assert_eq!(
            receipt.publications.step(Binary::Controller),
            FileStep::Restored
        );
        assert_eq!(receipt.publications.step(Binary::Cli), FileStep::Original);
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"controller"
        );
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Cli.name())).unwrap(),
            b"cli"
        );
    }

    #[test]
    fn corrupt_backup_retains_failed_rollback_evidence() {
        let (f, selected, owned, journal) = fixture();
        let batch = ImageBatch::begin(&selected, &owned, &journal).unwrap();
        let backup = f
            .path
            .join("jobs")
            .join(journal.read().unwrap().job_id.to_string())
            .join("backup-cli");
        std::fs::write(&backup, b"unrecognized backup").unwrap();
        assert_eq!(
            batch.publish().err().unwrap().to_string(),
            "update_images_rollback_failed"
        );
        let receipt = journal.read().unwrap();
        assert_eq!(receipt.phase, Phase::RollbackFailed);
        assert_eq!(
            receipt.publications.step(Binary::Controller),
            FileStep::Installed
        );
        assert_eq!(
            std::fs::read(f.bin.join(Binary::Controller.name())).unwrap(),
            b"new-controller"
        );
        assert_eq!(std::fs::read(backup).unwrap(), b"unrecognized backup");
        assert!(selected.lock().is_err());
        assert!(ImageBatch::begin(&selected, &owned, &journal).is_err());
    }
}
