//! Windows private journal. All Win32 mechanics stay in treer-platform.
//! Existing Controller-owned roots are checked, never adopted or ACL-repaired.
use anyhow::{bail, Result};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use treer_platform::machine_file::Root;

pub(crate) const LIVE_LIMIT: u64 = 32 * 1024 * 1024;
const STDERR_LIMIT: u64 = 8192;
const LEASE: &str = ".registry.lock";
const REPARSE_OR_DIRECTORY: u32 = 0x400 | 0x10;

pub struct Store {
    pub root: PathBuf,
    cwd: PathBuf,
    handle: Root,
}
#[derive(Debug)]
pub struct PublicationUncertain;
impl std::fmt::Display for PublicationUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private_state_publication_uncertain")
    }
}
impl std::error::Error for PublicationUncertain {}

/// Share-mode-zero OS lease, held for the lifetime of this value. Never unlink.
pub struct Lease {
    _file: File,
    _store: Store,
}
impl Lease {
    pub fn acquire(root: &Path, cwd: &Path, _agent: &str) -> Result<Self> {
        let store =
            Store::open(root, cwd).map_err(|_| anyhow::anyhow!("private_state_unavailable"))?;
        match store.read_checked(LEASE, 0) {
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {
                // Stage gives the new lease an explicit private DACL before
                // any use. Never repair/adopt an existing permissive file.
                let staged = store
                    .handle
                    .stage(LEASE, &temporary("lease"), 0o600)
                    .map_err(|error| lease_error(&error.into()))?;
                staged
                    .commit(false)
                    .map_err(|error| lease_error(&error.into()))?;
            }
            Err(error) => return Err(lease_error(&error)),
        }
        let file = store
            .handle
            .exclusive_lease()
            .map_err(|error| lease_error(&error.into()))?;
        check_file(&file, &store.root.join(LEASE), 0)
            .map_err(|_| anyhow::anyhow!("private_state_lease_unavailable"))?;
        Ok(Self {
            _file: file,
            _store: store,
        })
    }
}

// ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION are evidence of an
// occupied file lease. Permission denial alone is not; unsafe ACL/storage
// refusals must not be advertised as another legitimate writer.
fn lease_error(error: &anyhow::Error) -> anyhow::Error {
    let occupied = error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| matches!(error.raw_os_error(), Some(32 | 33)));
    anyhow::anyhow!(if occupied {
        "private_state_in_use"
    } else {
        "private_state_lease_unavailable"
    })
}

pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}
pub(crate) fn temporary(kind: &str) -> String {
    format!(
        ".treer-transfer-xfer_rust-{kind}-{}.tmp",
        uuid::Uuid::new_v4()
    )
}
pub(crate) fn check_file(file: &File, path: &Path, limit: u64) -> Result<()> {
    let (_, _, links, attrs) = treer_platform::transfer_file_identity(file)?;
    if links != 1
        || attrs & REPARSE_OR_DIRECTORY != 0
        || !file.metadata()?.is_file()
        || file.metadata()?.len() > limit
        || !treer_platform::private_path_is_secure(path)?
    {
        bail!("private_state_file_unavailable");
    }
    Ok(())
}

impl Store {
    pub fn open(root: &Path, cwd: &Path) -> Result<Self> {
        if !root.is_absolute() {
            bail!("private_state_unavailable");
        }
        // Open lexical components first: canonicalization must not erase a
        // junction/symlink. These retained handles refuse delete sharing.
        let handle = Root::open(root, &[])?;
        let root = fs::canonicalize(root)?;
        let cwd = fs::canonicalize(cwd)?;
        if !treer_platform::private_path_is_secure(&root)? {
            bail!("private_state_root_unavailable");
        }
        // Compare actual directory identities, not case-sensitive path strings:
        // Windows spelling/case aliases cannot turn cwd data into "outside".
        let cwd_file = treer_platform::machine_file::open_protected(&cwd)?;
        let (cwd_volume, cwd_index, _, _) = treer_platform::transfer_file_identity(&cwd_file)?;
        let mut child: Option<&Path> = None;
        for ancestor in root.ancestors() {
            let file = treer_platform::machine_file::open_protected(ancestor)?;
            let (volume, index, _, _) = treer_platform::transfer_file_identity(&file)?;
            if (volume, index) == (cwd_volume, cwd_index) {
                let hidden = child
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str())
                    .is_some_and(|name| name.starts_with('.') && name != "." && name != "..");
                if !hidden {
                    bail!("private_state_placement");
                }
                break;
            }
            child = Some(ancestor);
        }
        for ancestor in root.ancestors() {
            for marker in [".git", ".hg", ".jj", ".svn", ".bzr", ".sl"] {
                match fs::symlink_metadata(ancestor.join(marker)) {
                    Ok(_) => bail!("private_state_in_vcs"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(Self { root, cwd, handle })
    }
    pub fn recheck(&self) -> Result<()> {
        Self::open(&self.root, &self.cwd)?;
        Ok(())
    }
    pub(crate) fn read_checked(&self, name: &str, limit: u64) -> Result<File> {
        let file = self.handle.read(name)?;
        check_file(&file, &self.root.join(name), limit)?;
        Ok(file)
    }
    pub(crate) fn directory(&self) -> &Root {
        &self.handle
    }
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub fn load(&self) -> Result<Option<Value>> {
        self.recheck()?;
        let mut file = match self.read_checked("rust-spike.json", LIVE_LIMIT) {
            Ok(file) => file,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(LIVE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIVE_LIMIT {
            bail!("private_state_invalid");
        }
        Ok(Some(
            crate::bounded_json::decode(&bytes)
                .map_err(|_| anyhow::anyhow!("private_state_invalid"))?,
        ))
    }
    fn publish(&self, name: &str, kind: &str, bytes: &[u8], limit: u64) -> Result<()> {
        self.recheck()?;
        if bytes.len() as u64 > limit {
            bail!("private_state_too_large");
        }
        match self.read_checked(name, limit) {
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        let mut staged = self.handle.stage(name, &temporary(kind), 0o600)?;
        staged.file_mut().write_all(bytes)?;
        staged.file_mut().sync_all()?;
        // Atomic WRITE_THROUGH rename, never delete the old checkpoint first.
        // A failed publish has no dispatch/recovery authority.
        staged.commit(true).map_err(|_| PublicationUncertain)?;
        Ok(())
    }
    pub fn save_stderr(&self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u64 > STDERR_LIMIT {
            bail!("stderr_tail_too_large");
        }
        self.publish("rust-provider-stderr.log", "stderr", bytes, STDERR_LIMIT)
    }
    pub fn save(&self, state: &Value) -> Result<()> {
        crate::bounded_json::admit(state).map_err(|_| anyhow::anyhow!("private_state_invalid"))?;
        #[cfg(feature = "fixture-harness")]
        if fixture_checkpoint_failure(state) {
            bail!("fixture_checkpoint_failure");
        }
        self.publish(
            "rust-spike.json",
            "spike",
            &serde_json::to_vec(state)?,
            LIVE_LIMIT,
        )?;
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
                .join(format!("rust-windows-store-{}", uuid::Uuid::new_v4()));
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
            assert!(
                treer_platform::private_path_is_secure(&f.root.join("rust-spike.json")).unwrap()
            );
        }
        store.save_stderr(b"synthetic bounded diagnostic").unwrap();
        assert!(store.save_stderr(&[0; 8193]).is_err());
        assert!(
            treer_platform::private_path_is_secure(&f.root.join("rust-provider-stderr.log"))
                .unwrap()
        );
    }
    #[test]
    fn permissive_acl_hardlink_and_vcs_refuse_without_rewrite() {
        let f = Fixture::new();
        let store = f.store();
        let path = f.root.join("rust-spike.json");
        store.save(&json!({"schema":1,"records":[]})).unwrap();
        let original = fs::read(&path).unwrap();
        fs::hard_link(&path, f.base.join("alias")).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&json!({"changed":true})).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(f.base.join("alias")).unwrap();
        treer_platform::apply_sddl_dacl(&path, "D:P(A;;GA;;;WD)").unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&json!({"changed":true})).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        treer_platform::secure_file(&path).unwrap(); // Exact test-owned leaf only.
        fs::write(f.base.join(".git"), b"synthetic VCS marker").unwrap();
        assert!(store.recheck().is_err());
        assert!(store.save_stderr(b"refused").is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn public_root_is_not_adopted() {
        let f = Fixture::new();
        treer_platform::apply_sddl_dacl(&f.root, "D:P(A;;GA;;;WD)").unwrap();
        assert!(Store::open(&f.root, &f.cwd).is_err());
        assert!(!treer_platform::private_path_is_secure(&f.root).unwrap());
    }

    #[test]
    fn physical_cwd_identity_enforces_hidden_first_component() {
        let f = Fixture::new();
        assert!(Store::open(&f.root, &f.base).is_err());
        let hidden = f.base.join(".private");
        treer_platform::secure_directory(&hidden).unwrap();
        assert!(Store::open(&hidden, &f.base).is_ok());
        assert!(Store::open(&hidden, &hidden).is_err());
        let alternate = PathBuf::from(f.base.to_string_lossy().to_uppercase());
        assert!(Store::open(&f.root, &alternate).is_err());
    }
    #[test]
    #[ignore = "requires Windows symbolic-link privilege; no implicit privileged setup"]
    fn actual_reparse_root_is_refused() {
        let f = Fixture::new();
        let alias = f.base.join("alias");
        std::os::windows::fs::symlink_dir(&f.root, &alias).unwrap();
        assert!(Store::open(&alias, &f.cwd).is_err());
    }

    #[test]
    fn ambiguous_json_is_not_loaded_or_replaced() {
        let f = Fixture::new();
        let path = f.root.join("rust-spike.json");
        let original = br#"{"dispatchBlocked":true,"dispatchBlocked":false}"#;
        fs::write(&path, original).unwrap();
        treer_platform::secure_file(&path).unwrap();
        assert_eq!(
            f.store().load().unwrap_err().to_string(),
            "private_state_invalid"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut deep = Value::Null;
        for _ in 0..32 {
            deep = json!([deep]);
        }
        assert!(f.store().save(&deep).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn only_actual_sharing_or_lock_violation_is_classified_in_use() {
        for code in [32, 33] {
            let error: anyhow::Error = std::io::Error::from_raw_os_error(code).into();
            assert_eq!(lease_error(&error).to_string(), "private_state_in_use");
        }
        for code in [5, 2, 87] {
            let error: anyhow::Error = std::io::Error::from_raw_os_error(code).into();
            assert_eq!(
                lease_error(&error).to_string(),
                "private_state_lease_unavailable"
            );
        }
        assert_eq!(
            lease_error(&anyhow::anyhow!("private_state_file_unavailable")).to_string(),
            "private_state_lease_unavailable"
        );
    }
    #[test]
    fn permissive_existing_lease_is_not_repaired_or_classified_in_use() {
        let f = Fixture::new();
        let path = f.root.join(LEASE);
        fs::write(&path, b"").unwrap();
        treer_platform::apply_sddl_dacl(&path, "D:P(A;;GA;;;WD)").unwrap();
        let error = match Lease::acquire(&f.root, &f.cwd, "synthetic") {
            Err(error) => error,
            Ok(_) => panic!("unsafe lease adopted"),
        };
        assert_eq!(error.to_string(), "private_state_lease_unavailable");
        assert!(!treer_platform::private_path_is_secure(&path).unwrap());
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn lease_is_exclusive_and_survives_in_place_after_holder_death() {
        let f = Fixture::new();
        let ready = f.base.join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "store::tests::lease_child"])
            .env("WINDOWS_STORE_ROOT", &f.root)
            .env("WINDOWS_STORE_CWD", &f.cwd)
            .env("WINDOWS_STORE_READY", &ready)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
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
        let root = PathBuf::from(std::env::var_os("WINDOWS_STORE_ROOT").unwrap());
        let cwd = PathBuf::from(std::env::var_os("WINDOWS_STORE_CWD").unwrap());
        let _lease = Lease::acquire(&root, &cwd, "synthetic").unwrap();
        fs::write(
            PathBuf::from(std::env::var_os("WINDOWS_STORE_READY").unwrap()),
            b"ready",
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(10));
    }
}
