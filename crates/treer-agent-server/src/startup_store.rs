use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use treer_protocol::AgentStartupSpec;
use uuid::Uuid;

const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAgentStartup {
    #[serde(flatten)]
    pub spec: AgentStartupSpec,
    pub workspace_id: String,
    pub workload_credential: String,
    pub last_host_epoch: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    entries: Vec<StoredAgentStartup>,
}

#[derive(Clone)]
pub struct StartupStore {
    inner: Arc<StartupStoreInner>,
}

struct StartupStoreInner {
    path: PathBuf,
    entries: Mutex<HashMap<String, StoredAgentStartup>>,
}

impl StartupStore {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let entries = read_store(&path)?;
        Ok(Self {
            inner: Arc::new(StartupStoreInner {
                path,
                entries: Mutex::new(entries),
            }),
        })
    }

    pub fn entries(&self) -> Vec<StoredAgentStartup> {
        self.inner
            .entries
            .lock()
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, agent_id: &str) -> Option<StoredAgentStartup> {
        self.inner
            .entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(agent_id).cloned())
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
                    && entry.last_host_epoch != host_epoch
            })
            .map(|entry| entry.spec.agent_id)
            .collect::<Vec<_>>();
        candidates.sort();
        candidates
    }

    pub fn upsert(&self, entry: StoredAgentStartup) -> io::Result<()> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        entries.insert(entry.spec.agent_id.clone(), entry);
        persist(&self.inner.path, &entries)
    }

    pub fn remove(&self, agent_id: &str) -> io::Result<bool> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| io::Error::other("Agent startup store lock poisoned"))?;
        let removed = entries.remove(agent_id).is_some();
        if removed {
            persist(&self.inner.path, &entries)?;
        }
        Ok(removed)
    }
}

fn read_store(path: &Path) -> io::Result<HashMap<String, StoredAgentStartup>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let store: StoreFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if store.version != STORE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Agent startup store version {}", store.version),
        ));
    }
    Ok(store
        .entries
        .into_iter()
        .map(|entry| (entry.spec.agent_id.clone(), entry))
        .collect())
}

fn persist(path: &Path, entries: &HashMap<String, StoredAgentStartup>) -> io::Result<()> {
    if entries.is_empty() {
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
    let mut values: Vec<_> = entries.values().cloned().collect();
    values.sort_by(|left, right| left.spec.agent_id.cmp(&right.spec.agent_id));
    let bytes = serde_json::to_vec_pretty(&StoreFile {
        version: STORE_VERSION,
        entries: values,
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
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&temporary, path)
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
