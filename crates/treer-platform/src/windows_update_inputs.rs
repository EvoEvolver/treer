//! Read-only, physically pinned input pair for Windows update admission.
//! No signature, update phase, process control or file publication occurs here.
use std::{fs::File, io, path::Path};

/// Retains every physical ancestor and read-only file handles without write or
/// delete sharing. The caller authenticates contents; no path is exposed for
/// reopening or later execution. This is not an installation mutation lock.
pub struct PinnedPair {
    root: crate::machine_file::Root,
    controller: File,
    cli: File,
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
        })
    }

    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self.root.identity()
    }

    pub fn files(&self) -> (&File, &File) {
        (&self.controller, &self.cli)
    }
}
