//! Fail-closed Unix storage boundary, not isolation against another same-UID process.
use crate::store::Error;
use rusqlite::{Connection, OpenFlags};
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use std::ffi::OsStr;
use std::path::{Component, Path};

pub(crate) struct PrivateFiles {
    // Lock is kept alive through Connection::drop (Store declares db first).
    _lock: OwnedFd,
    _directory: OwnedFd,
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

fn open_directory(path: &Path, create_leaf: bool) -> Result<OwnedFd, Error> {
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

fn check_file(file: &OwnedFd) -> Result<(), Error> {
    let metadata = denied(fs::fstat(file))?;
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
    let lock = private_file(&directory, "hub.lock")?;
    fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == Errno::WOULDBLOCK {
            Error::Busy
        } else {
            Error::PrivateStorage
        }
    })?;
    for name in [
        "hub.sqlite3",
        "hub.sqlite3-wal",
        "hub.sqlite3-shm",
        "hub.sqlite3-journal",
    ] {
        // Existing sidecars are checked before SQLite can follow or modify them.
        // Empty files are safe SQLite initial state. Never truncate existing data.
        private_file(&directory, name)?;
    }
    let db = denied(Connection::open_with_flags(
        path.join("hub.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    ))?;
    // Retain directory and lock descriptors. Hostile same-UID code can replace
    // either paths or locks; preventing that requires a separate OS account.
    Ok((
        db,
        PrivateFiles {
            _lock: lock,
            _directory: directory,
        },
    ))
}

pub(crate) fn enable_wal(db: &Connection, path: &Path) -> Result<(), Error> {
    let mode: String = denied(db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)))?;
    if mode != "wal" {
        return Err(Error::PrivateStorage);
    }
    denied(db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"))?;
    let directory = open_directory(path, false)?;
    for name in ["hub.sqlite3", "hub.sqlite3-wal", "hub.sqlite3-shm"] {
        let file = denied(fs::openat(
            &directory,
            OsStr::new(name),
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        ))?;
        check_file(&file)?;
    }
    denied(fs::fsync(&directory))?;
    Ok(())
}

pub(crate) fn recheck(files: &PrivateFiles) -> Result<(), Error> {
    check_directory(&files._directory, true)?;
    check_file(&files._lock)?;
    for name in [
        "hub.lock",
        "hub.sqlite3",
        "hub.sqlite3-wal",
        "hub.sqlite3-shm",
        "hub.sqlite3-journal",
    ] {
        match fs::openat(
            &files._directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        ) {
            Ok(file) => check_file(&file)?,
            Err(Errno::NOENT)
                if name.ends_with("-wal")
                    || name.ends_with("-shm")
                    || name.ends_with("-journal") => {}
            Err(_) => return Err(Error::PrivateStorage),
        }
    }
    Ok(())
}

/// Strict local provisioning input, never an HTTP upload or a hub-supplied file.
pub(crate) fn read_config(path: &Path) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let parent = path.parent().ok_or(Error::PrivateStorage)?;
    let name = path.file_name().ok_or(Error::PrivateStorage)?;
    let directory = open_directory(parent, false)?;
    let file = denied(fs::openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
        Mode::empty(),
    ))?;
    check_file(&file)?;
    let mut bytes = Vec::new();
    denied(
        std::fs::File::from(file)
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes),
    )?;
    if bytes.len() > 16 * 1024 {
        return Err(Error::PrivateStorage);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Limits, Store};
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::{env, fs as stdfs, process::Command};

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        // macOS /var and /tmp are symlinks; production requires explicit physical paths.
        let base = temporary.path().canonicalize().unwrap();
        stdfs::set_permissions(&base, stdfs::Permissions::from_mode(0o700)).unwrap();
        let path = base.join("state");
        (temporary, path)
    }

    #[test]
    fn private_files_persist_reopen_and_exclude_another_writer() {
        let (_temporary, path) = fixture();
        let mut store = Store::open(&path, Limits::default()).unwrap();
        store.register_paused("ag_test", 1, "ag_target").unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
        assert_eq!(stdfs::metadata(&path).unwrap().mode() & 0o7777, 0o700);
        for name in [
            "hub.lock",
            "hub.sqlite3",
            "hub.sqlite3-wal",
            "hub.sqlite3-shm",
        ] {
            let metadata = stdfs::metadata(path.join(name)).unwrap();
            assert_eq!(metadata.mode() & 0o7777, 0o600);
            assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        }
        drop(store);
        let mut reopened = Store::open(&path, Limits::default()).unwrap();
        assert!(reopened.register_paused("ag_test", 1, "ag_target").is_err());
    }

    #[test]
    fn periodic_recheck_rejects_changed_sidecar_permissions() {
        let (_temporary, path) = fixture();
        let store = Store::open(&path, Limits::default()).unwrap();
        store.recheck_private_storage().unwrap();
        let shm = path.join("hub.sqlite3-shm");
        stdfs::set_permissions(&shm, stdfs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.recheck_private_storage().is_err());
        // Detection must not silently repair the changed mode.
        assert_eq!(stdfs::metadata(shm).unwrap().mode() & 0o7777, 0o644);
    }

    #[test]
    fn reject_symlink_at_every_boundary_without_touching_target() {
        for name in [
            "hub.lock",
            "hub.sqlite3",
            "hub.sqlite3-wal",
            "hub.sqlite3-shm",
            "hub.sqlite3-journal",
        ] {
            let (_temporary, path) = fixture();
            stdfs::create_dir(&path).unwrap();
            stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o700)).unwrap();
            let target = path.parent().unwrap().join("unrelated");
            stdfs::write(&target, b"never change").unwrap();
            symlink(&target, path.join(name)).unwrap();
            assert!(Store::open(&path, Limits::default()).is_err(), "{name}");
            assert_eq!(stdfs::read(&target).unwrap(), b"never change");
        }
        let (_temporary, path) = fixture();
        let alias = path.parent().unwrap().join("alias");
        symlink(path.parent().unwrap(), &alias).unwrap();
        assert!(Store::open(&alias.join("state"), Limits::default()).is_err());
        symlink(path.parent().unwrap(), &path).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
    }

    #[test]
    fn reject_nonprivate_hardlinked_and_special_files() {
        for name in [
            "hub.lock",
            "hub.sqlite3",
            "hub.sqlite3-wal",
            "hub.sqlite3-shm",
            "hub.sqlite3-journal",
        ] {
            let (_temporary, path) = fixture();
            drop(Store::open(&path, Limits::default()).unwrap());
            let file = path.join(name);
            stdfs::write(&file, b"").unwrap();
            stdfs::set_permissions(&file, stdfs::Permissions::from_mode(0o644)).unwrap();
            assert!(Store::open(&path, Limits::default()).is_err(), "{name}");
        }
        let (_temporary, path) = fixture();
        drop(Store::open(&path, Limits::default()).unwrap());
        let file = path.join("hub.sqlite3");
        stdfs::hard_link(&file, path.parent().unwrap().join("alias")).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
        stdfs::remove_file(&file).unwrap();
        stdfs::create_dir(&file).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
    }

    #[test]
    fn reject_unsafe_ancestors_vcs_and_missing_parent() {
        for marker in [".git", ".hg", ".svn"] {
            let (_temporary, path) = fixture();
            stdfs::write(path.parent().unwrap().join(marker), b"marker").unwrap();
            assert!(Store::open(&path, Limits::default()).is_err());
            assert!(!path.exists());
        }
        let (_temporary, path) = fixture();
        let parent = path.parent().unwrap();
        stdfs::set_permissions(parent, stdfs::Permissions::from_mode(0o777)).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
        stdfs::set_permissions(parent, stdfs::Permissions::from_mode(0o700)).unwrap();
        assert!(Store::open(&path.join("missing"), Limits::default()).is_err());
        assert!(Store::open(Path::new("relative"), Limits::default()).is_err());
        assert!(Store::open(&parent.join("../state"), Limits::default()).is_err());
        stdfs::create_dir(&path).unwrap();
        stdfs::set_permissions(&path, stdfs::Permissions::from_mode(0o755)).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
    }

    #[test]
    fn reject_unrelated_database_or_unknown_schema_version() {
        for sql in [
            "CREATE TABLE unrelated(secret TEXT)",
            "PRAGMA application_id=1413828168; PRAGMA user_version=4",
        ] {
            let (_temporary, path) = fixture();
            drop(Store::open(&path, Limits::default()).unwrap());
            let file = path.join("hub.sqlite3");
            stdfs::remove_file(&file).unwrap();
            let db = Connection::open(&file).unwrap();
            db.execute_batch(sql).unwrap();
            drop(db);
            stdfs::set_permissions(&file, stdfs::Permissions::from_mode(0o600)).unwrap();
            assert!(Store::open(&path, Limits::default()).is_err());
            let db = Connection::open(&file).unwrap();
            let exists: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name='endpoints'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 0);
        }
    }

    #[test]
    fn crash_child() {
        let Some(path) = env::var_os("ENDPOINT_TEST_CRASH_STATE") else {
            return;
        };
        let mut store = Store::open(Path::new(&path), Limits::default()).unwrap();
        store
            .register_paused("ag_committed", 1, "ag_target")
            .unwrap();
        // SQLite has committed into WAL; emulate process loss without Drop/checkpoint.
        std::process::exit(0);
    }

    #[test]
    fn wal_recovers_committed_state_after_unclean_child_exit() {
        let (_temporary, path) = fixture();
        let result = Command::new(env::current_exe().unwrap())
            .env_clear()
            .env("ENDPOINT_TEST_CRASH_STATE", &path)
            .args([
                "--exact",
                "private_store::tests::crash_child",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(result.status.success());
        let mut store = Store::open(&path, Limits::default()).unwrap();
        assert!(store
            .register_paused("ag_committed", 1, "ag_target")
            .is_err());
    }
}
