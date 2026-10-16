//! Prepare the old Controller's job-local activator image before any installed
//! name changes, and attest the running copy before child-side session setup.
//! This does not spawn a worker or authorize an activation.
use std::{
    fs::File,
    io::Write,
    os::unix::fs::{FileExt, MetadataExt, PermissionsExt},
};

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

use super::{
    artifacts::Binary,
    images::digest_file,
    selection::{Selected, SelectionLocks},
    Journal, Phase, Receipt,
};

const NAME: &str = "activator";
const LIMIT: u64 = 128 * 1024 * 1024;

/// A retained read-only image, bound to one job's complete preparation receipt.
/// No Debug/Serialize: neither a path nor arbitrary image bytes enter receipts.
pub struct ActivatorImage {
    image: File,
    preparing: Receipt,
}

impl ActivatorImage {
    /// Hold both selection locks. Copy only the selected installed Controller,
    /// whose original digest is in the journal; never PATH/current_exe or new
    /// downloaded bytes. Existing/partial images are retained, not overwritten.
    pub fn prepare(selected: &Selected, locks: &SelectionLocks, journal: &Journal) -> Result<Self> {
        let preparing = journal.read()?;
        if preparing.phase != Phase::Preparing
            || preparing.workspace_id != selected.workspace()
            || preparing.server_id != selected.server_id()
        {
            bail!("update_activator_job_mismatch");
        }
        let directory = locks.directory(selected, Binary::Controller)?;
        let source = directory
            .read_file(Binary::Controller.name())
            .map_err(|_| anyhow::anyhow!("update_activator_image_unavailable"))?;
        let mode = source.metadata()?.permissions().mode();
        if mode & 0o500 != 0o500 || mode & 0o7022 != 0 {
            bail!("update_activator_image_unsafe");
        }
        let expected = preparing
            .publications
            .original_digest(Binary::Controller)
            .to_owned();
        let mut target = journal
            .directory
            .create_file(NAME)
            .map_err(|_| anyhow::anyhow!("update_activator_image_exists_or_unavailable"))?;
        // The new file stays non-executable until the bytes actually copied
        // match the bound digest. Any failure retains evidence for recovery.
        let mut hasher = Sha256::new();
        let mut offset = 0;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = source.read_at(&mut buffer, offset)?;
            if count == 0 {
                break;
            }
            offset += count as u64;
            if offset > LIMIT {
                bail!("update_activator_image_too_large");
            }
            hasher.update(&buffer[..count]);
            target.write_all(&buffer[..count])?;
        }
        if offset == 0 || format!("{:x}", hasher.finalize()) != expected {
            bail!("update_activator_image_conflict");
        }
        target.sync_all()?;
        target.set_permissions(std::fs::Permissions::from_mode(0o500))?;
        target.sync_all()?;
        // No writable handle survives preparation (important for executable
        // launch on Linux). Keep the reopened exact inode for peer attestation.
        let identity = target.metadata()?;
        drop(target);
        journal.directory.sync()?;
        let image = journal.directory.read_file(NAME)?;
        let actual = image.metadata()?;
        if (identity.dev(), identity.ino()) != (actual.dev(), actual.ino()) {
            bail!("update_activator_image_conflict");
        }
        let result = Self { image, preparing };
        result.verify(journal)?;
        // Recheck the installed source too: a cooperating updater owns locks,
        // but never accept an image changed by an uncooperative local writer.
        if digest_file(&directory.read_file(Binary::Controller.name())?)? != expected {
            bail!("update_activator_image_conflict");
        }
        Ok(result)
    }

    /// Revalidate before launch/peer authentication. Permit only the ordinary
    /// single Preparing -> Prepared transition, with all other fields equal.
    pub fn verify(&self, journal: &Journal) -> Result<()> {
        let current = journal.read()?;
        let mut expected = self.preparing.clone();
        if current.phase == Phase::Prepared
            && Some(current.revision) == expected.revision.checked_add(1)
        {
            expected.phase = Phase::Prepared;
            expected.revision = current.revision;
            expected.updated_at = current.updated_at;
        }
        if current != expected {
            bail!("update_activator_job_changed");
        }
        let named = journal.directory.read_file(NAME)?;
        let held = self.image.metadata()?;
        let actual = named.metadata()?;
        if (held.dev(), held.ino()) != (actual.dev(), actual.ino())
            || actual.permissions().mode() & 0o7777 != 0o500
            || digest_file(&named)? != self.digest()
            || digest_file(&self.image)? != self.digest()
        {
            bail!("update_activator_image_conflict");
        }
        Ok(())
    }

    pub fn image(&self) -> &File {
        &self.image
    }
    pub fn digest(&self) -> &str {
        self.preparing
            .publications
            .original_digest(Binary::Controller)
    }
}

/// Child-side proof, retained only in the process actually executing the job's
/// old Controller copy. No Debug/Serialize or path-based executable fallback.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct RunningActivator {
    process: treer_platform::process_image::ProcessImage,
    prepared: Receipt,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RunningActivator {
    /// Invoke synchronously after exec, before connecting the handoff endpoint
    /// or starting runtime threads. A failure must terminate worker startup.
    pub fn enter(selected: &Selected, journal: &Journal) -> Result<Self> {
        let prepared = journal.read()?;
        if prepared.phase != Phase::Prepared
            || prepared.workspace_id != selected.workspace()
            || prepared.server_id != selected.server_id()
        {
            bail!("update_activator_job_mismatch");
        }
        let result = Self {
            process: treer_platform::process_image::ProcessImage::capture(std::process::id())
                .map_err(|_| anyhow::anyhow!("update_activator_process_unavailable"))?,
            prepared,
        };
        // Check the running inode and bytes before changing the session. An
        // arbitrary executable with identical bytes is not the job's copy.
        result.verify_image(selected, journal)?;
        treer_platform::process_image::enter_private_session()
            .map_err(|_| anyhow::anyhow!("update_activator_session_unavailable"))?;
        result.verify(selected, journal)?;
        Ok(result)
    }

    pub fn verify(&self, selected: &Selected, journal: &Journal) -> Result<()> {
        self.verify_image(selected, journal)?;
        self.process
            .verify_private_session()
            .map_err(|_| anyhow::anyhow!("update_activator_session_unavailable"))
    }

    fn verify_image(&self, selected: &Selected, journal: &Journal) -> Result<()> {
        if self.process.pid() != std::process::id()
            || self.prepared.workspace_id != selected.workspace()
            || self.prepared.server_id != selected.server_id()
            || journal.read()? != self.prepared
        {
            bail!("update_activator_job_changed");
        }
        self.process
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_activator_process_unavailable"))?;
        let named = journal.directory.read_file(NAME)?;
        let expected = self
            .prepared
            .publications
            .original_digest(Binary::Controller);
        if named.metadata()?.permissions().mode() & 0o7777 != 0o500
            || !self.process.matches_file(&named)?
            || digest_file(&named)? != expected
            || digest_file(self.process.executable())? != expected
        {
            bail!("update_activator_image_conflict");
        }
        self.process
            .verify_current()
            .map_err(|_| anyhow::anyhow!("update_activator_process_unavailable"))?;
        if journal.read()? != self.prepared {
            bail!("update_activator_job_changed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{
        publication::Publications, selection::tests::Fixture, tests::receipt,
    };
    use treer_platform::private_directory::PrivateDirectory;

    fn fixture() -> (Fixture, Selected, SelectionLocks, Journal) {
        let f = Fixture::new();
        f.enrollment("srv_a", "ws_a", &f.bin.join("treer-agent-server"));
        std::fs::set_permissions(
            f.bin.join("treer-agent-server"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let selected = f.inventory().select(None, false).unwrap();
        let locks = selected.lock().unwrap();
        let mut r = receipt();
        r.workspace_id = "ws_a".into();
        r.server_id = "srv_a".into();
        r.publications = Publications::new(
            format!("{:x}", Sha256::digest(b"controller")),
            "0".repeat(64),
            &r.controller_sha256,
            &r.cli_sha256,
        );
        let jobs = PrivateDirectory::open(&f.path).unwrap();
        let journal = Journal::create(&jobs, &r).unwrap();
        (f, selected, locks, journal)
    }

    #[test]
    fn private_old_image_is_independent_and_rechecked_across_preparation() {
        let (f, selected, locks, j) = fixture();
        let original = std::fs::read(f.bin.join("treer-agent-server")).unwrap();
        let image = ActivatorImage::prepare(&selected, &locks, &j).unwrap();
        assert_eq!(
            image.image().metadata().unwrap().permissions().mode() & 0o7777,
            0o500
        );
        assert_ne!(
            image.image().metadata().unwrap().ino(),
            std::fs::metadata(f.bin.join("treer-agent-server"))
                .unwrap()
                .ino()
        );
        assert_eq!(digest_file(image.image()).unwrap(), image.digest());
        assert!(ActivatorImage::prepare(&selected, &locks, &j).is_err());
        assert_eq!(
            std::fs::read(f.bin.join("treer-agent-server")).unwrap(),
            original
        );
        j.prepare_fixture().unwrap();
        image.verify(&j).unwrap();
        let r = j.read().unwrap();
        j.advance(r.revision, Phase::Activating).unwrap();
        assert_eq!(
            image.verify(&j).unwrap_err().to_string(),
            "update_activator_job_changed"
        );
    }

    #[test]
    fn bad_source_stays_non_executable_and_existing_file_is_never_replaced() {
        let (f, selected, locks, j) = fixture();
        std::fs::write(f.bin.join("treer-agent-server"), b"changed").unwrap();
        assert_eq!(
            ActivatorImage::prepare(&selected, &locks, &j)
                .err()
                .unwrap()
                .to_string(),
            "update_activator_image_conflict"
        );
        let partial = j.directory.read_file(NAME).unwrap();
        assert_eq!(partial.metadata().unwrap().permissions().mode() & 0o111, 0);
        let inode = partial.metadata().unwrap().ino();
        assert!(ActivatorImage::prepare(&selected, &locks, &j).is_err());
        assert_eq!(
            j.directory
                .read_file(NAME)
                .unwrap()
                .metadata()
                .unwrap()
                .ino(),
            inode
        );
    }

    #[test]
    fn replaced_tampered_or_writable_job_images_are_rejected() {
        for case in ["replace", "content", "mode"] {
            let (f, selected, locks, j) = fixture();
            let image = ActivatorImage::prepare(&selected, &locks, &j).unwrap();
            let path = f.path.join(j.read().unwrap().job_id.to_string()).join(NAME);
            match case {
                "replace" => {
                    std::fs::remove_file(&path).unwrap();
                    std::fs::write(&path, b"controller").unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                        .unwrap();
                }
                "content" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    std::fs::write(&path, b"tampered").unwrap();
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                        .unwrap();
                }
                _ => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap()
                }
            }
            assert!(image.verify(&j).is_err(), "{case}");
        }
    }

    #[test]
    fn another_jobs_copy_and_wrong_enrollment_are_never_adopted() {
        let (f, selected, locks, j) = fixture();
        let image = ActivatorImage::prepare(&selected, &locks, &j).unwrap();
        let mut r = j.read().unwrap();
        r.job_id = uuid::Uuid::new_v4();
        let jobs = PrivateDirectory::open(&f.path).unwrap();
        let other = Journal::create(&jobs, &r).unwrap();
        assert!(image.verify(&other).is_err());
        r.job_id = uuid::Uuid::new_v4();
        r.workspace_id = "ws_other".into();
        let wrong = Journal::create(&jobs, &r).unwrap();
        assert_eq!(
            ActivatorImage::prepare(&selected, &locks, &wrong)
                .err()
                .unwrap()
                .to_string(),
            "update_activator_job_mismatch"
        );
        assert!(wrong.directory.read_file(NAME).is_err());
    }

    #[test]
    fn preexisting_alias_is_not_followed_chmodded_or_deleted() {
        for hard_link in [false, true] {
            let (f, selected, locks, j) = fixture();
            let source = f.bin.join("treer-agent-server");
            let before = std::fs::metadata(&source).unwrap();
            let path = f.path.join(j.read().unwrap().job_id.to_string()).join(NAME);
            if hard_link {
                std::fs::hard_link(&source, &path).unwrap();
            } else {
                std::os::unix::fs::symlink(&source, &path).unwrap();
            }
            assert!(ActivatorImage::prepare(&selected, &locks, &j).is_err());
            assert!(std::fs::symlink_metadata(&path).is_ok());
            let after = std::fs::metadata(&source).unwrap();
            assert_eq!(
                (before.dev(), before.ino(), before.permissions().mode()),
                (after.dev(), after.ino(), after.permissions().mode())
            );
            assert_eq!(std::fs::read(&source).unwrap(), b"controller");
        }
    }
}
