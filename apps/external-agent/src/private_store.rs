// Adapted from message-endpoint private_store.rs (#217, 5e0cdce4).
//! Fail-closed Unix storage boundary, not isolation against another same-UID process.
use crate::Error;
use rusqlite::{Connection, OpenFlags};
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use std::path::{Component, Path};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) struct PrivateFiles {
    // Lock is kept alive through Connection::drop (Store declares db first).
    _lock: OwnedFd,
    _directory: OwnedFd,
    database_file: Option<OwnedFd>,
    identity_failed: Arc<AtomicBool>,
}

impl Drop for PrivateFiles {
    fn drop(&mut self) {
        // CLOEXEC still permits a fork-before-exec inheritance window. Closing
        // only our descriptor leaves flock held by an inherited open file
        // description. Explicit unlock releases that description immediately,
        // after Store's earlier-declared SQLite connection has been dropped.
        // Close our pinned DB descriptor before allowing another cooperating
        // store to open; its Connection has already dropped.
        drop(self.database_file.take());
        let _ = fs::flock(&self._lock, FlockOperation::Unlock);
    }
}

fn denied<T, E>(result: Result<T, E>) -> Result<T, Error> {
    result.map_err(|_| Error::PrivateStorage)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn vcs_free(directory: &OwnedFd) -> Result<(), Error> {
    for marker in [".git", ".hg", ".svn"] {
        match fs::statat(directory, marker, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {}
            _ => return Err(Error::PrivateStorage),
        }
    }
    Ok(())
}

fn check_directory(directory: &OwnedFd, leaf: bool) -> Result<(), Error> {
    let metadata = denied(fs::fstat(directory))?;
    let uid = rustix::process::geteuid().as_raw();
    let mode = metadata.st_mode;
    if FileType::from_raw_mode(mode) != FileType::Directory
        || (metadata.st_uid != 0 && metadata.st_uid != uid)
        || (mode & 0o022 != 0 && mode & 0o1000 == 0)
        || (leaf && (metadata.st_uid != uid || mode & 0o7777 != 0o700))
    {
        return Err(Error::PrivateStorage);
    }
    vcs_free(directory)
}

pub(crate) fn open_directory(path: &Path, create_leaf: bool) -> Result<OwnedFd, Error> {
    if !path.is_absolute() {
        return Err(Error::PrivateStorage);
    }
    let components: Vec<_> = path.components().collect();
    if components.len() < 2
        || components[1..]
            .iter()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::PrivateStorage);
    }
    let mut directory = denied(fs::open("/", directory_flags(), Mode::empty()))?;
    check_directory(&directory, false)?;
    for (index, component) in components[1..].iter().enumerate() {
        let leaf = index == components.len() - 2;
        let name = component.as_os_str();
        let next = match fs::openat(&directory, name, directory_flags(), Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) if leaf && create_leaf => {
                // Only the final dedicated state directory may be newly created.
                denied(fs::mkdirat(&directory, name, Mode::from_raw_mode(0o700)))?;
                denied(fs::fsync(&directory))?;
                denied(fs::openat(
                    &directory,
                    name,
                    directory_flags(),
                    Mode::empty(),
                ))?
            }
            Err(_) => return Err(Error::PrivateStorage),
        };
        check_directory(&next, leaf)?;
        directory = next;
    }
    Ok(directory)
}

fn private_file(directory: &OwnedFd, name: &str) -> Result<OwnedFd, Error> {
    let common =
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY;
    let file = match fs::openat(
        directory,
        name,
        common | OFlags::CREATE | OFlags::EXCL,
        Mode::from_raw_mode(0o600),
    ) {
        Ok(file) => file,
        Err(Errno::EXIST) => denied(fs::openat(directory, name, common, Mode::empty()))?,
        Err(_) => return Err(Error::PrivateStorage),
    };
    check_file(&file)?;
    Ok(file)
}

pub(crate) fn check_file(file: &OwnedFd) -> Result<(), Error> {
    let metadata = denied(fs::fstat(file))?;
    check_file_metadata(&metadata)
}

fn check_file_metadata(metadata: &fs::Stat) -> Result<(), Error> {
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_uid != rustix::process::geteuid().as_raw()
        || metadata.st_mode & 0o7777 != 0o600
        || metadata.st_nlink != 1
    {
        return Err(Error::PrivateStorage);
    }
    Ok(())
}

pub(crate) fn open(path: &Path) -> Result<(Connection, PrivateFiles), Error> {
    let directory = open_directory(path, true)?;
    let lock = private_file(&directory, "external-agent.lock")?;
    fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == Errno::WOULDBLOCK {
            Error::Busy
        } else {
            Error::PrivateStorage
        }
    })?;
    let database = private_file(&directory, "external-agent.sqlite3")?;
    // Retain this validated inode until AFTER SQLite closes. Never open/close
    // additional database descriptors while SQLite owns its record locks.
    for name in [
        "external-agent.sqlite3-wal",
        "external-agent.sqlite3-shm",
        "external-agent.sqlite3-journal",
    ] {
        // Existing sidecars are checked before SQLite can follow or modify them.
        // Empty files are safe SQLite initial state. Never truncate existing data.
        private_file(&directory, name)?;
    }
    let files = PrivateFiles {
        _lock: lock,
        _directory: directory,
        database_file: Some(database),
        identity_failed: Arc::new(AtomicBool::new(false)),
    };
    let db = denied(Connection::open_with_flags(
        path.join("external-agent.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ))?;
    recheck(&files)?;
    let identity = admission_identity(&files)?;
    // Safe hook: no SQL or transient database descriptors. Returning true
    // rolls back any transaction whose configured pathname has drifted,
    // including worker writes that do not pass through HTTP admission.
    db.commit_hook(Some(move || identity.check().is_err()));
    // Retain directory and lock descriptors. Hostile same-UID code can replace
    // either paths or locks; preventing that requires a separate OS account.
    Ok((db, files))
}

pub(crate) fn enable_wal(db: &Connection, path: &Path) -> Result<(), Error> {
    let mode: String = denied(db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)))?;
    if mode != "wal" {
        return Err(Error::PrivateStorage);
    }
    denied(db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"))?;
    let directory = open_directory(path, false)?;
    for name in [
        "external-agent.sqlite3",
        "external-agent.sqlite3-wal",
        "external-agent.sqlite3-shm",
    ] {
        let metadata = denied(fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW))?;
        check_file_metadata(&metadata)?;
    }
    denied(fs::fsync(&directory))?;
    Ok(())
}

/// Admission and commit checks share only a directory descriptor and immutable
/// identity; they never duplicate or close the pinned database descriptor.
pub(crate) struct AdmissionIdentity {
    directory: OwnedFd,
    expected: fs::Stat,
    failed: Arc<AtomicBool>,
}
impl AdmissionIdentity {
    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(Error::PrivateStorage);
        }
        let result = (|| {
            check_directory(&self.directory, true)?;
            let actual = denied(fs::statat(
                &self.directory,
                "external-agent.sqlite3",
                AtFlags::SYMLINK_NOFOLLOW,
            ))?;
            check_file_metadata(&actual)?;
            if actual.st_dev != self.expected.st_dev || actual.st_ino != self.expected.st_ino {
                return Err(Error::PrivateStorage);
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
        result
    }
}
pub(crate) fn admission_identity(files: &PrivateFiles) -> Result<AdmissionIdentity, Error> {
    recheck(files)?;
    Ok(AdmissionIdentity {
        directory: denied(files._directory.try_clone())?,
        expected: denied(fs::fstat(
            files.database_file.as_ref().ok_or(Error::PrivateStorage)?,
        ))?,
        failed: files.identity_failed.clone(),
    })
}

fn check_database_identity(files: &PrivateFiles) -> Result<(), Error> {
    if files.identity_failed.load(Ordering::Relaxed) {
        return Err(Error::PrivateStorage);
    }
    let result = (|| {
        let metadata = denied(fs::statat(
            &files._directory,
            "external-agent.sqlite3",
            AtFlags::SYMLINK_NOFOLLOW,
        ))?;
        check_file_metadata(&metadata)?;
        let pinned = denied(fs::fstat(
            files.database_file.as_ref().ok_or(Error::PrivateStorage)?,
        ))?;
        check_file_metadata(&pinned)?;
        if metadata.st_dev != pinned.st_dev || metadata.st_ino != pinned.st_ino {
            return Err(Error::PrivateStorage);
        }
        Ok(())
    })();
    if result.is_err() {
        // Restoring the pathname must not silently re-enable this connection.
        // Operator recovery requires stopping and reopening the App.
        files.identity_failed.store(true, Ordering::Relaxed);
    }
    result
}

pub(crate) fn recheck(files: &PrivateFiles) -> Result<(), Error> {
    check_database_identity(files)?;
    check_directory(&files._directory, true)?;
    check_file(&files._lock)?;
    for name in [
        "external-agent.lock",
        "external-agent.sqlite3",
        "external-agent.sqlite3-wal",
        "external-agent.sqlite3-shm",
        "external-agent.sqlite3-journal",
    ] {
        // Never open/close a live SQLite file just to inspect permissions:
        // closing ANY descriptor for an inode releases this process's POSIX
        // record locks on that inode, including SQLite's database/SHM locks.
        match fs::statat(&files._directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => check_file_metadata(&metadata)?,
            Err(Errno::NOENT)
                if name.ends_with("-wal")
                    || name.ends_with("-shm")
                    || name.ends_with("-journal") => {}
            Err(_) => return Err(Error::PrivateStorage),
        }
    }
    Ok(())
}
