//! Read-only, physically pinned input pair for Windows update admission.
//! No signature, update phase or image publication occurs here. Pinned spawning
//! is mechanics only: the Controller must authenticate bytes and arguments.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// Retains every physical ancestor and read-only file handles without write or
/// delete sharing. The caller authenticates contents; no path is exposed for
/// arbitrary reopening. This is not an installation mutation lock. The spawn
/// primitive retains these same pins while Windows opens the fixed image path.
pub struct PinnedPair {
    pub(super) root: crate::machine_file::Root,
    pub(super) controller: File,
    pub(super) cli: File,
    pub(super) path: PathBuf,
}

impl PinnedPair {
    /// Mechanical creation from the fixed pinned controller name only. No PATH
    /// search, inherited environment, shell or exposed Command override. The
    /// caller authenticates the SAME held bytes and selects restricted argv;
    /// neither this function nor a successful Child implies worker readiness.
    pub fn spawn_pinned_controller(
        &self,
        args: &[std::ffi::OsString],
    ) -> io::Result<std::process::Child> {
        let path = self.path.join("treer-agent-server.exe");
        let reopened = crate::windows_update_store::open_record(&path, false)?;
        if crate::transfer_file_identity(&reopened)?
            != crate::transfer_file_identity(&self.controller)?
        {
            return Err(io::Error::other("Windows launch image changed"));
        }
        std::process::Command::new(path)
            .args(args)
            .current_dir(&self.path)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    }

    /// Existing private fixed-NTFS directories only. Never repairs an ACL or
    /// creates a missing name. Installed and staged pairs use the same names.
    pub fn open(path: &Path) -> io::Result<Self> {
        let root = crate::machine_file::Root::open(path, &[])?;
        if !crate::private_path_is_secure(path)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe update inputs",
            ));
        }
        crate::windows::require_update_ntfs(path)?;
        let controller =
            crate::windows_update_store::open_record(&path.join("treer-agent-server.exe"), false)?;
        let cli = crate::windows_update_store::open_record(&path.join("treer.exe"), false)?;
        Ok(Self {
            root,
            controller,
            cli,
            path: path.into(),
        })
    }

    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.root.identity()
    }

    pub fn files(&self) -> (&File, &File) {
        (&self.controller, &self.cli)
    }

    /// Consume the pinned installation and hold its stable, no-sharing lock.
    /// This serializes participating updaters across distinct task directories.
    /// Never removes/recreates an existing lock or repairs its ACL. The lock
    /// carries no operation record: owner exit is NOT recovery/retry authority.
    pub fn lock_installation(self) -> io::Result<LockedInstallation> {
        let lock = crate::windows_update_store::exclusive_lock(
            &self.path.join(".treer-controller-update.lock"),
        )?;
        Ok(LockedInstallation {
            pair: self,
            _lock: lock,
        })
    }
}

/// The installation pair cannot be detached from its exclusive lock. Releasing
/// this value releases the lease, never deletes its stable name. This is not a
/// durable crash fence or protection from nonparticipating same-account code.
pub struct LockedInstallation {
    pub(super) pair: PinnedPair,
    pub(super) _lock: File,
}

impl LockedInstallation {
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.pair.identity()
    }

    pub fn files(&self) -> (&File, &File) {
        self.pair.files()
    }

    /// Publish a write-once, bounded opaque installation claim while the lease
    /// and physical parents remain held. Existing names of ANY kind refuse,
    /// even identical bytes. Write/flush/readback errors retain partial evidence;
    /// there is deliberately no remove, overwrite, resume or retry operation.
    /// The Controller supplies the authenticated binding/schema, not this layer.
    pub fn claim(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid update claim size",
            ));
        }
        let path = self.pair.path.join(".treer-controller-update.claim");
        let mut file = crate::windows::create_private_update_file(&path)?;
        crate::windows_update_store::checked_file(&path, &file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let mut observed = Vec::new();
        (&mut file).take(4097).read_to_end(&mut observed)?;
        if observed != bytes {
            return Err(io::Error::other("update claim readback differs"));
        }
        Ok(())
    }

    /// Read-only observation, never adoption or permission to resume. Only a
    /// private regular single-link claim is readable; invalid/partial bytes
    /// still require Controller schema and exact binding validation.
    pub fn read_claim(&self) -> io::Result<Vec<u8>> {
        let mut file = crate::windows_update_store::open_record(
            &self.pair.path.join(".treer-controller-update.claim"),
            false,
        )?;
        let mut bytes = Vec::new();
        (&mut file).take(4097).read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(io::Error::other("invalid update claim size"));
        }
        Ok(bytes)
    }
}
