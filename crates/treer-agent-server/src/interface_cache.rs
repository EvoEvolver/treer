use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;
use treer_protocol::AgentInterfaceDescriptor;
use uuid::Uuid;

const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAgentInterface {
    pub agent_id: String,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub interface: AgentInterfaceDescriptor,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: Vec<CachedAgentInterface>,
}

#[derive(Clone)]
pub struct InterfaceCache {
    inner: Arc<InterfaceCacheInner>,
}

struct InterfaceCacheInner {
    path: PathBuf,
    entries: Mutex<HashMap<String, CachedAgentInterface>>,
}

impl InterfaceCache {
    pub fn load(path: PathBuf) -> Self {
        let entries = match read_cache(&path) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(path = %path.display(), %error, "ignoring invalid Agent Interface cache");
                HashMap::new()
            }
        };
        Self {
            inner: Arc::new(InterfaceCacheInner {
                path,
                entries: Mutex::new(entries),
            }),
        }
    }

    pub fn entries(&self) -> Vec<CachedAgentInterface> {
        self.inner
            .entries
            .lock()
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn replace_all(&self, entries: Vec<CachedAgentInterface>) -> io::Result<()> {
        let mut current = self
            .inner
            .entries
            .lock()
            .map_err(|_| io::Error::other("Agent Interface cache lock poisoned"))?;
        *current = entries
            .into_iter()
            .map(|entry| (entry.agent_id.clone(), entry))
            .collect();
        persist(&self.inner.path, &current)
    }

    pub fn upsert(&self, entry: CachedAgentInterface) -> io::Result<()> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| io::Error::other("Agent Interface cache lock poisoned"))?;
        entries.insert(entry.agent_id.clone(), entry);
        persist(&self.inner.path, &entries)
    }

    pub fn remove(&self, agent_id: &str) -> io::Result<()> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| io::Error::other("Agent Interface cache lock poisoned"))?;
        if entries.remove(agent_id).is_some() {
            persist(&self.inner.path, &entries)?;
        }
        Ok(())
    }
}

fn read_cache(path: &Path) -> io::Result<HashMap<String, CachedAgentInterface>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let cache: CacheFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if cache.version != CACHE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported cache version {}", cache.version),
        ));
    }
    Ok(cache
        .entries
        .into_iter()
        .map(|entry| (entry.agent_id.clone(), entry))
        .collect())
}

fn persist(path: &Path, entries: &HashMap<String, CachedAgentInterface>) -> io::Result<()> {
    if entries.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("interfaces.json");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let mut values: Vec<_> = entries.values().cloned().collect();
    values.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    let bytes = serde_json::to_vec_pretty(&CacheFile {
        version: CACHE_VERSION,
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
    use treer_protocol::AGENT_INTERFACE_PROTOCOL_V1;

    fn entry(agent_id: &str) -> CachedAgentInterface {
        CachedAgentInterface {
            agent_id: agent_id.to_string(),
            pid: 42,
            started_at: Utc::now(),
            interface: AgentInterfaceDescriptor {
                protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
                instance_id: "test-interface".to_string(),
                port: 4180,
                capabilities: vec!["prompt.submit".to_string()],
                ui_path: Some("/".to_string()),
                registered_at: Utc::now(),
            },
        }
    }

    #[test]
    fn persists_and_removes_entries() {
        let directory = std::env::temp_dir().join(format!(
            "treer-interface-cache-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        let path = directory.join("interfaces.json");
        let cache = InterfaceCache::load(path.clone());
        cache.upsert(entry("agent-one")).expect("persist entry");

        let restored = InterfaceCache::load(path.clone()).entries();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].agent_id, "agent-one");

        cache.remove("agent-one").expect("remove entry");
        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
