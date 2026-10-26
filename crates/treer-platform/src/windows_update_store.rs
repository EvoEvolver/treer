//! Windows private, single-record publication for the future native updater.
//! This stores bounded opaque bytes, not an authenticated update receipt. It
//! does not interpret phases, activate images, recover a job, or enable updates.
//! One OS account remains one trust domain. No existing ACL is repaired here.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ,
};

const RECORD: &str = "receipt.bin";
const NEXT: &str = "receipt.next";
const LOCK: &str = "receipt.lock";
const LIMIT: u64 = 64 * 1024;

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
