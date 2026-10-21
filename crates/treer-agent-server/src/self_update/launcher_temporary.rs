//! Exclusive job-private temporary storage, without starting any child.
//! Never adopt an existing directory, chmod it, fall back to /tmp or delete it
//! on Drop/error: leftover storage is evidence for explicit recovery.
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use treer_platform::private_directory::PrivateDirectory;

use super::{
    artifacts::Binary, handoff::OwnedActivation, launcher_preflight, selection::Selected, Journal,
    Receipt,
};

const CHILD: &str = "launcher-tmp";

/// Keeps both the original job owner/locks and the temporary-directory fd alive.
/// Not Debug/Serialize; not a child-start or installer-execution capability.
pub struct LauncherTemporary<'a> {
    selected: &'a Selected,
    owned: &'a OwnedActivation,
    journal: &'a Journal,
    receipt: Receipt,
    job_path: PathBuf,
    path: PathBuf,
    directory: PrivateDirectory,
}

impl<'a> LauncherTemporary<'a> {
    /// job_path is an internal worker locator, not remote argv. Require its
    /// UUID and physical inode to match the original retained job handle BEFORE
    /// creating anything. mkdirat under that handle is exclusive and fsynced.
    pub fn create(
        selected: &'a Selected,
        owned: &'a OwnedActivation,
        journal: &'a Journal,
        job_path: &Path,
    ) -> Result<Self> {
        let receipt = launcher_preflight::checked(selected, owned, journal)?;
        owned.locks().directory(selected, Binary::Controller)?;
        owned.locks().directory(selected, Binary::Cli)?;
        let name = receipt.job_id.to_string();
        if job_path.file_name() != Some(std::ffi::OsStr::new(&name)) {
            bail!("update_launcher_temporary_binding_mismatch");
        }
        journal
            .directory
            .verify_path(job_path)
            .map_err(|_| anyhow::anyhow!("update_launcher_temporary_binding_mismatch"))?;
        let directory = journal
            .directory
            .create_child(CHILD)
            .map_err(|_| anyhow::anyhow!("update_launcher_temporary_unavailable"))?;
        let temporary = Self {
            selected,
            owned,
            journal,
            receipt,
            job_path: job_path.to_owned(),
            path: job_path.join(CHILD),
            directory,
        };
        temporary.verify()?;
        Ok(temporary)
    }

    pub fn verify(&self) -> Result<()> {
        if launcher_preflight::checked(self.selected, self.owned, self.journal)? != self.receipt {
            bail!("update_launcher_temporary_binding_mismatch");
        }
        self.owned
            .locks()
            .directory(self.selected, Binary::Controller)?;
        self.owned.locks().directory(self.selected, Binary::Cli)?;
        self.journal
            .directory
            .verify_path(&self.job_path)
            .and_then(|()| self.directory.verify_path(&self.path))
            .map_err(|_| anyhow::anyhow!("update_launcher_temporary_changed"))
    }

    pub fn path(&self) -> Result<&Path> {
        self.verify()?;
        Ok(&self.path)
    }

    /// A recipe must borrow this SAME selection, original owner and journal
    /// instance, not another job or a copied receipt with matching identifiers.
    pub(super) fn verify_for(
        &self,
        selected: &Selected,
        owned: &OwnedActivation,
        journal: &Journal,
        receipt: &Receipt,
    ) -> Result<()> {
        if !std::ptr::eq(self.selected, selected)
            || !std::ptr::eq(self.owned, owned)
            || !std::ptr::eq(self.journal, journal)
            || self.receipt != *receipt
        {
            bail!("update_launcher_temporary_binding_mismatch");
        }
        self.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::Publications, selection::tests::Fixture, tests::receipt, Phase,
    };
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
    };

    fn fixture() -> (Fixture, Selected, OwnedActivation, Journal, PathBuf) {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join(Binary::Controller.name()));
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let mut r = receipt();
        r.workspace_id = "ws_a".into();
        r.server_id = "srv_a".into();
        r.publications = Publications::new(
            r.controller_sha256.clone(),
            r.cli_sha256.clone(),
            &r.controller_sha256,
            &r.cli_sha256,
        );
        let job_path = f.path.join("jobs").join(r.job_id.to_string());
        let journal = Journal::create(&jobs, &r).unwrap();
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
            job_path,
        )
    }

    #[test]
    fn exclusive_private_storage_preserves_receipt_and_survives_drop_for_recovery() {
        let (_f, selected, owned, journal, path) = fixture();
        let before = journal.read().unwrap();
        let temporary = LauncherTemporary::create(&selected, &owned, &journal, &path).unwrap();
        assert_eq!(temporary.path().unwrap(), path.join(CHILD));
        assert_eq!(
            fs::metadata(temporary.path().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(LauncherTemporary::create(&selected, &owned, &journal, &path).is_err());
        assert_eq!(journal.read().unwrap(), before);
        let marker = temporary.path().unwrap().join("recovery-evidence");
        fs::write(&marker, b"preserve").unwrap();
        drop(temporary);
        assert_eq!(fs::read(marker).unwrap(), b"preserve");
    }

    #[test]
    fn refuses_existing_entries_without_adopting_chmodding_or_removing_them() {
        for kind in ["directory", "file", "symlink"] {
            let (f, selected, owned, journal, path) = fixture();
            let target = path.join(CHILD);
            match kind {
                "directory" => {
                    fs::create_dir(&target).unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
                }
                "file" => fs::write(&target, b"evidence").unwrap(),
                "symlink" => symlink(&f.path, &target).unwrap(),
                _ => unreachable!(),
            }
            let before = fs::symlink_metadata(&target).unwrap();
            let error = LauncherTemporary::create(&selected, &owned, &journal, &path)
                .err()
                .unwrap();
            assert_eq!(error.to_string(), "update_launcher_temporary_unavailable");
            let after = fs::symlink_metadata(&target).unwrap();
            assert_eq!(before.permissions().mode(), after.permissions().mode());
            assert_eq!(before.file_type(), after.file_type());
            if kind == "file" {
                assert_eq!(fs::read(&target).unwrap(), b"evidence");
            }
        }
    }

    #[test]
    fn wrong_physical_job_or_alias_fails_before_creating_storage() {
        let (f, selected, owned, journal, path) = fixture();
        let other = f.path.join(journal.read().unwrap().job_id.to_string());
        fs::create_dir(&other).unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = f.path.join("alias");
        symlink(path.parent().unwrap(), &alias).unwrap();
        let alias = alias.join(path.file_name().unwrap());
        for locator in [&other, &alias, &f.path] {
            assert_eq!(
                LauncherTemporary::create(&selected, &owned, &journal, locator)
                    .err()
                    .unwrap()
                    .to_string(),
                "update_launcher_temporary_binding_mismatch"
            );
        }
        assert!(!other.join(CHILD).exists());
        assert!(!path.join(CHILD).exists());
    }

    #[test]
    fn replaced_job_or_temporary_directory_invalidates_original_handles() {
        for replace_job in [false, true] {
            let (_f, selected, owned, journal, path) = fixture();
            let temporary = LauncherTemporary::create(&selected, &owned, &journal, &path).unwrap();
            let target = if replace_job {
                path.clone()
            } else {
                path.join(CHILD)
            };
            fs::rename(&target, target.with_extension("old")).unwrap();
            fs::create_dir(&target).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                temporary.path().err().unwrap().to_string(),
                "update_launcher_temporary_changed"
            );
        }
    }

    #[test]
    fn changed_permissions_and_same_phase_revision_are_rejected() {
        let (_f, selected, owned, journal, path) = fixture();
        let temporary = LauncherTemporary::create(&selected, &owned, &journal, &path).unwrap();
        fs::set_permissions(path.join(CHILD), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(temporary.verify().is_err());
        fs::set_permissions(path.join(CHILD), fs::Permissions::from_mode(0o700)).unwrap();
        temporary.verify().unwrap();
        let mut changed = journal.read().unwrap();
        changed.revision += 1;
        journal.write(&changed).unwrap();
        assert_eq!(
            temporary.verify().unwrap_err().to_string(),
            "update_launcher_temporary_binding_mismatch"
        );
    }
}
