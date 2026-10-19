//! Fail-closed Unix storage boundary, not isolation against another same-UID process.
use crate::store::Error;
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
    let lock = private_file(&directory, "hub.lock")?;
    fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == Errno::WOULDBLOCK {
            Error::Busy
        } else {
            Error::PrivateStorage
        }
    })?;
    let database = private_file(&directory, "hub.sqlite3")?;
    // Retain this validated inode until AFTER SQLite closes. Never open/close
    // additional database descriptors while SQLite owns its record locks.
    for name in ["hub.sqlite3-wal", "hub.sqlite3-shm", "hub.sqlite3-journal"] {
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
        path.join("hub.sqlite3"),
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
    for name in ["hub.sqlite3", "hub.sqlite3-wal", "hub.sqlite3-shm"] {
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
                "hub.sqlite3",
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
            "hub.sqlite3",
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
        "hub.lock",
        "hub.sqlite3",
        "hub.sqlite3-wal",
        "hub.sqlite3-shm",
        "hub.sqlite3-journal",
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

/// Strict local provisioning input, never an HTTP upload or a hub-supplied file.
pub(crate) fn read_config(path: &Path) -> Result<Vec<u8>, Error> {
    read_config_optional(path)?.ok_or(Error::PrivateStorage)
}

pub(crate) fn read_config_optional(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    use std::io::Read;
    let parent = path.parent().ok_or(Error::PrivateStorage)?;
    let name = path.file_name().ok_or(Error::PrivateStorage)?;
    let directory = open_directory(parent, false)?;
    let file = match fs::openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(Error::PrivateStorage),
    };
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
    Ok(Some(bytes))
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
    fn closing_owner_releases_lock_even_when_spawned_child_inherits_descriptor() {
        use std::process::Stdio;
        let (_temporary, path) = fixture();
        let store = Store::open(&path, Limits::default()).unwrap();
        // Model the fork-before-exec inheritance window deterministically by
        // retaining the same open file description as this private child's stdin.
        let inherited = store
            ._private_files
            .as_ref()
            .unwrap()
            ._lock
            .try_clone()
            .unwrap();
        let mut child = Command::new("/usr/bin/python3")
            .env_clear()
            .args(["-c", "import time; time.sleep(30)"])
            .stdin(Stdio::from(std::fs::File::from(inherited)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(store);
        let result = Store::open(&path, Limits::default());
        // Reap this exact child even if the assertion below fails.
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            result.is_ok(),
            "owner closed but inherited descriptor retained lock: {:?}",
            result.err()
        );
    }

    #[test]
    fn database_pin_outlives_connection_and_closes_before_writer_unlock() {
        let (_temporary, path) = fixture();
        let store = Store::open(&path, Limits::default()).unwrap();
        let Store {
            db,
            _private_files: files,
            ..
        } = store;
        db.close().unwrap();
        let files = files.unwrap();
        check_file(files.database_file.as_ref().unwrap()).unwrap();
        assert!(Store::open(&path, Limits::default()).is_err());
        drop(files);
        let reopened = Store::open(&path, Limits::default()).unwrap();
        reopened.recheck_private_storage().unwrap();
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
    fn sqlite_shared_lock_survives_open_and_periodic_metadata_checks() {
        let (_temporary, path) = fixture();
        let mut store = Store::open(&path, Limits::default()).unwrap();
        store.register_paused("ag_before", 1, "ag_target").unwrap();
        for _ in 0..3 {
            // Probe in a separate process: POSIX locks are process-wide. WAL
            // connections retain a database SHARED lock until connection close.
            let result = Command::new("/usr/bin/python3")
                .env_clear()
                .args(["-I", "-c", "import fcntl,sys\nwith open(sys.argv[1], 'r+b') as f:\n try: fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB, 510, 0x40000002)\n except BlockingIOError: sys.exit(0)\n sys.exit(1)"])
                .arg(path.join("hub.sqlite3"))
                .output()
                .unwrap();
            assert!(result.status.success(), "SQLite shared lock was lost");
            store.recheck_private_storage().unwrap();
        }
        store.register_paused("ag_after", 1, "ag_target").unwrap();
        drop(store);
        let reopened = Store::open(&path, Limits::default()).unwrap();
        assert_eq!(
            reopened
                .db
                .query_row("SELECT count(*) FROM endpoints", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn periodic_metadata_check_rejects_replaced_sidecar_symlink() {
        let (_temporary, path) = fixture();
        let store = Store::open(&path, Limits::default()).unwrap();
        let shm = path.join("hub.sqlite3-shm");
        let target = path.join("untouched");
        stdfs::write(&target, b"not SQLite").unwrap();
        stdfs::remove_file(&shm).unwrap();
        symlink(&target, &shm).unwrap();
        assert!(store.recheck_private_storage().is_err());
        assert_eq!(stdfs::read(target).unwrap(), b"not SQLite");
    }

    #[test]
    fn worker_commit_hook_refuses_drift_and_rolls_back_telegram_offset() {
        for replacement in [false, true] {
            for explicit_transaction in [false, true] {
                let (_temporary, path) = fixture();
                let store = Store::open(&path, Limits::default()).unwrap();
                store.db.execute_batch("CREATE TABLE telegram_accounts(account TEXT PRIMARY KEY,offset INTEGER NOT NULL); INSERT INTO telegram_accounts VALUES('synthetic',17);").unwrap();
                // Deliberately bypass all API and worker recheck call sites.
                // In the explicit case, drift happens after SQL writes but
                // before commit; the hook must roll back the entire transaction.
                if explicit_transaction {
                    store
                        .db
                        .execute_batch("BEGIN IMMEDIATE; UPDATE telegram_accounts SET offset=18;")
                        .unwrap();
                }
                let database = path.join("hub.sqlite3");
                stdfs::rename(&database, path.join("original.sqlite3")).unwrap();
                if replacement {
                    stdfs::write(&database, b"replacement").unwrap();
                    stdfs::set_permissions(&database, stdfs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                let result = if explicit_transaction {
                    store.db.execute_batch("COMMIT;")
                } else {
                    store
                        .db
                        .execute_batch("UPDATE telegram_accounts SET offset=18;")
                };
                assert!(result.is_err());
                assert!(store.db.is_autocommit());
                assert_eq!(
                    store
                        .db
                        .query_row("SELECT offset FROM telegram_accounts", [], |r| r
                            .get::<_, i64>(0))
                        .unwrap(),
                    17
                );
                assert!(store.recheck_private_storage().is_err());
                if replacement {
                    assert_eq!(stdfs::read(database).unwrap(), b"replacement");
                } else {
                    assert!(!database.exists());
                }
            }
        }
    }

    #[test]
    fn database_replacement_latches_even_after_original_identity_is_restored() {
        let (_temporary, path) = fixture();
        let mut store = Store::open(&path, Limits::default()).unwrap();
        store
            .register_paused("ag_original", 1, "ag_target")
            .unwrap();
        let database = path.join("hub.sqlite3");
        let original = path.join("original.sqlite3");
        stdfs::rename(&database, &original).unwrap();
        stdfs::write(&database, b"replacement").unwrap();
        stdfs::set_permissions(&database, stdfs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.recheck_private_storage().is_err());
        assert_eq!(stdfs::read(&database).unwrap(), b"replacement");
        stdfs::remove_file(&database).unwrap();
        stdfs::rename(&original, &database).unwrap();
        assert!(store.recheck_private_storage().is_err());
        drop(store);
        let reopened = Store::open(&path, Limits::default()).unwrap();
        reopened.recheck_private_storage().unwrap();
        assert_eq!(
            reopened
                .db
                .query_row("SELECT count(*) FROM endpoints", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn database_unlink_is_rejected_without_recreating_the_path() {
        let (_temporary, path) = fixture();
        let store = Store::open(&path, Limits::default()).unwrap();
        let database = path.join("hub.sqlite3");
        stdfs::remove_file(&database).unwrap();
        assert!(store.recheck_private_storage().is_err());
        assert!(store.recheck_private_storage().is_err());
        assert!(!database.exists());
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
