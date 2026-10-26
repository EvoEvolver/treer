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
    /// Independent physical-directory guards under this owner's existing pins.
    /// This does not lock startup-store contents or prevent atomic file updates.
    pub fn repin(&self) -> io::Result<Self> {
        let expected = self.identity()?;
        let directory = Self::open(&self.path)?;
        if directory.identity()? != expected {
            return Err(io::Error::other("startup directory changed"));
        }
        Ok(directory)
    }

    /// One bounded, checked inode snapshot. Absence means only NotFound at this
    /// observation, not a fence against later Controller startup-store writes.
    pub fn startup_snapshot(&self, name: &str) -> io::Result<Option<(u64, Vec<u8>)>> {
        if name.is_empty()
            || matches!(name, "." | "..")
            || name.len() > 200
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        {
            return Err(io::Error::other("invalid snapshot name"));
        }
        let mut file = match crate::windows_update_store::open_record(&self.path.join(name), false)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let (volume, id, _, _) = crate::transfer_file_identity(&file)?;
        if id == 0 || volume != self.identity()?.0 {
            return Err(io::Error::other("snapshot volume differs"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("invalid startup snapshot size"));
        }
        Ok(Some((id, bytes)))
    }
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
    /// Write a fresh raw startup snapshot, preserving private credentials. An
    /// absent snapshot has no data file. Never overwrite either existing name.
    pub fn create_startup(&self, bytes: Option<&[u8]>) -> io::Result<Option<File>> {
        for name in ["startup.backup", "startup.evidence"] {
            match std::fs::symlink_metadata(self.directory.path.join(name)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(io::Error::other("startup backup exists or is unreadable")),
            }
        }
        let Some(bytes) = bytes else { return Ok(None) };
        if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("invalid startup backup size"));
        }
        let path = self.directory.path.join("startup.backup");
        let mut file = crate::windows::create_private_update_file(&path)?;
        crate::windows_update_store::checked_file(&path, &file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(Some(crate::windows::readonly_update_file(file)?))
    }

    pub fn startup_absent(&self) -> io::Result<bool> {
        match std::fs::symlink_metadata(self.directory.path.join("startup.backup")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
            Ok(_) => Ok(false),
        }
    }

    pub fn publish_startup_evidence(&self, bytes: &[u8]) -> io::Result<()> {
        self.publish_named_evidence("startup.evidence", bytes)
    }

    pub fn read_startup_evidence(&self) -> io::Result<Vec<u8>> {
        self.read_named_evidence("startup.evidence")
    }

    pub fn publish_pair_evidence(&self, bytes: &[u8], restoration: bool) -> io::Result<()> {
        self.publish_named_evidence(
            if restoration {
                "restoration.evidence"
            } else {
                "publication.evidence"
            },
            bytes,
        )
    }

    pub fn require_pair_evidence_absent(&self, restoration: bool) -> io::Result<()> {
        let name = if restoration {
            "restoration.evidence"
        } else {
            "publication.evidence"
        };
        match std::fs::symlink_metadata(self.directory.path.join(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(io::Error::other("pair evidence exists or is unreadable")),
        }
    }

    pub fn read_pair_evidence(&self, restoration: bool) -> io::Result<Vec<u8>> {
        self.read_named_evidence(if restoration {
            "restoration.evidence"
        } else {
            "publication.evidence"
        })
    }
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.directory.identity()
    }
    pub fn files(&self) -> (&File, &File) {
        (&self.controller, &self.cli)
    }

    /// Write-once opaque evidence, not a platform interpretation of success.
    pub fn publish_evidence(&self, bytes: &[u8]) -> io::Result<()> {
        self.publish_named_evidence("backup.evidence", bytes)
    }

    fn publish_named_evidence(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid backup evidence size",
            ));
        }
        let path = self.directory.path.join(name);
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
        self.read_named_evidence("backup.evidence")
    }

    fn read_named_evidence(&self, name: &str) -> io::Result<Vec<u8>> {
        let mut file =
            crate::windows_update_store::open_record(&self.directory.path.join(name), false)?;
        let mut bytes = Vec::new();
        (&mut file).take(4097).read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::other("invalid backup evidence size"));
        }
        Ok(bytes)
    }
}
