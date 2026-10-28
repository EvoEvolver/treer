//! Private Rust journal and single-writer lease. Node import is in node_import.
use anyhow::{bail, Result};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

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

/// A process-lifetime, nonblocking single-writer lease. Keep the inode in place:
/// unlinking a lock file could let another process lock a different inode.
pub struct Lease { _file: File }
impl Lease {
    pub fn acquire(root: &Path, cwd: &Path, _agent: &str) -> Result<Self> {
        let store = Store::open(root, cwd)
            .map_err(|_| anyhow::anyhow!("private_state_unavailable"))?;
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(store.root.join(".rust-launcher.lock"))
            .map_err(|_| anyhow::anyhow!("private_state_lease_unavailable"))?;
        let metadata = file.metadata()
            .map_err(|_| anyhow::anyhow!("private_state_lease_unavailable"))?;
        if !metadata.is_file() || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
            bail!("private_state_lease_unavailable");
        }
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| anyhow::anyhow!("private_state_in_use"))?;
        Ok(Self { _file: file })
    }
}
impl Store {
    /// Checked once when opened. Same-user processes are trusted; the owner
    /// and mode checks keep other OS users out of the private journal.
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
        Ok(Self { root, cwd })
    }
    pub fn load(&self) -> Result<Option<Value>> {
        let path = self.root.join("rust-spike.json");
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let mut file = match options.open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() > 32 * 1024 * 1024 {
            bail!("invalid private state file");
        }
        #[cfg(unix)]
        {
            let m = file.metadata()?;
            if m.uid() != fs::metadata(&self.root)?.uid() || m.mode() & 0o077 != 0 || m.nlink() != 1
            {
                bail!("private state file owner/mode/links");
            }
        }
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut data)?;
        if data.len() > 32 * 1024 * 1024 {
            bail!("state too large");
        }
        Ok(Some(crate::bounded_json::decode(&data)
            .map_err(|_| anyhow::anyhow!("private_state_invalid"))?))
    }
    pub fn save_stderr(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > 8192 { bail!("stderr_tail_too_large"); }
        let temp = self.root.join(format!(".rust-stderr-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&temp)?;
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, self.root.join("rust-provider-stderr.log"))?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_file(&temp); }
        result
    }
    pub fn save(&self, state: &Value) -> Result<()> {
        crate::bounded_json::admit(state)
            .map_err(|_| anyhow::anyhow!("private_state_invalid"))?;
        #[cfg(feature = "fixture-harness")]
        if fixture_checkpoint_failure(state) { bail!("fixture_checkpoint_failure"); }
        let data = serde_json::to_vec(state)?;
        if data.len() > 32 * 1024 * 1024 {
            bail!("state too large");
        }
        let temp = self
            .root
            .join(format!(".rust-spike-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut f = options.open(&temp)?;
        let result = (|| -> Result<()> {
            f.write_all(&data)?;
            f.sync_all()?;
            fs::rename(&temp, self.root.join("rust-spike.json"))?;
            #[cfg(feature = "fixture-harness")]
            if std::env::var("RUST_SPIKE_CHECKPOINT_FAIL").as_deref() == Ok("directory-sync")
                && state["records"].as_array().is_some_and(|records| !records.is_empty()) {
                return Err(PublicationUncertain.into());
            }
            // Rename already published the new bytes. A directory-sync error
            // must stop dispatch, but must not restore stale in-memory receipts.
            File::open(&self.root).and_then(|directory| directory.sync_all())
                .map_err(|_| PublicationUncertain)?;
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
    pub fn retain_history(&self) -> Result<()> {
        crate::disk_history::retain(&self.root, &self.cwd)
    }
}

#[cfg(feature = "fixture-harness")]
fn fixture_checkpoint_failure(state: &Value) -> bool {
    let Ok(phase) = std::env::var("RUST_SPIKE_CHECKPOINT_FAIL") else { return false; };
    let record = &state["records"][0];
    let edit = &state["goals"]["edits"][0];
    match phase.as_str() {
        "dispatch-intent" => record["submission"]["state"] == "running" && record["submission"]["turn_id"].is_null(),
        "dispatch-result" => record["submission"]["turn_id"].is_string(),
        "completion" => record["submission"]["state"] == "done",
        "completion-prune" => state["records"].as_array().is_some_and(|records| records.iter().any(|r| r["operation"] == "prune-fault" && r["submission"]["state"] == "done")),
        "notification-ack" => record["notified"] == true,
        "goal-applying" => edit["applying"] == true,
        "goal-applied" => edit["applied"] == true,
        "goal-mode" => edit["mode"].is_string(),
        "goal-sent" => edit["sent"] == true,
        _ => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn placement_inside_cwd_or_vcs_is_allowed_but_unsafe_modes_are_refused() {
        let home = fs::canonicalize(std::env::temp_dir()).unwrap()
            .join(format!("rust-home-placement-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(home.join(".git")).unwrap();
        let root = home.join("ordinary/private");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = Store::open(&root, &home).unwrap();
        store.save(&serde_json::json!({"records":[]})).unwrap();
        assert_eq!(store.load().unwrap(), Some(serde_json::json!({"records":[]})));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Store::open(&root, &home).is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ambiguous_or_oversized_json_is_refused_without_rewriting_source() {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap()
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
        let ambiguous = br#"{"dispatchBlocked":true,"dispatchBlocked":false,"credential-canary":"synthetic"}"#;
        fs::write(&path, ambiguous).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.load().unwrap_err().to_string(), "private_state_invalid");
        assert_eq!(fs::read(&path).unwrap(), ambiguous);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let mut deep = Value::Null;
        for _ in 0..32 { deep = serde_json::json!([deep]); }
        assert_eq!(store.save(&deep).unwrap_err().to_string(), "private_state_invalid");
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
