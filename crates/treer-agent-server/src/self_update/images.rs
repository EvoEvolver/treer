//! Unix image backup/publication under the installation and enrollment locks.
//! This layer never executes bytes or restarts a process. Authenticated receipt
//! digests and checked parent handles are prerequisites, not CLI-supplied paths.
use std::{
    fs::File,
    io::Write,
    os::unix::fs::{FileExt, PermissionsExt},
};

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use treer_platform::installation_directory::InstallationDirectory;

use super::{
    artifacts::Binary,
    publication::{FileStep, Reconcile},
    Journal, Phase, Receipt,
};

const LIMIT: u64 = 128 * 1024 * 1024;

fn target(receipt: &Receipt, binary: Binary) -> &str {
    match binary {
        Binary::Controller => &receipt.controller_sha256,
        Binary::Cli => &receipt.cli_sha256,
    }
}

fn backup(binary: Binary) -> &'static str {
    match binary {
        Binary::Controller => "backup-controller",
        Binary::Cli => "backup-cli",
    }
}

fn backup_mode(journal: &Journal, binary: Binary) -> Result<u32> {
    let file = journal
        .directory
        .read_file(&format!("{}.mode", backup(binary)))?;
    if file.metadata()?.len() != 4 {
        bail!("invalid_update_backup_mode");
    }
    let mut bytes = [0; 4];
    file.read_exact_at(&mut bytes, 0)?;
    let mode = u32::from_be_bytes(bytes);
    validate_mode(mode)?;
    Ok(mode)
}

fn validate_mode(mode: u32) -> Result<()> {
    if mode & !0o755 != 0 || mode & 0o500 != 0o500 {
        bail!("invalid_update_backup_mode");
    }
    Ok(())
}

fn stage(receipt: &Receipt, binary: Binary, restoring: bool) -> String {
    format!(
        ".treer-update-{}-{}-{}",
        receipt.job_id,
        binary.name(),
        if restoring { "restore" } else { "install" }
    )
}

fn current(journal: &Journal, revision: u64) -> Result<Receipt> {
    let receipt = journal.read()?;
    if receipt.revision != revision {
        bail!("update_revision_or_phase_conflict");
    }
    Ok(receipt)
}

/// Offset reads avoid sharing seek position with a retained verified reader.
/// Bound and hash the bytes actually copied, not just a preceding stat().
fn copy(source: &File, mut destination: Option<&mut File>) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut offset = 0_u64;
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let length = source.read_at(&mut bytes, offset)?;
        if length == 0 {
            break;
        }
        offset += length as u64;
        if offset > LIMIT {
            bail!("update_image_too_large");
        }
        hasher.update(&bytes[..length]);
        if let Some(file) = destination.as_mut() {
            file.write_all(&bytes[..length])?;
        }
    }
    if offset == 0 {
        bail!("invalid_update_image");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn matches(source: &File, expected: &str) -> Result<()> {
    if copy(source, None)? != expected {
        bail!("update_image_conflict");
    }
    Ok(())
}

pub(super) fn digest_file(source: &File) -> Result<String> {
    copy(source, None)
}

/// Create a registered sibling stage and close its writable handle before any
/// execution/publication. A failed attempt removes only its newly created name.
fn create_stage(
    installed: &InstallationDirectory,
    name: &str,
    source: &File,
    expected: &str,
    mode: u32,
) -> Result<()> {
    validate_mode(mode)?;
    let mut file = installed.create_file(name)?;
    let result = (|| -> Result<()> {
        if copy(source, Some(&mut file))? != expected {
            bail!("update_image_conflict");
        }
        file.sync_all()?;
        // Verification precedes executable permission, and permissions are set
        // on the retained handle rather than by reopening an ambient path.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        drop(file);
        matches(&installed.read_file(name)?, expected)?;
        installed.sync()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = installed.remove_file(name);
    }
    result
}

/// Back up the exact observed old image privately, then stage authenticated new
/// bytes. No installed name changes here. Partial preparation is NOT restartable
/// by resubmitting: the caller retains the job for explicit recovery/cleanup.
pub fn prepare(
    installed: &InstallationDirectory,
    journal: &Journal,
    revision: u64,
    binary: Binary,
    authenticated_source: &File,
) -> Result<()> {
    let receipt = current(journal, revision)?;
    if receipt.phase != Phase::Preparing {
        bail!("update_revision_or_phase_conflict");
    }
    matches(authenticated_source, target(&receipt, binary))?;
    let source = installed.read_file(binary.name())?;
    let expected = receipt.publications.original_digest(binary);
    matches(&source, expected)?;
    let original_mode = source.metadata()?.permissions().mode() & 0o777;
    validate_mode(original_mode)?;
    let mut saved = journal.directory.create_file(backup(binary))?;
    let result = (|| -> Result<()> {
        if copy(&source, Some(&mut saved))? != expected {
            bail!("update_image_conflict");
        }
        saved.sync_all()?;
        drop(saved);
        journal.directory.sync()?;
        matches(&journal.directory.read_file(backup(binary))?, expected)
    })();
    if let Err(error) = result {
        let _ = journal.directory.remove_file(backup(binary));
        return Err(error);
    }
    let mut mode = journal
        .directory
        .create_file(&format!("{}.mode", backup(binary)))?;
    mode.write_all(&original_mode.to_be_bytes())?;
    mode.sync_all()?;
    drop(mode);
    journal.directory.sync()?;
    if receipt.publications.step(binary) != FileStep::Unchanged {
        create_stage(
            installed,
            &stage(&receipt, binary, false),
            authenticated_source,
            target(&receipt, binary),
            0o755,
        )?;
    }
    Ok(())
}

/// Read-only batch preflight before changing either installed image. Backups,
/// original modes, current bytes and (when needed) executable stages must all
/// match the Prepared job. This is not a claim that two renames are atomic.
pub(super) fn preflight(
    installed: &InstallationDirectory,
    journal: &Journal,
    revision: u64,
    binary: Binary,
) -> Result<()> {
    let receipt = current(journal, revision)?;
    if receipt.phase != Phase::Prepared {
        bail!("update_revision_or_phase_conflict");
    }
    matches(
        &journal.directory.read_file(backup(binary))?,
        receipt.publications.original_digest(binary),
    )?;
    let mode = backup_mode(journal, binary)?;
    let original = installed.read_file(binary.name())?;
    matches(&original, receipt.publications.original_digest(binary))?;
    if original.metadata()?.permissions().mode() & 0o7777 != mode {
        bail!("update_original_mode_changed");
    }
    if receipt.publications.step(binary) != FileStep::Unchanged {
        let staged = installed.read_file(&stage(&receipt, binary, false))?;
        matches(&staged, target(&receipt, binary))?;
        if staged.metadata()?.permissions().mode() & 0o7777 != 0o755 {
            bail!("invalid_update_stage_mode");
        }
    }
    Ok(())
}

/// Install one image, persisting the intent BEFORE atomic rename and completion
/// AFTER it. Reopening the same job after a crash only reconciles that intent;
/// it never turns a terminal job into another update or performs activation.
/// Caller must retain installation THEN enrollment locks for the entire call.
pub fn install(
    installed: &InstallationDirectory,
    journal: &Journal,
    revision: u64,
    binary: Binary,
) -> Result<Receipt> {
    let mut receipt = current(journal, revision)?;
    if receipt.phase != Phase::Activating {
        bail!("update_revision_or_phase_conflict");
    }
    matches(
        &journal.directory.read_file(backup(binary))?,
        receipt.publications.original_digest(binary),
    )?;
    backup_mode(journal, binary)?;
    let actual = copy(&installed.read_file(binary.name())?, None)?;
    let decision = receipt.reconcile_file(binary, Some(&actual));
    match (receipt.publications.step(binary), decision) {
        (FileStep::Unchanged | FileStep::Installed, Reconcile::NoMutation) => return Ok(receipt),
        (FileStep::Original, Reconcile::NoMutation) => {
            matches(
                &installed.read_file(&stage(&receipt, binary, false))?,
                target(&receipt, binary),
            )?;
            receipt = journal.record_file(receipt.revision, binary, FileStep::InstallIntent)?;
        }
        (FileStep::InstallIntent, Reconcile::InstallVerified) => {}
        (FileStep::InstallIntent, Reconcile::RecordInstalled) => {
            return journal.record_file(receipt.revision, binary, FileStep::Installed);
        }
        _ => bail!("update_image_conflict"),
    }
    let staged = stage(&receipt, binary, false);
    matches(&installed.read_file(&staged)?, target(&receipt, binary))?;
    installed.replace_executable(&staged, binary.name())?;
    matches(
        &installed.read_file(binary.name())?,
        target(&receipt, binary),
    )?;
    journal.record_file(receipt.revision, binary, FileStep::Installed)
}

/// Restore only a verified private backup from this job. Ambiguous or missing
/// installed bytes fail closed; a failed rollback retains its receipt/backups.
pub fn restore(
    installed: &InstallationDirectory,
    journal: &Journal,
    revision: u64,
    binary: Binary,
) -> Result<Receipt> {
    let mut receipt = current(journal, revision)?;
    if receipt.phase != Phase::RollingBack {
        bail!("update_revision_or_phase_conflict");
    }
    let saved = journal.directory.read_file(backup(binary))?;
    matches(&saved, receipt.publications.original_digest(binary))?;
    let original_mode = backup_mode(journal, binary)?;
    let actual = copy(&installed.read_file(binary.name())?, None)?;
    if receipt.reconcile_file(binary, Some(&actual)) == Reconcile::Conflict {
        bail!("update_image_conflict");
    }
    match receipt.publications.step(binary) {
        FileStep::Original | FileStep::Unchanged | FileStep::Restored => return Ok(receipt),
        FileStep::Installed | FileStep::InstallIntent => {
            receipt = journal.record_file(receipt.revision, binary, FileStep::RestoreIntent)?;
        }
        FileStep::RestoreIntent => {}
    }
    match receipt.reconcile_file(binary, Some(&actual)) {
        Reconcile::RecordRestored => {
            if installed
                .read_file(binary.name())?
                .metadata()?
                .permissions()
                .mode()
                & 0o777
                != original_mode
            {
                bail!("invalid_update_backup_mode");
            }
        }
        Reconcile::RestoreVerified => {
            let staged = stage(&receipt, binary, true);
            match installed.read_file(&staged) {
                Ok(file) => {
                    matches(&file, receipt.publications.original_digest(binary))?;
                    if file.metadata()?.permissions().mode() & 0o777 != original_mode {
                        bail!("invalid_update_backup_mode");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    create_stage(
                        installed,
                        &staged,
                        &saved,
                        receipt.publications.original_digest(binary),
                        original_mode,
                    )?;
                }
                Err(error) => return Err(error.into()),
            }
            installed.replace_executable(&staged, binary.name())?;
            matches(
                &installed.read_file(binary.name())?,
                receipt.publications.original_digest(binary),
            )?;
        }
        _ => bail!("update_image_conflict"),
    }
    journal.record_file(receipt.revision, binary, FileStep::Restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{publication::Publications, Source};
    use chrono::Utc;
    use std::io::Read;
    use std::{
        os::unix::fs::{symlink, DirBuilderExt},
        path::PathBuf,
    };
    use treer_platform::private_directory::PrivateDirectory;
    use uuid::Uuid;

    struct Fixture {
        path: PathBuf,
        installed: InstallationDirectory,
        jobs: PrivateDirectory,
        _lock: File,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-images-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let installed = InstallationDirectory::open(&path).unwrap();
            let jobs = PrivateDirectory::open(&path)
                .unwrap()
                .create_child("jobs")
                .unwrap();
            let lock = installed.lock().unwrap();
            Self {
                path,
                installed,
                jobs,
                _lock: lock,
            }
        }
        fn journal(&self, old: &[u8], new: &[u8]) -> Journal {
            for binary in [Binary::Controller, Binary::Cli] {
                let mut file = self.installed.create_file(binary.name()).unwrap();
                file.write_all(old).unwrap();
                file.set_permissions(std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let original = digest(old);
            let target = digest(new);
            let now = Utc::now();
            Journal::create(
                &self.jobs,
                &Receipt {
                    schema_version: 1,
                    job_id: Uuid::new_v4(),
                    revision: 0,
                    workspace_id: "ws_fixture".into(),
                    server_id: "srv_fixture".into(),
                    release_sha: "a".repeat(40),
                    artifact_source_sha: "b".repeat(40),
                    controller_sha256: target.clone(),
                    cli_sha256: target.clone(),
                    publications: Publications::new(original.clone(), original, &target, &target),
                    source: Source::Pin {
                        sums_sha256: "c".repeat(64),
                        allow_pinned: true,
                        allow_artifact_origin: false,
                        allow_same: true,
                        allow_downgrade: false,
                    },
                    phase: Phase::Preparing,
                    created_at: now,
                    updated_at: now,
                },
            )
            .unwrap()
        }
        fn source(&self, bytes: &[u8]) -> File {
            let mut file = self.installed.create_file("source").unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
            drop(file);
            self.installed.read_file("source").unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }
    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn activate(j: &Journal) -> Receipt {
        let r = j.prepare_fixture().unwrap();
        j.advance(r.revision, Phase::Activating).unwrap()
    }

    #[test]
    fn verified_backups_install_and_rollback_use_real_atomic_replacements() {
        let f = Fixture::new();
        let j = f.journal(b"old", b"new");
        let source = f.source(b"new");
        for b in [Binary::Controller, Binary::Cli] {
            prepare(&f.installed, &j, 0, b, &source).unwrap();
        }
        assert_eq!(
            std::fs::metadata(f.path.join(jobs_backup(&j, Binary::Controller)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut old_reader = f.installed.read_file(Binary::Controller.name()).unwrap();
        let mut r = activate(&j);
        for b in [Binary::Controller, Binary::Cli] {
            r = install(&f.installed, &j, r.revision, b).unwrap();
        }
        assert_eq!(
            std::fs::read(f.path.join("treer-agent-server")).unwrap(),
            b"new"
        );
        let mut old = String::new();
        old_reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old");
        r = j.advance(r.revision, Phase::RollingBack).unwrap();
        for b in [Binary::Controller, Binary::Cli] {
            r = restore(&f.installed, &j, r.revision, b).unwrap();
        }
        r = j.advance(r.revision, Phase::RolledBack).unwrap();
        assert_eq!(
            std::fs::read(f.path.join("treer-agent-server")).unwrap(),
            b"old"
        );
        assert_eq!(std::fs::read(f.path.join("treer")).unwrap(), b"old");
        assert_eq!(
            std::fs::metadata(f.path.join("treer"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(install(&f.installed, &j, r.revision, Binary::Controller).is_err());
        assert!(restore(&f.installed, &j, r.revision, Binary::Controller).is_err());
    }
    fn jobs_backup(j: &Journal, b: Binary) -> PathBuf {
        PathBuf::from("jobs").join(j.id.to_string()).join(backup(b))
    }

    #[test]
    fn corrupt_download_or_backup_never_changes_installed_names() {
        let f = Fixture::new();
        let j = f.journal(b"old", b"new");
        let source = f.source(b"untrusted");
        assert!(prepare(&f.installed, &j, 0, Binary::Controller, &source).is_err());
        assert!(j.directory.read_file(backup(Binary::Controller)).is_err());
        assert_eq!(
            std::fs::read(f.path.join("treer-agent-server")).unwrap(),
            b"old"
        );
        assert_eq!(j.read().unwrap().revision, 0);
    }

    #[test]
    fn install_recovers_a_crash_after_rename_without_requiring_consumed_stage() {
        let f = Fixture::new();
        let j = f.journal(b"old", b"new");
        let source = f.source(b"new");
        prepare(&f.installed, &j, 0, Binary::Controller, &source).unwrap();
        let r = activate(&j);
        let r = j
            .record_file(r.revision, Binary::Controller, FileStep::InstallIntent)
            .unwrap();
        f.installed
            .replace_executable(
                &stage(&r, Binary::Controller, false),
                Binary::Controller.name(),
            )
            .unwrap();
        let reopened = Journal::open(&f.jobs, &j.id.to_string()).unwrap();
        let r = install(&f.installed, &reopened, r.revision, Binary::Controller).unwrap();
        assert_eq!(r.publications.step(Binary::Controller), FileStep::Installed);
        assert_eq!(
            install(&f.installed, &reopened, r.revision, Binary::Controller)
                .unwrap()
                .revision,
            r.revision
        );
        assert!(install(&f.installed, &reopened, r.revision - 1, Binary::Controller).is_err());
    }

    #[test]
    fn corrupted_backup_and_unknown_destination_fail_before_rollback_mutation() {
        let f = Fixture::new();
        let j = f.journal(b"old", b"new");
        let source = f.source(b"new");
        prepare(&f.installed, &j, 0, Binary::Controller, &source).unwrap();
        let r = activate(&j);
        let r = install(&f.installed, &j, r.revision, Binary::Controller).unwrap();
        let r = j.advance(r.revision, Phase::RollingBack).unwrap();
        std::fs::write(f.path.join(jobs_backup(&j, Binary::Controller)), b"corrupt").unwrap();
        assert!(restore(&f.installed, &j, r.revision, Binary::Controller).is_err());
        assert_eq!(j.read().unwrap().revision, r.revision);
        assert_eq!(
            std::fs::read(f.path.join("treer-agent-server")).unwrap(),
            b"new"
        );
        std::fs::write(f.path.join(jobs_backup(&j, Binary::Controller)), b"old").unwrap();
        std::fs::write(f.path.join("treer-agent-server"), b"unknown").unwrap();
        assert!(restore(&f.installed, &j, r.revision, Binary::Controller).is_err());
        assert_eq!(j.read().unwrap().revision, r.revision);
    }

    #[test]
    fn rollback_recovers_on_both_sides_of_rename_and_restores_original_permissions() {
        for renamed in [false, true] {
            let f = Fixture::new();
            let j = f.journal(b"old", b"new");
            let source = f.source(b"new");
            prepare(&f.installed, &j, 0, Binary::Controller, &source).unwrap();
            let r = activate(&j);
            let r = install(&f.installed, &j, r.revision, Binary::Controller).unwrap();
            let r = j.advance(r.revision, Phase::RollingBack).unwrap();
            let r = j
                .record_file(r.revision, Binary::Controller, FileStep::RestoreIntent)
                .unwrap();
            let staged = stage(&r, Binary::Controller, true);
            create_stage(
                &f.installed,
                &staged,
                &j.directory.read_file(backup(Binary::Controller)).unwrap(),
                r.publications.original_digest(Binary::Controller),
                0o700,
            )
            .unwrap();
            if renamed {
                f.installed
                    .replace_executable(&staged, Binary::Controller.name())
                    .unwrap();
            }
            let reopened = Journal::open(&f.jobs, &j.id.to_string()).unwrap();
            let r = restore(&f.installed, &reopened, r.revision, Binary::Controller).unwrap();
            assert_eq!(r.publications.step(Binary::Controller), FileStep::Restored);
            assert_eq!(
                std::fs::read(f.path.join("treer-agent-server")).unwrap(),
                b"old"
            );
            assert_eq!(
                std::fs::metadata(f.path.join("treer-agent-server"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn same_image_skips_replace_and_alias_stages_cannot_be_published() {
        let f = Fixture::new();
        let j = f.journal(b"same", b"same");
        let source = f.source(b"same");
        prepare(&f.installed, &j, 0, Binary::Controller, &source).unwrap();
        let r = activate(&j);
        let before = std::fs::metadata(f.path.join("treer-agent-server")).unwrap();
        let r = install(&f.installed, &j, r.revision, Binary::Controller).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            before.ino(),
            std::fs::metadata(f.path.join("treer-agent-server"))
                .unwrap()
                .ino()
        );
        assert_eq!(r.publications.step(Binary::Controller), FileStep::Unchanged);
        let other = Fixture::new();
        let j = other.journal(b"old", b"new");
        let source = other.source(b"new");
        prepare(&other.installed, &j, 0, Binary::Controller, &source).unwrap();
        let r = activate(&j);
        let staged = stage(&r, Binary::Controller, false);
        other.installed.remove_file(&staged).unwrap();
        symlink("source", other.path.join(&staged)).unwrap();
        assert!(install(&other.installed, &j, r.revision, Binary::Controller).is_err());
        assert_eq!(j.read().unwrap().revision, r.revision);
    }
}
