//! App-local CLI pin checks, not attestation of an Agent's startup declaration.
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
#[derive(PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified: i64,
    modified_ns: i64,
    changed: i64,
    changed_ns: i64,
    mode: u32,
}
fn stamp(path: &Path) -> Result<Stamp, ()> {
    let m = fs::metadata(path).map_err(|_| ())?;
    if !m.is_file() || m.len() > 64 * 1024 * 1024 {
        return Err(());
    }
    Ok(Stamp {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        modified: m.mtime(),
        modified_ns: m.mtime_nsec(),
        changed: m.ctime(),
        changed_ns: m.ctime_nsec(),
        mode: m.mode(),
    })
}
pub(crate) struct Guard {
    path: PathBuf,
    expected: String,
    checked: Mutex<Option<(Stamp, Instant)>>,
    failed: AtomicBool,
}
impl Guard {
    pub(crate) fn new(path: &Path, expected: &str) -> Result<Self, ()> {
        if !path.is_absolute()
            || expected.len() != 64
            || !expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(());
        }
        let guard = Self {
            path: path.into(),
            expected: expected.into(),
            checked: Mutex::new(None),
            failed: false.into(),
        };
        guard.check()?;
        Ok(guard)
    }
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> Result<(), ()> {
        if self.failed() {
            return Err(());
        }
        let mut checked = match self.checked.lock() {
            Ok(c) => c,
            Err(_) => {
                self.failed.store(true, Ordering::Release);
                return Err(());
            }
        };
        if self.failed() {
            return Err(());
        }
        let result = (|| {
            let current = stamp(&self.path)?;
            if checked
                .as_ref()
                .is_some_and(|(s, at)| s == &current && at.elapsed() < Duration::from_secs(30))
            {
                return Ok(());
            }
            let digest = crate::relay::hash_executable(&self.path).map_err(|_| ())?;
            if digest != self.expected || stamp(&self.path)? != current {
                return Err(());
            }
            *checked = Some((current, Instant::now()));
            Ok(())
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_is_immediate_shared_failure_is_sticky_even_after_restoring_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli");
        fs::write(&path, b"approved").unwrap();
        let digest = crate::relay::hash_executable(&path).unwrap();
        let guard = Guard::new(&path, &digest).unwrap();
        fs::write(&path, b"unapproved replacement").unwrap();
        assert!(guard.check().is_err());
        assert!(guard.failed());
        fs::write(&path, b"approved").unwrap();
        assert!(guard.check().is_err());
        assert!(Guard::new(&path, &digest).is_ok()); // explicit new process state
    }
    #[test]
    fn periodic_full_hash_still_checks_a_matching_cached_stamp_and_removal_latches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli");
        fs::write(&path, b"approved").unwrap();
        let digest = crate::relay::hash_executable(&path).unwrap();
        let guard = Guard::new(&path, &digest).unwrap();
        fs::write(&path, b"different").unwrap();
        // Model metadata that alone could not distinguish the replacement.
        *guard.checked.lock().unwrap() = Some((
            stamp(&path).unwrap(),
            Instant::now() - Duration::from_secs(31),
        ));
        assert!(guard.check().is_err());
        fs::write(&path, b"approved").unwrap();
        let guard = Guard::new(&path, &digest).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(guard.check().is_err());
        assert!(guard.failed());
    }
    #[test]
    fn special_files_are_rejected_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        assert!(Guard::new(&path, &"a".repeat(64)).is_err());
        assert!(crate::relay::hash_executable(&path).is_err());
    }
}
