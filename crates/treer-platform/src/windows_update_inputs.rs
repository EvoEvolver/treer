//! Read-only, physically pinned input pair for Windows update admission.
//! No signature, update phase, process control or image publication occurs here.
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// Retains every physical ancestor and read-only file handles without write or
/// delete sharing. The caller authenticates contents; no path is exposed for
/// reopening or later execution. This is not an installation mutation lock.
pub struct PinnedPair {
    root: crate::machine_file::Root,
    controller: File,
    cli: File,
    path: PathBuf,
}

impl PinnedPair {
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
    pair: PinnedPair,
    _lock: File,
}

impl LockedInstallation {
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.pair.identity()
    }

    pub fn files(&self) -> (&File, &File) {
        self.pair.files()
    }
}
