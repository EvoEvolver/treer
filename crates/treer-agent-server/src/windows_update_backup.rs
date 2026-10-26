//! Copy checked old images, then publish backup-only evidence. Never replace
//! an installed image, mark rollback/update success or resume an old job.
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

fn copy_checked(
    source: &mut dyn Read,
    target: &mut dyn Write,
    expected: &str,
    limit: u64,
) -> Result<()> {
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0; 16 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > limit {
            bail!("windows_update_backup_size_refused");
        }
        target.write_all(&buffer[..read])?;
        hash.update(&buffer[..read]);
    }
    if size == 0 || format!("{:x}", hash.finalize()) != expected {
        bail!("windows_update_backup_digest_refused");
    }
    Ok(())
}

#[cfg(windows)]
mod native {
    use super::super::ReservedInputs;
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::io::{Seek, SeekFrom};
    use treer_platform::windows_update_backup::{BackupDirectory, SealedBackup};

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct Evidence {
        schema_version: u32,
        phase: Phase,
        binding: crate::windows_update_intent::Binding,
        directory_volume: u64,
        directory_file_id: u64,
        controller_file_id: u64,
        cli_file_id: u64,
        controller_size: u64,
        cli_size: u64,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum Phase {
        ImagesBackedUp,
    }

    /// Retains installed/target pins and installation lease with checked backup
    /// files. There is no public constructor, execution, resume or success API.
    pub struct BackedUpInputs {
        reserved: ReservedInputs,
        backup: SealedBackup,
    }

    impl ReservedInputs {
        /// Consume a single reservation. Partial files and unknown publication
        /// remain evidence on error, never cleaned up or automatically retried.
        pub fn backup(self, directory: BackupDirectory) -> Result<BackedUpInputs> {
            let identity = directory.identity()?;
            if identity.0 != self.binding().installation_volume
                || identity == self.0._installed.identity()?
                || identity == self.0._target.identity()?
            {
                bail!("windows_update_backup_directory_refused");
            }
            self.0.inspect_installation_claim()?;
            let mut writing = directory.create()?;
            let (mut old_controller, mut old_cli) = self.0._installed.files();
            old_controller.seek(SeekFrom::Start(0))?;
            old_cli.seek(SeekFrom::Start(0))?;
            let (controller, cli) = writing.files();
            copy_checked(
                &mut old_controller,
                controller,
                &self.binding().old_controller_sha256,
                128 * 1024 * 1024,
            )?;
            copy_checked(
                &mut old_cli,
                cli,
                &self.binding().old_cli_sha256,
                128 * 1024 * 1024,
            )?;
            let backed = BackedUpInputs {
                reserved: self,
                backup: writing.seal()?,
            };
            backed.verify_images()?;
            let bytes = serde_json::to_vec(&backed.expected_evidence()?)?;
            backed.backup.publish_evidence(&bytes)?;
            backed.inspect_evidence()?;
            Ok(backed)
        }
    }

    impl BackedUpInputs {
        pub fn binding(&self) -> &crate::windows_update_intent::Binding {
            self.reserved.binding()
        }

        fn expected_evidence(&self) -> Result<Evidence> {
            let (volume, directory_id) = self.backup.identity()?;
            let (controller, cli) = self.backup.files();
            let (cv, controller_id, _, _) = treer_platform::transfer_file_identity(controller)?;
            let (lv, cli_id, _, _) = treer_platform::transfer_file_identity(cli)?;
            if volume != cv
                || volume != lv
                || directory_id == 0
                || controller_id == 0
                || cli_id == 0
            {
                bail!("windows_update_backup_identity_refused");
            }
            Ok(Evidence {
                schema_version: 1,
                phase: Phase::ImagesBackedUp,
                binding: self.binding().clone(),
                directory_volume: volume,
                directory_file_id: directory_id,
                controller_file_id: controller_id,
                cli_file_id: cli_id,
                controller_size: controller.metadata()?.len(),
                cli_size: cli.metadata()?.len(),
            })
        }

        fn verify_images(&self) -> Result<()> {
            let (mut controller, mut cli) = self.backup.files();
            controller.seek(SeekFrom::Start(0))?;
            cli.seek(SeekFrom::Start(0))?;
            if super::super::digest(&mut controller, None)? != self.binding().old_controller_sha256
                || super::super::digest(&mut cli, None)? != self.binding().old_cli_sha256
            {
                bail!("windows_update_backup_readback_refused");
            }
            Ok(())
        }

        fn inspect_evidence(&self) -> Result<()> {
            let actual: Evidence = serde_json::from_slice(&self.backup.read_evidence()?)?;
            if actual != self.expected_evidence()? {
                bail!("windows_update_backup_evidence_conflict");
            }
            Ok(())
        }

        /// Check held bytes and the strict, exact evidence. Successful inspection
        /// grants no replacement, rollback, retirement or health authority.
        pub fn inspect_backups(&self) -> Result<()> {
            self.verify_images()?;
            self.inspect_evidence()
        }
    }
}

#[cfg(windows)]
pub use native::BackedUpInputs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_update_backup_copy_checks_full_bytes_bounds_and_digest() {
        let bytes = b"original image";
        let hash = format!("{:x}", Sha256::digest(bytes));
        let mut target = Vec::new();
        copy_checked(&mut &bytes[..], &mut target, &hash, 32).unwrap();
        assert_eq!(target, bytes);
        assert!(copy_checked(&mut &bytes[..], &mut Vec::new(), &hash, 1).is_err());
        assert!(copy_checked(&mut &bytes[..], &mut Vec::new(), &"0".repeat(64), 32).is_err());
        assert!(copy_checked(&mut &b""[..], &mut Vec::new(), &hash, 32).is_err());
    }

    #[test]
    fn windows_update_backup_copy_propagates_read_and_partial_write_failure() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WriteZero.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(copy_checked(&mut Broken, &mut Vec::new(), &"0".repeat(64), 32).is_err());
        let hash = format!("{:x}", Sha256::digest(b"abc"));
        assert!(copy_checked(&mut &b"abc"[..], &mut Broken, &hash, 32).is_err());
        struct Partial(Vec<u8>);
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.0.is_empty() {
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                self.0.extend_from_slice(&bytes[..2]);
                Ok(2)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut partial = Partial(Vec::new());
        assert!(copy_checked(&mut &b"abc"[..], &mut partial, &hash, 32).is_err());
        assert_eq!(partial.0, b"ab");
    }
}
