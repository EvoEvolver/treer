//! Retained authenticated executable for self-update child calls.
//! No spawn, receipt transition, path discovery or PATH fallback here.
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Result};
use treer_platform::installation_directory::InstallationDirectory;

use super::{hex, images::digest_file};

/// Internal capability: retains the exact checked inode and physical parent.
/// Callers must still hold their installation/enrollment locks, verify directly
/// before execution and after quiescence, and construct clean fixed commands.
/// Same-account actors remain one trust domain; this does not make path-based
/// exec atomic with verification or protect against a malicious same-user writer.
pub(super) struct VerifiedImage<'a> {
    directory: &'a InstallationDirectory,
    parent: &'a Path,
    name: &'a str,
    digest: &'a str,
    file: File,
}

impl<'a> VerifiedImage<'a> {
    pub(super) fn open(
        directory: &'a InstallationDirectory,
        path: &'a Path,
        digest: &'a str,
    ) -> Result<Self> {
        if !hex(digest, 64)
            || !path.is_absolute()
            || !path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        {
            bail!("unsafe_update_image_probe");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?;
        let current = InstallationDirectory::open(parent)
            .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?;
        let identity = current
            .identity()
            .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?;
        if identity
            != directory
                .identity()
                .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?
        {
            bail!("unsafe_update_image_probe");
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?;
        let file = directory
            .read_file(name)
            .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?;
        if file
            .metadata()
            .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?
            .permissions()
            .mode()
            & 0o100
            == 0
        {
            bail!("update_image_not_executable");
        }
        let image = Self {
            directory,
            parent,
            name,
            digest,
            file,
        };
        image.verify()?;
        Ok(image)
    }

    /// Derive the execution path from THIS retained authentication object;
    /// callers cannot accidentally pair its proof with a different argv path.
    pub(super) fn executable(&self) -> PathBuf {
        self.parent.join(self.name)
    }

    pub(super) fn verify(&self) -> Result<()> {
        let checked = (|| -> Result<()> {
            let parent = InstallationDirectory::open(self.parent)?;
            if parent.identity()? != self.directory.identity()? {
                bail!("changed");
            }
            let reopened = self.directory.read_file(self.name)?;
            let held = self.file.metadata()?;
            let current = reopened.metadata()?;
            if (held.dev(), held.ino()) != (current.dev(), current.ino())
                || digest_file(&reopened)? != self.digest
            {
                bail!("changed");
            }
            if current.permissions().mode() & 0o100 == 0 {
                bail!("not_executable");
            }
            Ok(())
        })();
        checked.map_err(|_| anyhow::anyhow!("update_image_conflict"))
    }
}
