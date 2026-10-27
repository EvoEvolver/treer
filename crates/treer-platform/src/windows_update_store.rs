//! Windows private, single-record publication for the future native updater.
//! This stores bounded opaque bytes, not an authenticated update receipt. It
//! does not interpret phases, activate images, recover a job, or enable updates.
//! One OS account remains one trust domain. No existing ACL is repaired here.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const RECORD: &str = "receipt.bin";
const NEXT: &str = "receipt.next";
const LOCK: &str = "receipt.lock";
const LIMIT: u64 = 64 * 1024;
const TRANSFER: &str = "worker-transfer.bin";

pub enum PairPhase {
    Started,
    Published,
    Refused,
}
impl PairPhase {
    fn name(&self) -> &'static str {
        match self {
            Self::Started => "worker-pair-started.bin",
            Self::Published => "worker-pair-published.bin",
            Self::Refused => "worker-pair-refused.bin",
        }
    }
}

/// SAME held immutable transfer record. Its bytes alone grant no authority.
pub struct TransferRecord {
    file: File,
    expected: Vec<u8>,
}
impl TransferRecord {
    pub fn verify(&self, expected: &[u8]) -> io::Result<()> {
        if expected != self.expected {
            return Err(refused());
        }
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))?;
        let mut raw = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut raw)?;
        if raw != expected {
            return Err(refused());
        }
        Ok(())
    }
}

fn read_transfer(directory: &Path, expected: &[u8]) -> io::Result<TransferRecord> {
    if expected.is_empty() || expected.len() as u64 > LIMIT {
        return Err(refused());
    }
    // Receiver retains its SAME original object, whose sharing denies all new
    // writers/deleters. This observer must share WRITE to coexist with that
    // object's original write access; it has no write access of its own.
    let path = directory.join(TRANSFER);
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)?;
    checked_file(&path, &file)?;
    let record = TransferRecord {
        file,
        expected: expected.into(),
    };
    record.verify(expected)?;
    Ok(record)
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe Windows update store",
    )
}

/// Owns non-delete-sharing handles for every physical ancestor and an exclusive
/// stable lock file. A second writer cannot open the same lock, even after a
/// pathname alias; a leftover next file is unresolved, never auto-adopted.
pub struct UpdateStore {
    path: PathBuf,
    _parents: crate::machine_file::Root,
    _lock: File,
}

pub(super) fn checked_file(path: &Path, file: &File) -> io::Result<()> {
    let (_, _, links, attributes) = crate::windows::transfer_file_identity(file)?;
    if links != 1
        || attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
        || !crate::private_path_is_secure(path)?
    {
        return Err(refused());
    }
    Ok(())
}

pub(super) fn open_record(path: &Path, write: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(if write { 0 } else { FILE_SHARE_READ })
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    checked_file(path, &file)?;
    Ok(file)
}

pub(super) fn exclusive_lock(path: &Path) -> io::Result<File> {
    let lock = match open_record(path, true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            crate::windows::create_private_update_file(path)?
        }
        Err(error) => return Err(error),
    };
    checked_file(path, &lock)?;
    Ok(lock)
}

impl UpdateStore {
    /// Fixed read-only reconciliation under this original holder. Never adopt.
    pub fn read_transfer(&self, expected: &[u8]) -> io::Result<TransferRecord> {
        read_transfer(&self.path, expected)
    }
    pub fn directory_path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> io::Result<(u64, u64)> {
        self._parents.identity()
    }

    pub fn lock_identity(&self) -> io::Result<(u64, u64)> {
        crate::windows_update_inputs::lock_identity(&self._lock)
    }

    /// Duplicate only into the ORIGINAL owned Child, never a PID lookup. The
    /// new remote reference has no write/delete rights. Its numeric value is
    /// local to that child and carries no operation authority. On any later
    /// unknown exchange it remains until child exit: remotely closing it could
    /// close a reused handle after the receiver has run. Sender keeps all pins.
    #[allow(unsafe_code)]
    pub fn share_lock_with_child(&self, child: &std::process::Child) -> io::Result<u64> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::{DuplicateHandle, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
            Storage::FileSystem::{FILE_READ_ATTRIBUTES, SYNCHRONIZE},
            System::Threading::{GetCurrentProcess, WaitForSingleObject},
        };
        if unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return Err(refused());
        }
        let mut remote = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self._lock.as_raw_handle(),
                child.as_raw_handle(),
                &mut remote,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                0,
                0,
            )
        } == 0
            || remote.is_null()
            || remote == INVALID_HANDLE_VALUE
        {
            return Err(refused());
        }
        Ok(remote as usize as u64)
    }

    /// Existing physical directory only. The owner explicitly creates/seals a
    /// fresh job directory beforehand; opening never chmods or adopts a weak ACL.
    pub fn open(path: &Path) -> io::Result<Self> {
        let parents = crate::machine_file::Root::open(path, &[])?;
        if !crate::private_path_is_secure(path)? {
            return Err(refused());
        }
        crate::windows::require_update_ntfs(path)?;
        let lock_path = path.join(LOCK);
        let lock = exclusive_lock(&lock_path)?;
        // Do not mistake a crash, rejected rename, symlink, or unreadable stage
        // for permission to retry publication. Keep all names for reconciliation.
        match std::fs::symlink_metadata(path.join(NEXT)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(refused()),
        }
        Ok(Self {
            path: path.into(),
            _parents: parents,
            _lock: lock,
        })
    }

    pub fn read(&self) -> io::Result<Vec<u8>> {
        let mut file = open_record(&self.path.join(RECORD), false)?;
        let length = file.metadata()?.len();
        if length == 0 || length > LIMIT {
            return Err(refused());
        }
        let mut bytes = Vec::new();
        (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length || file.metadata()?.len() != length {
            return Err(refused());
        }
        Ok(bytes)
    }

    /// Flush a new private file, then publish by same-directory Win32 rename.
    /// Existing records must already be private, regular and single-link.
    /// Errors retain the old record and/or exclusive next file; never retry this
    /// call after an unknown result. Atomic visibility is not a power-loss claim.
    pub fn publish(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() as u64 > LIMIT {
            return Err(refused());
        }
        let target = self.path.join(RECORD);
        let exists = match self.read() {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let next = self.path.join(NEXT);
        let mut file = crate::windows::create_private_update_file(&next)?;
        checked_file(&next, &file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        crate::windows::publish_transfer_file(&next, &target, exists)?;
        if self.read()? != bytes {
            return Err(refused());
        }
        Ok(())
    }
}

/// Independent receiver guards for a fixed job directory, original exclusive
/// lock object and pinned intent. No publish, recovery or sender-release API.
pub struct ReceivedJob {
    _parents: crate::machine_file::Root,
    _lock: File,
    receipt: File,
    expected: Vec<u8>,
    path: PathBuf,
}

impl ReceivedJob {
    /// Fixed EXCL append-only phase names. Bytes are evidence, never a resume
    /// constructor or permission. Original intent and job lock remain held.
    pub fn record_pair_phase(&self, phase: PairPhase, bytes: &[u8]) -> io::Result<TransferRecord> {
        self.record_once(phase.name(), bytes)
    }
    /// One fixed CREATE_NEW/flush/readback record under retained original job
    /// parents and lock. Partial/error names remain; no overwrite or retry.
    pub fn record_transfer(&self, bytes: &[u8]) -> io::Result<TransferRecord> {
        self.record_once(TRANSFER, bytes)
    }
    fn record_once(&self, name: &str, bytes: &[u8]) -> io::Result<TransferRecord> {
        self.verify(&self.expected)?;
        if bytes.is_empty() || bytes.len() as u64 > LIMIT {
            return Err(refused());
        }
        let path = self.path.join(name);
        let mut file = crate::windows::create_private_transfer_record(&path)?;
        checked_file(&path, &file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        file.seek(SeekFrom::Start(0))?;
        let mut observed = Vec::new();
        (&mut file).take(LIMIT + 1).read_to_end(&mut observed)?;
        if observed != bytes {
            return Err(refused());
        }
        let record = TransferRecord {
            file: crate::windows::readonly_update_file(file)?,
            expected: bytes.into(),
        };
        record.verify(bytes)?;
        Ok(record)
    }
    /// Creator connection and independently authenticated expected intent are
    /// prerequisites. Borrow the remote handle value, duplicate locally with
    /// reduced rights, never adopt/close an arbitrary numeric source handle.
    #[allow(unsafe_code)]
    pub fn retain_from_creator(
        creator: &crate::windows_update_pipe::CreationParentConnection,
        directory: &Path,
        expected_directory: (u64, u64),
        expected_lock: (u64, u64),
        remote: u64,
        expected: &[u8],
    ) -> io::Result<Self> {
        if expected.is_empty() || expected.len() as u64 > LIMIT || !creator.creator_is_live()? {
            return Err(refused());
        }
        let remote = usize::try_from(remote).map_err(|_| refused())?;
        if remote == 0 || remote == usize::MAX {
            return Err(refused());
        }
        let parents = crate::machine_file::Root::open(directory, &[])?;
        if parents.identity()? != expected_directory
            || expected_lock.0 != expected_directory.0
            || !crate::private_path_is_secure(directory)?
        {
            return Err(refused());
        }
        crate::windows::require_update_ntfs(directory)?;
        let lock = File::from(crate::windows_update_inputs::duplicate_lock(
            remote as *mut _,
        )?);
        checked_file(&directory.join(LOCK), &lock)?;
        if crate::windows_update_inputs::lock_identity(&lock)? != expected_lock {
            return Err(refused());
        }
        let receipt = open_record(&directory.join(RECORD), false)?;
        if crate::transfer_file_identity(&receipt)?.0 != expected_directory.0 {
            return Err(refused());
        }
        let received = Self {
            _parents: parents,
            _lock: lock,
            receipt,
            expected: expected.to_vec(),
            path: directory.into(),
        };
        received.verify(expected)?;
        if !creator.creator_is_live()? {
            return Err(refused());
        }
        Ok(received)
    }

    /// Compare full bytes from the SAME read-only, non-write/delete-sharing
    /// receipt. A successful comparison is not a durable takeover record.
    pub fn verify(&self, expected: &[u8]) -> io::Result<()> {
        if expected != self.expected || expected.is_empty() || expected.len() as u64 > LIMIT {
            return Err(refused());
        }
        let mut file = &self.receipt;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes != expected {
            return Err(refused());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "treer-update-store-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            crate::secure_directory(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn windows_update_store_private_atomic_publication_and_writer_exclusion() {
        let root = Fixture::new();
        let store = UpdateStore::open(&root.0).unwrap();
        assert!(UpdateStore::open(&root.0).is_err());
        store.publish(b"old receipt").unwrap();
        assert!(crate::private_path_is_secure(&root.0.join(RECORD)).unwrap());
        store.publish(b"next receipt").unwrap();
        assert_eq!(store.read().unwrap(), b"next receipt");
        assert!(!root.0.join(NEXT).exists());
        drop(store);
        assert_eq!(
            UpdateStore::open(&root.0).unwrap().read().unwrap(),
            b"next receipt"
        );
    }

    #[test]
    fn windows_update_store_failed_publication_retains_old_and_unresolved_next() {
        let root = Fixture::new();
        let store = UpdateStore::open(&root.0).unwrap();
        store.publish(b"old").unwrap();
        let reader = open_record(&root.0.join(RECORD), false).unwrap();
        assert!(store.publish(b"new").is_err()); // no FILE_SHARE_DELETE
        drop(reader);
        assert_eq!(store.read().unwrap(), b"old");
        assert_eq!(std::fs::read(root.0.join(NEXT)).unwrap(), b"new");
        drop(store);
        assert!(UpdateStore::open(&root.0).is_err());
    }

    #[test]
    fn windows_update_store_refuses_weak_acl_and_hardlinks_without_repair() {
        let root = Fixture::new();
        crate::apply_sddl_dacl(&root.0, "D:P(A;;GA;;;WD)").unwrap();
        assert!(UpdateStore::open(&root.0).is_err());
        assert!(!root.0.join(LOCK).exists());
        crate::secure_directory(&root.0).unwrap();
        let store = UpdateStore::open(&root.0).unwrap();
        store.publish(b"original").unwrap();
        std::fs::hard_link(root.0.join(RECORD), root.0.join("alias")).unwrap();
        assert!(store.read().is_err());
        assert!(store.publish(b"new").is_err());
        assert!(!root.0.join(NEXT).exists());
    }

    #[test]
    fn windows_update_store_refuses_bounds_and_existing_stage_before_mutation() {
        let root = Fixture::new();
        let store = UpdateStore::open(&root.0).unwrap();
        assert!(store.publish(b"").is_err());
        assert!(store.publish(&vec![0; LIMIT as usize + 1]).is_err());
        assert!(!root.0.join(NEXT).exists());
        std::fs::write(root.0.join(NEXT), b"unresolved").unwrap();
        assert!(store.publish(b"new").is_err());
        assert_eq!(std::fs::read(root.0.join(NEXT)).unwrap(), b"unresolved");
        assert!(!root.0.join(RECORD).exists());
    }
}
