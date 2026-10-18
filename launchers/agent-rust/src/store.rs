//! Experiment-owned state only: never open or migrate a Node launcher's files.
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
    pub fn acquire(root: &Path, cwd: &Path, agent: &str) -> Result<Self> {
        let store = Store::open(root, cwd)
            .map_err(|_| anyhow::anyhow!("private_state_unavailable"))?;
        // This port cannot preserve all Node outbox/ack/goal/image semantics.
        // Refuse the entire legacy Agent directory, even for an explicit resume;
        // never parse, rewrite, rename, clean or silently ignore its contents.
        let mut legacy = cwd.to_path_buf();
        for (index, segment) in [".treer", "agents", if agent.is_empty() { "standalone" } else { agent }].into_iter().enumerate() {
            legacy.push(segment);
            match fs::symlink_metadata(&legacy) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {},
                Ok(_) => bail!("legacy_state_migration_required"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => bail!("legacy_state_migration_required"),
            }
            if index == 2 {
                bail!("legacy_state_migration_required");
            }
        }
        for name in ["queue.json", "agent-ui.json", "codex-ui.json", "images"] {
            match fs::symlink_metadata(store.root.join(name)) {
                Ok(_) => bail!("legacy_state_migration_required"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(_) => bail!("legacy_state_migration_required"),
            }
        }
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
    pub fn open(root: &Path, cwd: &Path) -> Result<Self> {
        if !root.is_absolute() || !root.exists() {
            bail!("private root must already exist");
        }
        // Inspect the lexical chain before canonicalization (do not erase links).
        for ancestor in root.ancestors() {
            if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
                bail!("linked private ancestor")
            }
        }
        let root = fs::canonicalize(root)?;
        let cwd = fs::canonicalize(cwd)?;
        if root.starts_with(&cwd) {
            bail!("spike private root must be outside cwd");
        }
        for ancestor in root.ancestors() {
            let metadata = fs::symlink_metadata(ancestor)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("unsafe private ancestor");
            }
            #[cfg(unix)]
            if (metadata.uid() != rustix::process::geteuid().as_raw() && metadata.uid() != 0)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                bail!("private ancestor owner/mode");
            }
            for marker in [".git", ".hg", ".jj", ".svn", ".bzr", ".sl"] {
                match fs::symlink_metadata(ancestor.join(marker)) {
                    Ok(_) => bail!("private state in VCS"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
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
        self.recheck()?;
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
        Ok(Some(serde_json::from_slice(&data)?))
    }
    pub fn recheck(&self) -> Result<()> {
        Self::open(&self.root, &self.cwd)?;
        Ok(())
    }
    pub fn save(&self, state: &Value) -> Result<()> {
        // Re-evaluate placement on each journal update; git init disables writes.
        Self::open(&self.root, &self.cwd)?;
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
