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

/// Physical observations, never content authentication or operation authority.
/// A receiver must obtain these through its separately authenticated handoff.
/// Directory privacy, volume equality and a matching tuple do not prove runtime
/// configuration, signatures, Policy, or permission to release the sender pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairIdentity {
    pub directory: (u64, u64),
    pub controller: (u64, u64),
    pub cli: (u64, u64),
}

pub struct ImageParents {
    _root: crate::machine_file::Root,
}

/// Independently retained fixed claim, not ownership, recovery or a release
/// permit. The Controller authenticates expected bytes before construction.
pub struct ClaimPin {
    file: File,
    expected: Vec<u8>,
}

impl ClaimPin {
    /// Read the SAME held file, never reopen a pathname or replace its bytes.
    pub fn verify(&self, expected: &[u8]) -> io::Result<()> {
        if expected != self.expected || expected.is_empty() || expected.len() > 4096 {
            return Err(io::Error::other("Windows update claim unconfirmed"));
        }
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes != self.expected {
            return Err(io::Error::other("Windows update claim unconfirmed"));
        }
        Ok(())
    }
}

impl PinnedPair {
    pub fn into_image_parents(self) -> ImageParents {
        ImageParents { _root: self.root }
    }
    /// Retain the fixed existing private single-link claim under these pinned
    /// ancestors. No create/adoption/write/cleanup API; no sender release.
    pub fn retain_claim(&self, expected: &[u8]) -> io::Result<ClaimPin> {
        if expected.is_empty() || expected.len() > 4096 {
            return Err(io::Error::other("Windows update claim unconfirmed"));
        }
        let file = crate::windows_update_store::open_record(
            &self.path.join(".treer-controller-update.claim"),
            false,
        )?;
        if lock_identity(&file)?.0 != self.identity()?.0 {
            return Err(io::Error::other("Windows update claim volume differs"));
        }
        let pin = ClaimPin {
            file,
            expected: expected.to_vec(),
        };
        pin.verify(expected)?;
        Ok(pin)
    }
    /// Locator data for a separately authenticated receiver. The caller must
    /// keep these pins held while the receiver opens and compares every ID;
    /// the pathname alone is neither custody nor operation authorization.
    pub fn directory_path(&self) -> &Path {
        &self.path
    }
    /// Mechanical creation from the fixed pinned controller name only. No PATH
    /// search, inherited environment, shell or exposed Command override. The
    /// caller authenticates the SAME held bytes and selects restricted argv;
    /// neither this function nor a successful Child implies worker readiness.
    pub fn spawn_pinned_controller(
        &self,
        installation: &LockedInstallation,
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
            // The fixed private worker may authenticate its pipe server using
            // this explicit creator handle, not an ambient PID/environment.
            .stdin(crate::windows_update_pipe::creator_process_stdio()?)
            .stdout(std::process::Stdio::null())
            // Bootstrap-only stderr carries a reduced-rights duplicate of the
            // SAME installation lock object, never a diagnostic byte stream.
            // Child exit releases this inherited reference; ACK does not.
            .stderr(installation.worker_lock_stdio()?)
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

    /// Observe the SAME held directory and fixed image handles. No pathname
    /// lookup or cursor movement is involved. This does not hash file contents.
    pub fn pair_identity(&self) -> io::Result<PairIdentity> {
        Ok(PairIdentity {
            directory: self.root.identity()?,
            controller: lock_identity(&self.controller)?,
            cli: lock_identity(&self.cli)?,
        })
    }

    /// Independently retain all physical ancestors and both read-only images,
    /// then compare exact identities. The sender must still hold its original
    /// pins throughout this call: observations alone cannot close a release /
    /// reopen gap. New handles have independent read offsets. On refusal only
    /// these newly opened handles are dropped; no file, ACL, lock or claim is
    /// changed. This primitive does not adopt an installation lock or operation.
    pub fn open_matching(path: &Path, expected: PairIdentity) -> io::Result<Self> {
        if expected.directory.1 == 0
            || expected.controller.1 == 0
            || expected.cli.1 == 0
            || expected.controller.0 != expected.directory.0
            || expected.cli.0 != expected.directory.0
            || expected.controller == expected.cli
        {
            return Err(io::Error::other("windows_update_pair_identity_refused"));
        }
        let pair = Self::open(path)?;
        if pair.pair_identity()? != expected {
            return Err(io::Error::other("windows_update_pair_identity_refused"));
        }
        Ok(pair)
    }

    pub fn files(&self) -> (&File, &File) {
        (&self.controller, &self.cli)
    }

    /// Re-pin this SAME held pair without exporting a path. The borrow keeps
    /// sender pins alive throughout independent opening and identity comparison.
    pub fn repin(&self) -> io::Result<Self> {
        Self::open_matching(&self.path, self.pair_identity()?)
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

/// Retains physical parents and the ORIGINAL lock after consuming image pins.
/// Mechanical separation only; the caller must authorize custody transfer.
pub struct InstallationLease {
    _root: crate::machine_file::Root,
    _lock: File,
}

impl LockedInstallation {
    pub fn into_lease(self) -> InstallationLease {
        let PinnedPair {
            root,
            controller,
            cli,
            path: _,
        } = self.pair;
        drop((controller, cli));
        InstallationLease {
            _root: root,
            _lock: self._lock,
        }
    }
    /// Retain an independent read-only image pair, never adopt/duplicate its
    /// installation lease or claim. Caller must retain this original owner.
    pub fn repin_images(&self) -> io::Result<PinnedPair> {
        self.pair.repin()
    }

    /// Physical identity of the held lock FILE, not its installation directory.
    /// This observation alone never authorizes adoption of an operation.
    pub fn lock_file_identity(&self) -> io::Result<(u64, u64)> {
        lock_identity(&self._lock)
    }

    /// Explicit bootstrap-only standard error slot. The duplicate has only
    /// read-attributes/synchronize rights, but retains the SAME no-sharing file
    /// object. No raw handle value, inheritable ambient handle, pathname reopen
    /// or lock replacement is involved. Do not use this slot for logging.
    pub fn worker_lock_stdio(&self) -> io::Result<std::process::Stdio> {
        use std::os::windows::io::AsRawHandle;
        Ok(std::process::Stdio::from(duplicate_lock(
            self._lock.as_raw_handle(),
        )?))
    }

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

#[allow(unsafe_code)] // Exact OS-handle duplication; no raw ownership is exposed.
pub(super) fn duplicate_lock(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::{
        Foundation::{DuplicateHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{FILE_READ_ATTRIBUTES, SYNCHRONIZE},
        System::Threading::GetCurrentProcess,
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other("windows_update_lock_unconfirmed"));
    }
    let mut result = std::ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut result,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            0,
            0,
        )
    } == 0
        || result.is_null()
        || result == INVALID_HANDLE_VALUE
    {
        return Err(io::Error::other("windows_update_lock_unconfirmed"));
    }
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(result) })
}

pub(super) fn lock_identity(file: &File) -> io::Result<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let (volume, id, links, attributes) = crate::transfer_file_identity(file)?;
    if id == 0
        || links != 1
        || attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err(io::Error::other("windows_update_lock_unconfirmed"));
    }
    Ok((volume, id))
}

/// Child-side custody of the original creator's explicit lock-file reference.
/// No Clone/Deserialize/raw-handle constructor. This is not a LockedInstallation:
/// image/ancestor/state pins and independently admitted operation authority have
/// NOT been transferred. The borrowed standard handle also remains inherited
/// until process exit, even when this extra retained reference is dropped.
pub struct InheritedInstallationLock {
    pub(super) _file: File,
}

impl InheritedInstallationLock {
    /// Requires an already verified live original-creator connection. Expected
    /// lock identity must come from future admitted handoff, not an untrusted
    /// path/JSON claim. This method checks mechanical custody only, not Policy.
    #[allow(unsafe_code)] // Borrow, never close, the bootstrap standard handle.
    pub fn retain_from_creator(
        creator: &crate::windows_update_pipe::CreationParentConnection,
        expected: (u64, u64),
    ) -> io::Result<Self> {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        if !creator.creator_is_live()? {
            return Err(io::Error::other("windows_update_lock_unconfirmed"));
        }
        // Borrow stderr; do not close a standard handle owned by runtime.
        let file = File::from(duplicate_lock(unsafe { GetStdHandle(STD_ERROR_HANDLE) })?);
        if lock_identity(&file)? != expected || !creator.creator_is_live()? {
            return Err(io::Error::other("windows_update_lock_unconfirmed"));
        }
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
#[path = "windows_update_inputs_tests.rs"]
mod tests;
