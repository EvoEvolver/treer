//! Private Rust journal and single-writer lease.
use crate::fs_util;
use anyhow::{bail, Result};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const LIVE_LIMIT: u64 = 32 * 1024 * 1024;
const STDERR_LIMIT: usize = 8192;
const LEASE: &str = ".rust-launcher.lock";

pub struct Store {
    pub root: PathBuf,
    cwd: PathBuf,
}

#[derive(Debug)]
pub struct PublicationUncertain;
impl std::fmt::Display for PublicationUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private_state_publication_uncertain")
    }
}
impl std::error::Error for PublicationUncertain {}

/// A process-lifetime, nonblocking single-writer OS file lock. Keep the file
/// in place: unlinking a lock file could let another process lock a new one.
pub struct Lease {
    _file: File,
}
impl Lease {
    pub fn acquire(root: &Path, cwd: &Path, _agent: &str) -> Result<Self> {
        let store =
            Store::open(root, cwd).map_err(|_| anyhow::anyhow!("private_state_unavailable"))?;
        let file = fs_util::open_or_create(&store.root.join(LEASE))
            .map_err(|_| anyhow::anyhow!("private_state_lease_unavailable"))?;
        let metadata = file
            .metadata()
            .map_err(|_| anyhow::anyhow!("private_state_lease_unavailable"))?;
        if !fs_util::is_private_file(&metadata) {
            bail!("private_state_lease_unavailable");
        }
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(fs::TryLockError::WouldBlock) => bail!("private_state_in_use"),
            Err(fs::TryLockError::Error(_)) => bail!("private_state_lease_unavailable"),
        }
    }
}
impl Store {
    /// The root must already exist (the Controller creates it private).
    /// Same-user processes are trusted; on Unix the owner and mode checks keep
    /// other OS users out of the private journal.
    pub fn open(root: &Path, cwd: &Path) -> Result<Self> {
        if !root.is_absolute() || !root.exists() {
            bail!("private root must already exist");
        }
        let root = fs::canonicalize(root)?;
        let cwd = fs::canonicalize(cwd)?;
        #[cfg(unix)]
        for ancestor in root.ancestors() {
            let metadata = fs::metadata(ancestor)?;
            if (metadata.uid() != rustix::process::geteuid().as_raw() && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                bail!("private ancestor owner/mode");
            }
        }
        #[cfg(unix)]
        {
            let m = fs::metadata(&root)?;
            if m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 {
                bail!("private root owner/mode");
            }
        }
        if !fs::metadata(&root)?.is_dir() {
            bail!("private root is not a directory");
        }
        Ok(Self { root, cwd })
    }
    pub fn load(&self) -> Result<Option<Value>> {
        let mut file = match fs_util::open_read(&self.root.join("rust-spike.json")) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let metadata = file.metadata()?;
        if !fs_util::is_private_file(&metadata) || metadata.len() > LIVE_LIMIT {
            bail!("invalid private state file");
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(LIVE_LIMIT + 1)
            .read_to_end(&mut data)?;
        if data.len() as u64 > LIVE_LIMIT {
            bail!("state too large");
        }
        Ok(Some(
            crate::bounded_json::decode(&data)
                .map_err(|_| anyhow::anyhow!("private_state_invalid"))?,
        ))
    }
    /// Write `bytes` to a new exclusive temporary and atomically replace
    /// `name` with it. The rename publishes the new bytes; a later
    /// directory-sync error is `PublicationUncertain`.
    fn publish(&self, temporary: &str, name: &str, bytes: &[u8]) -> Result<()> {
        let temp = self
            .root
            .join(format!("{temporary}-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = fs_util::create_new(&temp)?;
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs_util::replace(&temp, &self.root.join(name))?;
            fs_util::sync_dir(&self.root).map_err(|_| PublicationUncertain)?;
            Ok(())
        })();
        if result.is_err() {
            // Only the exact name exclusively created above is ours. Never
            // sweep another attempt's temporary files or replace the primary
            // write/sync/rename error with a secondary cleanup error.
            if let Err(error) = fs::remove_file(&temp) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("Rust spike: private_state_temporary_cleanup_failed");
                }
            }
        }
        result
    }
    pub fn save_stderr(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > STDERR_LIMIT {
            bail!("stderr_tail_too_large");
        }
        self.publish(".rust-stderr", "rust-provider-stderr.log", bytes)
    }
    pub fn save(&self, state: &Value) -> Result<()> {
        crate::bounded_json::admit(state).map_err(|_| anyhow::anyhow!("private_state_invalid"))?;
        #[cfg(feature = "fixture-harness")]
        if fixture_checkpoint_failure(state) {
            bail!("fixture_checkpoint_failure");
        }
        let data = serde_json::to_vec(state)?;
        if data.len() as u64 > LIVE_LIMIT {
            bail!("state too large");
        }
        // Rename already published the new bytes. A directory-sync error
        // must stop dispatch, but must not restore stale in-memory receipts.
        self.publish(".rust-spike", "rust-spike.json", &data)?;
        #[cfg(feature = "fixture-harness")]
        if std::env::var("RUST_SPIKE_CHECKPOINT_FAIL").as_deref() == Ok("directory-sync")
            && state["records"]
                .as_array()
                .is_some_and(|records| !records.is_empty())
        {
            return Err(PublicationUncertain.into());
        }
        Ok(())
    }
    pub fn retain_history(&self) -> Result<()> {
        crate::disk_history::retain(&self.root, &self.cwd)
    }
}

#[cfg(feature = "fixture-harness")]
fn fixture_checkpoint_failure(state: &Value) -> bool {
    let Ok(phase) = std::env::var("RUST_SPIKE_CHECKPOINT_FAIL") else {
        return false;
    };
    let record = &state["records"][0];
    let edit = &state["goals"]["edits"][0];
    match phase.as_str() {
        "dispatch-intent" => {
            record["submission"]["state"] == "running" && record["submission"]["turn_id"].is_null()
        }
        "dispatch-result" => record["submission"]["turn_id"].is_string(),
        "completion" => record["submission"]["state"] == "done",
        "completion-prune" => state["records"].as_array().is_some_and(|records| {
            records
                .iter()
                .any(|r| r["operation"] == "prune-fault" && r["submission"]["state"] == "done")
        }),
        "notification-ack" => record["notified"] == true,
        "goal-applying" => edit["applying"] == true,
        "goal-applied" => edit["applied"] == true,
        "goal-mode" => edit["mode"].is_string(),
        "goal-sent" => edit["sent"] == true,
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    pub(crate) struct Fixture {
        pub base: PathBuf,
        pub root: PathBuf,
        pub cwd: PathBuf,
    }
    impl Fixture {
        pub(crate) fn new() -> Self {
            let base = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("rust-store-{}", uuid::Uuid::new_v4()));
            treer_platform::secure_directory(&base).unwrap();
            let root = base.join("private");
            let cwd = base.join("cwd");
            treer_platform::secure_directory(&root).unwrap();
            fs::create_dir(&cwd).unwrap();
            Self { base, root, cwd }
        }
        pub(crate) fn store(&self) -> Store {
            Store::open(&self.root, &self.cwd).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn private_atomic_checkpoint_and_stderr_roundtrip() {
        let f = Fixture::new();
        let store = f.store();
        assert_eq!(store.load().unwrap(), None);
        for n in 0..3 {
            store
                .save(&json!({"schema":1,"records":[],"revision":n}))
                .unwrap();
            assert_eq!(store.load().unwrap().unwrap()["revision"], n);
        }
        store.save_stderr(b"synthetic bounded diagnostic").unwrap();
        assert!(store.save_stderr(&[0; 8193]).is_err());
        assert_eq!(
            fs::read(f.root.join("rust-provider-stderr.log")).unwrap(),
            b"synthetic bounded diagnostic"
        );
        // Only the published files and no leftover temporaries.
        let mut names: Vec<_> = fs::read_dir(&f.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["rust-provider-stderr.log", "rust-spike.json"]);
    }

    #[test]
    fn lease_is_exclusive_and_survives_in_place_after_holder_death() {
        let f = Fixture::new();
        let ready = f.base.join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "store::tests::lease_child"])
            .env("STORE_LEASE_ROOT", &f.root)
            .env("STORE_LEASE_CWD", &f.cwd)
            .env("STORE_LEASE_READY", &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let observed = ready.exists();
        let refused = observed
            && match Lease::acquire(&f.root, &f.cwd, "other") {
                Err(error) => error.to_string() == "private_state_in_use",
                Ok(_) => false,
            };
        let _ = child.kill();
        child.wait().unwrap();
        assert!(
            observed && refused,
            "real child must own the exclusive lease"
        );
        let lease = Lease::acquire(&f.root, &f.cwd, "next").unwrap();
        assert!(f.root.join(LEASE).exists());
        drop(lease);
        assert!(f.root.join(LEASE).exists());
    }
    #[test]
    #[ignore = "owned subprocess for exclusive OS lease test"]
    fn lease_child() {
        let root = PathBuf::from(std::env::var_os("STORE_LEASE_ROOT").unwrap());
        let cwd = PathBuf::from(std::env::var_os("STORE_LEASE_CWD").unwrap());
        let _lease = Lease::acquire(&root, &cwd, "synthetic").unwrap();
        fs::write(
            PathBuf::from(std::env::var_os("STORE_LEASE_READY").unwrap()),
            b"ready",
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(10));
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn placement_inside_cwd_or_vcs_is_allowed_but_unsafe_modes_are_refused() {
        let home = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("rust-home-placement-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join(".git")).unwrap();
        let root = home.join("ordinary/private");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::open(&root, &home).unwrap();
        store.save(&serde_json::json!({"records":[]})).unwrap();
        assert_eq!(
            store.load().unwrap(),
            Some(serde_json::json!({"records":[]}))
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Store::open(&root, &home).is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ambiguous_or_oversized_json_is_refused_without_rewriting_source() {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("rust-json-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("private");
        let cwd = base.join("cwd");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&cwd).unwrap();
        let store = Store::open(&root, &cwd).unwrap();
        let path = root.join("rust-spike.json");
        let ambiguous =
            br#"{"dispatchBlocked":true,"dispatchBlocked":false,"credential-canary":"synthetic"}"#;
        fs::write(&path, ambiguous).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            store.load().unwrap_err().to_string(),
            "private_state_invalid"
        );
        assert_eq!(fs::read(&path).unwrap(), ambiguous);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let mut deep = Value::Null;
        for _ in 0..32 {
            deep = serde_json::json!([deep]);
        }
        assert_eq!(
            store.save(&deep).unwrap_err().to_string(),
            "private_state_invalid"
        );
        assert_eq!(fs::read(&path).unwrap(), ambiguous);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let valid = serde_json::json!({"schema":1,"dispatchBlocked":true,"records":[]});
        store.save(&valid).unwrap();
        assert_eq!(store.load().unwrap(), Some(valid));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn failed_publication_removes_only_its_own_temp_and_preserves_primary_error() {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("rust-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        let root = base.join("private");
        let cwd = base.join("cwd");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&cwd).unwrap();
        let target = root.join("rust-spike.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();
        let other_temp = root.join(".rust-spike-unrelated.tmp");
        fs::write(&other_temp, b"keep").unwrap();
        let store = Store::open(&root, &cwd).unwrap();
        let error = store
            .save(&serde_json::json!({"fixture":true}))
            .unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert_eq!(fs::read(&other_temp).unwrap(), b"keep");
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
        fs::remove_dir_all(base).unwrap();
    }
}
