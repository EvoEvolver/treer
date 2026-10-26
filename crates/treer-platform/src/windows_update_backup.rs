//! Private backup files only; the Controller authenticates bytes and evidence.
//! No installed file replacement, process control, recovery or cleanup API.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub struct BackupDirectory {
    path: PathBuf,
    root: crate::machine_file::Root,
}

impl BackupDirectory {
    /// Only an existing private physical fixed-NTFS directory. No ACL repair.
    pub fn open(path: &Path) -> io::Result<Self> {
        let root = crate::machine_file::Root::open(path, &[])?;
        if !crate::private_path_is_secure(path)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe update backup",
            ));
        }
        crate::windows::require_update_ntfs(path)?;
        Ok(Self {
            path: path.into(),
            root,
        })
    }

    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.root.identity()
    }

    /// Partial creation is unresolved evidence, never automatically deleted.
    /// Every image and evidence name is exclusive, fixed and private.
    pub fn create(self) -> io::Result<WritingBackup> {
        match std::fs::symlink_metadata(self.path.join("backup.evidence")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(io::Error::other(
                    "backup evidence already exists or is unreadable",
                ))
            }
        }
        let controller =
            crate::windows::create_private_update_file(&self.path.join("controller.backup"))?;
        let cli = crate::windows::create_private_update_file(&self.path.join("cli.backup"))?;
        Ok(WritingBackup {
            directory: self,
            controller,
            cli,
        })
    }
}

pub struct WritingBackup {
    directory: BackupDirectory,
    controller: File,
    cli: File,
}

impl WritingBackup {
    pub fn files(&mut self) -> (&mut File, &mut File) {
        (&mut self.controller, &mut self.cli)
    }

    /// Flush both files, then retain reduced-access duplicates of those SAME
    /// file objects. There is no path reopen or write/delete-sharing interval.
    pub fn seal(self) -> io::Result<SealedBackup> {
        self.controller.sync_all()?;
        self.cli.sync_all()?;
        crate::windows_update_store::checked_file(
            &self.directory.path.join("controller.backup"),
            &self.controller,
        )?;
        crate::windows_update_store::checked_file(
            &self.directory.path.join("cli.backup"),
            &self.cli,
        )?;
        Ok(SealedBackup {
            directory: self.directory,
            controller: crate::windows::readonly_update_file(self.controller)?,
            cli: crate::windows::readonly_update_file(self.cli)?,
        })
    }
}

/// Owns the physical directory and readonly, exclusive image handles. Bytes
/// remain untrusted until the Controller checks hashes and writes its evidence.
pub struct SealedBackup {
    directory: BackupDirectory,
    controller: File,
    cli: File,
}

impl SealedBackup {
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.directory.identity()
    }
    pub fn files(&self) -> (&File, &File) {
        (&self.controller, &self.cli)
    }

    /// Write-once opaque evidence, not a platform interpretation of success.
    pub fn publish_evidence(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid backup evidence size",
            ));
        }
        let path = self.directory.path.join("backup.evidence");
        let mut file = crate::windows::create_private_update_file(&path)?;
        crate::windows_update_store::checked_file(&path, &file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let mut readback = Vec::new();
        (&mut file).take(4097).read_to_end(&mut readback)?;
        if readback != bytes {
            return Err(io::Error::other("backup evidence readback differs"));
        }
        Ok(())
    }

    pub fn read_evidence(&self) -> io::Result<Vec<u8>> {
        let mut file = crate::windows_update_store::open_record(
            &self.directory.path.join("backup.evidence"),
            false,
        )?;
        let mut bytes = Vec::new();
        (&mut file).take(4097).read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::other("invalid backup evidence size"));
        }
        Ok(bytes)
    }
}
