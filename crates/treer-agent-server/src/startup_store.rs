use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use treer_protocol::AgentStartupSpec;
use uuid::Uuid;

const STORE_VERSION: u32 = 2;
const MAX_DENIED_AGENT_IDS: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAgentStartup {
    #[serde(default)]
    pub app_revision: u64,
    #[serde(flatten)]
    pub spec: AgentStartupSpec,
    pub workspace_id: String,
    pub workload_credential: String,
    pub last_host_epoch: String,
    #[serde(default)]
    pub initial_writes: Vec<treer_host_protocol::HostWrite>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    36
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    entries: Vec<StoredAgentStartup>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    denied_agent_ids: BTreeSet<String>,
}

#[derive(Clone, Default)]
struct StoreState {
    entries: HashMap<String, StoredAgentStartup>,
    denied_agent_ids: BTreeSet<String>,
}

#[derive(Clone)]
pub struct StartupStore {
    inner: Arc<StartupStoreInner>,
}

struct StartupStoreInner {
    path: PathBuf,
    state: Mutex<StoreState>,
}

impl StartupStore {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let state = read_store(&path)?;
        Ok(Self {
            inner: Arc::new(StartupStoreInner {
                path,
                state: Mutex::new(state),
            }),
        })
    }

    pub fn entries(&self) -> Vec<StoredAgentStartup> {
        self.inner
            .state
            .lock()
            .map(|state| state.entries.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, agent_id: &str) -> Option<StoredAgentStartup> {
        self.inner
            .state
            .lock()
            .ok()
            .and_then(|state| state.entries.get(agent_id).cloned())
    }

    pub fn candidate_ids(
        &self,
        workspace_id: &str,
        server_id: &str,
        host_epoch: &str,
    ) -> Vec<String> {
        let mut candidates = self
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.workspace_id == workspace_id
                    && entry.spec.server_id == server_id
                    && entry.spec.enabled
                    && (entry.last_host_epoch != host_epoch || entry.spec.kind == "app")
            })
            .map(|entry| entry.spec.agent_id)
            .collect::<Vec<_>>();
        candidates.sort();
        candidates
    }

    pub fn upsert(&self, entry: StoredAgentStartup) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        if state.denied_agent_ids.contains(&entry.spec.agent_id) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Agent identity was permanently denied",
            ));
        }
        let mut updated = state.clone();
        updated.entries.insert(entry.spec.agent_id.clone(), entry);
        persist(&self.inner.path, &updated)?;
        *state = updated;
        Ok(())
    }

    pub fn remove(&self, agent_id: &str) -> io::Result<bool> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        let mut updated = state.clone();
        let removed = updated.entries.remove(agent_id).is_some();
        if removed {
            persist(&self.inner.path, &updated)?;
            *state = updated;
        }
        Ok(removed)
    }

    pub fn update(
        &self,
        agent_id: &str,
        mutate: impl FnOnce(&mut StoredAgentStartup),
    ) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        let mut updated = state.clone();
        if let Some(entry) = updated.entries.get_mut(agent_id) {
            mutate(entry);
            if entry.spec.agent_id != agent_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Agent startup update cannot change identity",
                ));
            }
            persist(&self.inner.path, &updated)?;
            *state = updated;
        }
        Ok(())
    }

    /// The caller holds the Controller recovery mutex and has checked Host and
    /// runtime absence. Persist before acknowledging: delayed creates must fail
    /// even after Controller replacement. A clear/remove never removes this fence.
    pub fn deny_absent(&self, agent_id: &str) -> io::Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        if state.entries.contains_key(agent_id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Agent startup exists",
            ));
        }
        if state.denied_agent_ids.contains(agent_id) {
            return Ok(());
        }
        if state.denied_agent_ids.len() >= MAX_DENIED_AGENT_IDS {
            return Err(io::Error::other("Agent denial fence limit reached"));
        }
        let mut updated = state.clone();
        updated.denied_agent_ids.insert(agent_id.to_owned());
        persist(&self.inner.path, &updated)?;
        *state = updated;
        Ok(())
    }

    pub fn is_denied(&self, agent_id: &str) -> io::Result<bool> {
        self.inner
            .state
            .lock()
            .map(|state| state.denied_agent_ids.contains(agent_id))
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))
    }
}

fn read_store(path: &Path) -> io::Result<StoreState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StoreState::default()),
        Err(error) => return Err(error),
    };
    let store: StoreFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if store.denied_agent_ids.len() > MAX_DENIED_AGENT_IDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Agent denial fence limit exceeded",
        ));
    }
    if !matches!(store.version, 1 | STORE_VERSION)
        || (store.version == 1 && !store.denied_agent_ids.is_empty())
        || store
            .entries
            .iter()
            .any(|entry| store.denied_agent_ids.contains(&entry.spec.agent_id))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Agent startup store version {}", store.version),
        ));
    }
    let entries = store
        .entries
        .into_iter()
        .map(|entry| (entry.spec.agent_id.clone(), entry))
        .collect();
    Ok(StoreState {
        entries,
        denied_agent_ids: store.denied_agent_ids,
    })
}

fn persist(path: &Path, state: &StoreState) -> io::Result<()> {
    if state.entries.is_empty() && state.denied_agent_ids.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent-startup.json");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let mut values: Vec<_> = state.entries.values().cloned().collect();
    values.sort_by(|left, right| left.spec.agent_id.cmp(&right.spec.agent_id));
    let bytes = serde_json::to_vec_pretty(&StoreFile {
        // Preserve legacy rollback until the first persistent denial is used.
        // Older Controllers reject v2 instead of ignoring the denied-ID fence.
        version: if state.denied_agent_ids.is_empty() {
            1
        } else {
            STORE_VERSION
        },
        entries: values,
        denied_agent_ids: state.denied_agent_ids.clone(),
    })
    .map_err(io::Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        #[cfg(windows)]
        treer_platform::secure_directory(parent)?;
        let mut file = options.open(&temporary)?;
        #[cfg(windows)]
        treer_platform::secure_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        treer_platform::replace_file(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(agent_id: &str) -> StoredAgentStartup {
        StoredAgentStartup {
            app_revision: 0,
            spec: AgentStartupSpec {
                agent_id: agent_id.to_string(),
                server_id: "server-one".to_string(),
                kind: "command".to_string(),
                name: "worker".to_string(),
                cwd: ".".to_string(),
                command: "sh".to_string(),
                args: vec!["-lc".to_string(), "exec worker".to_string()],
                publish_ports: vec![],
                enabled: true,
                generation: 1,
            },
            workspace_id: "workspace-one".to_string(),
            workload_credential: "wlc_secret".to_string(),
            last_host_epoch: "host-one".to_string(),
            initial_writes: vec![],
            cols: 120,
            rows: 36,
        }
    }

    #[test]
    fn denial_limit_preserves_existing_fences_and_refuses_unbounded_load() {
        let directory =
            std::env::temp_dir().join(format!("treer-startup-limit-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("startup.json");
        let mut fixture = StoreFile {
            version: 2,
            entries: vec![],
            denied_agent_ids: (0..MAX_DENIED_AGENT_IDS)
                .map(|i| format!("ag_{i}"))
                .collect(),
        };
        fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
        let store = StartupStore::load(path.clone()).unwrap();
        store.deny_absent("ag_0").unwrap();
        assert_eq!(
            store.deny_absent("one-more").unwrap_err().to_string(),
            "Agent denial fence limit reached"
        );
        assert!(store.is_denied("ag_0").unwrap());
        assert!(!store.is_denied("one-more").unwrap());
        fixture.denied_agent_ids.insert("one-more".into());
        fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
        assert!(
            matches!(StartupStore::load(path), Err(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn legacy_reader_rejects_fenced_store_instead_of_ignoring_denials() {
        // Frozen pre-fence reader contract: unknown JSON fields are ignored,
        // but the version is checked before any startup entry can be restored.
        #[derive(Deserialize)]
        struct LegacyStoreFile {
            version: u32,
            entries: Vec<StoredAgentStartup>,
        }
        fn legacy_read(bytes: &[u8]) -> io::Result<Vec<StoredAgentStartup>> {
            let store: LegacyStoreFile = serde_json::from_slice(bytes)?;
            if store.version != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported Agent startup store version {}", store.version),
                ));
            }
            Ok(store.entries)
        }
        let directory =
            std::env::temp_dir().join(format!("treer-startup-legacy-{}", Uuid::new_v4().simple()));
        let path = directory.join("startup.json");
        let store = StartupStore::load(path.clone()).unwrap();
        store.upsert(entry("unrelated")).unwrap();
        assert_eq!(legacy_read(&fs::read(&path).unwrap()).unwrap().len(), 1);
        store.deny_absent("denied").unwrap();
        let error = legacy_read(&fs::read(&path).unwrap()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "unsupported Agent startup store version 2"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_update_cannot_change_identity_to_bypass_a_denial() {
        let directory =
            std::env::temp_dir().join(format!("treer-startup-update-{}", Uuid::new_v4().simple()));
        let path = directory.join("startup.json");
        let store = StartupStore::load(path.clone()).unwrap();
        store.upsert(entry("allowed")).unwrap();
        store.deny_absent("denied").unwrap();
        assert_eq!(
            store
                .update("allowed", |entry| entry.spec.agent_id = "denied".into())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let reload = StartupStore::load(path).unwrap();
        assert_eq!(reload.get("allowed").unwrap().spec.agent_id, "allowed");
        assert!(reload.get("denied").is_none());
        assert!(reload.is_denied("denied").unwrap());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn denied_ids_survive_reload_clear_and_other_startup_changes() {
        let directory =
            std::env::temp_dir().join(format!("treer-startup-denial-{}", Uuid::new_v4().simple()));
        let path = directory.join("startup.json");
        let store = StartupStore::load(path.clone()).unwrap();
        store.upsert(entry("unrelated")).unwrap();
        let legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(legacy["version"], 1);
        assert!(legacy.get("denied_agent_ids").is_none());
        store.deny_absent("denied").unwrap();
        let reload = StartupStore::load(path.clone()).unwrap();
        assert!(reload.is_denied("denied").unwrap());
        assert_eq!(
            reload.upsert(entry("denied")).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!reload.remove("denied").unwrap());
        reload.remove("unrelated").unwrap();
        reload.deny_absent("denied").unwrap();
        assert!(StartupStore::load(path.clone())
            .unwrap()
            .is_denied("denied")
            .unwrap());
        let fenced: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            fenced["version"], 2,
            "old Controller version==1 check rejects a fenced store"
        );
        assert_eq!(fenced["denied_agent_ids"], serde_json::json!(["denied"]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_denial_persistence_does_not_acknowledge_or_change_memory() {
        let directory = std::env::temp_dir().join(format!(
            "treer-startup-denial-fail-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("startup.json");
        let store = StartupStore::load(path.clone()).unwrap();
        fs::create_dir(&path).unwrap(); // deterministic replace failure on every OS
        assert!(store.deny_absent("denied").is_err());
        assert!(!store.is_denied("denied").unwrap());
        assert!(
            fs::read_dir(&directory)
                .unwrap()
                .all(|e| e.unwrap().path() == path),
            "failed writes remove temporary files"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn denial_and_startup_upsert_serialize_without_an_unfenced_success() {
        for _ in 0..12 {
            let directory = std::env::temp_dir().join(format!(
                "treer-startup-denial-race-{}",
                Uuid::new_v4().simple()
            ));
            let path = directory.join("startup.json");
            let store = StartupStore::load(path.clone()).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let writer = std::thread::spawn({
                let store = store.clone();
                let barrier = barrier.clone();
                move || {
                    barrier.wait();
                    store.upsert(entry("racing"))
                }
            });
            barrier.wait();
            let denied = store.deny_absent("racing");
            let write = writer.join().unwrap();
            let reload = StartupStore::load(path).unwrap();
            match (denied, write) {
                (Ok(()), Err(error)) => {
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                    assert!(reload.is_denied("racing").unwrap());
                    assert!(reload.get("racing").is_none());
                }
                (Err(error), Ok(())) => {
                    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
                    assert!(!reload.is_denied("racing").unwrap());
                    assert!(reload.get("racing").is_some());
                }
                _ => panic!("denial and startup cannot both succeed"),
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn persists_updates_and_removes_entries() {
        let directory = std::env::temp_dir().join(format!(
            "treer-startup-store-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("create directory");
        let path = directory.join("startup.json");
        let store = StartupStore::load(path.clone()).expect("load store");
        store.upsert(entry("agent-one")).expect("persist entry");
        assert_eq!(
            StartupStore::load(path.clone())
                .expect("reload store")
                .get("agent-one")
                .expect("stored entry")
                .spec
                .command,
            "sh"
        );
        assert!(store.remove("agent-one").expect("remove entry"));
        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("remove directory");
    }

    #[test]
    fn candidates_require_a_new_host_epoch_and_an_enabled_spec() {
        let directory = std::env::temp_dir().join(format!(
            "treer-startup-candidates-test-{}",
            Uuid::new_v4().simple()
        ));
        let path = directory.join("startup.json");
        let store = StartupStore::load(path).expect("load store");
        let mut disabled = entry("agent-disabled");
        disabled.spec.enabled = false;
        store.upsert(entry("agent-enabled")).expect("store enabled");
        store.upsert(disabled).expect("store disabled");

        assert!(store
            .candidate_ids("workspace-one", "server-one", "host-one")
            .is_empty());
        assert_eq!(
            store.candidate_ids("workspace-one", "server-one", "host-two"),
            ["agent-enabled"]
        );
        assert!(store
            .candidate_ids("another-workspace", "server-one", "host-two")
            .is_empty());

        fs::remove_dir_all(directory).expect("remove directory");
    }
}
