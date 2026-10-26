//! Credential-bearing startup state is backed up only inside a private job.
//! The updater normally never changes this state. Comparison is NOT a CAS and
//! does not authorize restoring over an active Controller's concurrent writes.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treer_platform::private_directory::PrivateDirectory;
use uuid::Uuid;

use super::{hex, identifier, Journal, Phase, Receipt};

const LIMIT: u64 = 16 * 1024 * 1024;
const ENTRY_LIMIT: usize = 4096;
const INDEX_LIMIT: u64 = 1024 * 1024;
const BACKUP: &str = "startup-backup.json";
const INDEX: &str = "startup-backup-index.json";

#[derive(Deserialize)]
struct Store {
    version: u32,
    entries: Vec<Entry>,
    #[serde(default)]
    denied_agent_ids: Vec<String>,
}

// Unknown fields (including workload credentials, command arguments and initial
// writes) are parsed/skipped but never retained in this metadata projection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    agent_id: String,
    workspace_id: String,
    server_id: String,
    generation: u64,
    #[serde(default)]
    app_revision: u64,
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    sha256: String,
    size: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema_version: u32,
    job_id: Uuid,
    workspace_id: String,
    server_id: String,
    fingerprint: Option<Fingerprint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Unchanged,
    Changed,
}

fn name(receipt: &Receipt) -> String {
    format!("{}-agent-startup.json", receipt.server_id)
}

fn bounded(directory: &PrivateDirectory, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let file = match directory.read_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("unsafe_update_startup_state"),
    };
    // Atomic startup-store replacement leaves this reader on one complete inode.
    // It is still a snapshot, not exclusion against subsequent legitimate edits.
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("update_startup_read_failed"))?;
    if bytes.len() as u64 > limit {
        bail!("update_startup_state_too_large");
    }
    Ok(Some(bytes))
}

fn fingerprint(bytes: &[u8], receipt: &Receipt) -> Result<Fingerprint> {
    if bytes.len() as u64 > LIMIT {
        bail!("update_startup_state_too_large");
    }
    let store: Store = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid_update_startup_state"))?;
    if !matches!(store.version, 1 | 2)
        || store.entries.len() > ENTRY_LIMIT
        || store.denied_agent_ids.len() > ENTRY_LIMIT
        || (store.version == 1 && !store.denied_agent_ids.is_empty())
    {
        bail!("unsupported_update_startup_state");
    }
    let mut denied = BTreeSet::new();
    for agent_id in store.denied_agent_ids {
        if !identifier(&agent_id, "ag_") || !denied.insert(agent_id) {
            bail!("invalid_update_startup_state");
        }
    }
    let mut entries = BTreeMap::new();
    for entry in store.entries {
        if !identifier(&entry.agent_id, "ag_")
            || entry.workspace_id != receipt.workspace_id
            || entry.server_id != receipt.server_id
            || entry.generation == 0
            || denied.contains(&entry.agent_id)
        {
            bail!("update_startup_binding_mismatch");
        }
        if entries.insert(entry.agent_id.clone(), entry).is_some() {
            bail!("invalid_update_startup_state");
        }
    }
    Ok(Fingerprint {
        // Bind the complete raw store, including version and denial fences.
        // Keep the existing safe index projection/schema: no fence is dropped
        // from the private backup or omitted from integrity/change detection.
        sha256: format!("{:x}", Sha256::digest(bytes)),
        size: bytes.len() as u64,
        entries: entries.into_values().collect(),
    })
}

fn write(directory: &PrivateDirectory, name: &str, bytes: &[u8]) -> Result<()> {
    let mut file = directory
        .create_file(name)
        .map_err(|_| anyhow::anyhow!("update_startup_backup_exists_or_unsafe"))?;
    file.write_all(bytes)
        .map_err(|_| anyhow::anyhow!("update_startup_backup_failed"))?;
    file.sync_all()
        .map_err(|_| anyhow::anyhow!("update_startup_backup_failed"))?;
    drop(file);
    directory
        .sync()
        .map_err(|_| anyhow::anyhow!("update_startup_backup_failed"))?;
    Ok(())
}

/// Exclusive capture during preparation, under installation/enrollment locks.
/// A partial write leaves an unfinished job for explicit recovery; do not retry
/// capture over it or remove any pre-existing file. Absent state is recorded as
/// absent (StartupStore deletes its file when its final entry is removed).
pub fn capture(state: &PrivateDirectory, journal: &Journal, expected_revision: u64) -> Result<()> {
    let receipt = journal.read()?;
    if receipt.phase != Phase::Preparing || receipt.revision != expected_revision {
        bail!("update_revision_or_phase_conflict");
    }
    let bytes = bounded(state, &name(&receipt), LIMIT)?;
    let fingerprint = bytes
        .as_deref()
        .map(|b| fingerprint(b, &receipt))
        .transpose()?;
    // Even absent source state must not bless a partial/pre-existing backup.
    for name in [INDEX, BACKUP] {
        match journal.directory.read_file(name) {
            Ok(_) => bail!("update_startup_backup_exists_or_unsafe"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("update_startup_backup_exists_or_unsafe"),
        }
    }
    if let Some(bytes) = bytes {
        write(&journal.directory, BACKUP, &bytes)?;
    }
    let index = Index {
        schema_version: 1,
        job_id: receipt.job_id,
        workspace_id: receipt.workspace_id,
        server_id: receipt.server_id,
        fingerprint,
    };
    let encoded =
        serde_json::to_vec(&index).map_err(|_| anyhow::anyhow!("update_startup_backup_failed"))?;
    if encoded.len() as u64 > INDEX_LIMIT {
        bail!("update_startup_index_too_large");
    }
    write(&journal.directory, INDEX, &encoded)
}

/// Revalidate private backup integrity and binding before using its comparison.
/// No filesystem paths or credential bytes can leave this function's result.
pub fn compare(state: &PrivateDirectory, journal: &Journal) -> Result<Comparison> {
    let receipt = journal.read()?;
    let bytes = bounded(&journal.directory, INDEX, INDEX_LIMIT)?
        .ok_or_else(|| anyhow::anyhow!("missing_update_startup_backup"))?;
    let index: Index = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid_update_startup_backup"))?;
    if index.schema_version != 1
        || index.job_id != receipt.job_id
        || index.workspace_id != receipt.workspace_id
        || index.server_id != receipt.server_id
    {
        bail!("update_startup_binding_mismatch");
    }
    let saved = bounded(&journal.directory, BACKUP, LIMIT)?;
    match (&index.fingerprint, saved) {
        (Some(expected), Some(bytes)) if expected.size <= LIMIT && hex(&expected.sha256, 64) => {
            if fingerprint(&bytes, &receipt)? != *expected {
                bail!("update_startup_backup_conflict");
            }
        }
        (None, None) => {}
        _ => bail!("update_startup_backup_conflict"),
    }
    let current = bounded(state, &name(&receipt), LIMIT)?
        .as_deref()
        .map(|b| fingerprint(b, &receipt))
        .transpose()?;
    Ok(if current == index.fingerprint {
        Comparison::Unchanged
    } else {
        Comparison::Changed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::{publication::Publications, Source};
    use chrono::Utc;
    use std::{
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };

    struct Fixture {
        path: PathBuf,
        state: PrivateDirectory,
        journal: Journal,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-startup-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let root = PrivateDirectory::open(&path).unwrap();
            let state = root.create_child("state").unwrap();
            let jobs = root.create_child("jobs").unwrap();
            let now = Utc::now();
            let old = "a".repeat(64);
            let new = "b".repeat(64);
            let receipt = Receipt {
                schema_version: 1,
                job_id: Uuid::new_v4(),
                revision: 0,
                workspace_id: "ws_fixture".into(),
                server_id: "srv_fixture".into(),
                release_sha: "a".repeat(40),
                artifact_source_sha: "b".repeat(40),
                controller_sha256: new.clone(),
                cli_sha256: new.clone(),
                publications: Publications::new(old.clone(), old, &new, &new),
                source: Source::Pin {
                    sums_sha256: "c".repeat(64),
                    allow_pinned: true,
                    allow_artifact_origin: false,
                    allow_same: true,
                    allow_downgrade: false,
                },
                phase: Phase::Preparing,
                created_at: now,
                updated_at: now,
            };
            let journal = Journal::create(&jobs, &receipt).unwrap();
            Self {
                path,
                state,
                journal,
            }
        }
        fn store(&self, json: &serde_json::Value) {
            let mut file = self.state.create_file("next").unwrap();
            file.write_all(&serde_json::to_vec_pretty(json).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            drop(file);
            self.state
                .replace_file("next", "srv_fixture-agent-startup.json")
                .unwrap();
        }
        fn backup_path(&self, name: &str) -> PathBuf {
            self.path
                .join("jobs")
                .join(self.journal.id.to_string())
                .join(name)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }
    fn store() -> serde_json::Value {
        serde_json::json!({"version": 1, "entries": [{
            "agent_id": "ag_fixture", "workspace_id": "ws_fixture", "server_id": "srv_fixture",
            "generation": 3, "app_revision": 9, "enabled": false,
            "workload_credential": "synthetic-private-credential", "args": ["private-argument"], "initial_writes": [{"text":"private-content"}]
        }]})
    }

    #[test]
    fn exact_private_backup_retains_disabled_entries_and_never_projects_credentials() {
        let f = Fixture::new();
        f.store(&store());
        let original = std::fs::read(f.path.join("state/srv_fixture-agent-startup.json")).unwrap();
        capture(&f.state, &f.journal, 0).unwrap();
        assert_eq!(std::fs::read(f.backup_path(BACKUP)).unwrap(), original);
        assert_eq!(
            std::fs::metadata(f.backup_path(BACKUP))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let index = std::fs::read_to_string(f.backup_path(INDEX)).unwrap();
        for text in [
            "synthetic-private-credential",
            "private-argument",
            "private-content",
            "workload_credential",
            "initial_writes",
        ] {
            assert!(!index.contains(text));
        }
        let metadata: Index = serde_json::from_str(&index).unwrap();
        assert_eq!(metadata.fingerprint.unwrap().entries[0].generation, 3);
        assert_eq!(
            compare(&f.state, &f.journal).unwrap(),
            Comparison::Unchanged
        );
        assert!(capture(&f.state, &f.journal, 0).is_err());
        assert_eq!(std::fs::read(f.backup_path(BACKUP)).unwrap(), original);
    }

    #[test]
    fn actual_startup_store_v2_denial_only_backup_preserves_fences_and_detects_edits() {
        let f = Fixture::new();
        let path = f.path.join("state/srv_fixture-agent-startup.json");
        let store = crate::startup_store::StartupStore::load(path.clone()).unwrap();
        store.deny_absent("ag_deleted").unwrap();
        let original = std::fs::read(&path).unwrap();
        let persisted: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(persisted["version"], 2);
        assert_eq!(persisted["entries"], serde_json::json!([]));
        capture(&f.state, &f.journal, 0).unwrap();
        assert_eq!(std::fs::read(f.backup_path(BACKUP)).unwrap(), original);
        assert_eq!(
            compare(&f.state, &f.journal).unwrap(),
            Comparison::Unchanged
        );
        let backup = crate::startup_store::StartupStore::load(f.backup_path(BACKUP)).unwrap();
        assert!(backup.is_denied("ag_deleted").unwrap());
        store.deny_absent("ag_later_deleted").unwrap();
        assert_eq!(compare(&f.state, &f.journal).unwrap(), Comparison::Changed);
        assert!(store.is_denied("ag_later_deleted").unwrap());
        assert!(!backup.is_denied("ag_later_deleted").unwrap());
        // A tampered backup cannot be accepted even though its entry projection
        // remains empty and the on-disk format is otherwise valid.
        std::fs::write(f.backup_path(BACKUP), std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            compare(&f.state, &f.journal).unwrap_err().to_string(),
            "update_startup_backup_conflict"
        );
    }

    #[test]
    fn v2_mixed_entries_and_denials_keep_private_bytes_and_existing_index_schema() {
        let f = Fixture::new();
        let mut json = store();
        json["version"] = 2.into();
        json["denied_agent_ids"] = serde_json::json!(["ag_deleted"]);
        f.store(&json);
        capture(&f.state, &f.journal, 0).unwrap();
        assert_eq!(
            compare(&f.state, &f.journal).unwrap(),
            Comparison::Unchanged
        );
        let index = std::fs::read_to_string(f.backup_path(INDEX)).unwrap();
        assert!(!index.contains("synthetic-private-credential"));
        assert!(!index.contains("denied_agent_ids"));
        let index: Index = serde_json::from_str(&index).unwrap();
        assert_eq!(index.schema_version, 1);
        assert_eq!(index.fingerprint.unwrap().entries.len(), 1);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.backup_path(BACKUP)).unwrap()).unwrap();
        assert_eq!(saved, json);
    }

    #[test]
    fn invalid_denial_fences_refuse_before_any_backup_without_echoing_input() {
        for kind in ["legacy", "duplicate", "overlap", "invalid", "limit", "type"] {
            let f = Fixture::new();
            let mut json = store();
            json["version"] = 2.into();
            json["denied_agent_ids"] = serde_json::json!(["ag_deleted"]);
            match kind {
                "legacy" => json["version"] = 1.into(),
                "duplicate" => {
                    json["denied_agent_ids"] = serde_json::json!(["ag_deleted", "ag_deleted"])
                }
                "overlap" => json["denied_agent_ids"] = serde_json::json!(["ag_fixture"]),
                "invalid" => json["denied_agent_ids"] = serde_json::json!(["private-credential"]),
                "limit" => {
                    json["denied_agent_ids"] = serde_json::json!((0..=ENTRY_LIMIT)
                        .map(|i| format!("ag_denied{i}"))
                        .collect::<Vec<_>>())
                }
                _ => json["denied_agent_ids"] = serde_json::json!(null),
            }
            f.store(&json);
            let error = capture(&f.state, &f.journal, 0).unwrap_err().to_string();
            assert!(!error.contains("private"));
            assert!(!error.contains(f.path.to_str().unwrap()));
            assert!(!f.backup_path(BACKUP).exists());
            assert!(!f.backup_path(INDEX).exists());
        }
    }

    #[test]
    fn concurrent_atomic_edits_and_removal_are_preserved_not_restored() {
        let f = Fixture::new();
        f.store(&store());
        capture(&f.state, &f.journal, 0).unwrap();
        let mut edited = store();
        edited["entries"][0]["app_revision"] = 10.into();
        f.store(&edited);
        assert_eq!(compare(&f.state, &f.journal).unwrap(), Comparison::Changed);
        let current: serde_json::Value = serde_json::from_slice(
            &std::fs::read(f.path.join("state/srv_fixture-agent-startup.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(current, edited);
        f.state
            .remove_file("srv_fixture-agent-startup.json")
            .unwrap();
        assert_eq!(compare(&f.state, &f.journal).unwrap(), Comparison::Changed);
        assert!(!f.path.join("state/srv_fixture-agent-startup.json").exists());
    }

    #[test]
    fn absent_state_is_explicit_and_newly_created_state_is_not_deleted() {
        let f = Fixture::new();
        capture(&f.state, &f.journal, 0).unwrap();
        assert!(!f.backup_path(BACKUP).exists());
        assert_eq!(
            compare(&f.state, &f.journal).unwrap(),
            Comparison::Unchanged
        );
        f.store(&store());
        assert_eq!(compare(&f.state, &f.journal).unwrap(), Comparison::Changed);
        assert!(f.path.join("state/srv_fixture-agent-startup.json").exists());

        let partial = Fixture::new();
        partial.journal.directory.create_file(BACKUP).unwrap();
        assert!(capture(&partial.state, &partial.journal, 0).is_err());
        assert!(!partial.backup_path(INDEX).exists());
    }

    #[test]
    fn malformed_cross_scope_legacy_or_duplicate_entries_fail_without_echoing_data() {
        for kind in ["scope", "generation", "duplicate", "version"] {
            let f = Fixture::new();
            let mut json = store();
            match kind {
                "scope" => json["entries"][0]["workspace_id"] = "ws_other".into(),
                "generation" => {
                    json["entries"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("generation");
                }
                "duplicate" => {
                    let entry = json["entries"][0].clone();
                    json["entries"].as_array_mut().unwrap().push(entry);
                }
                _ => json["version"] = 999.into(),
            }
            f.store(&json);
            let error = capture(&f.state, &f.journal, 0).err().unwrap().to_string();
            assert!(!error.contains("private"));
            assert!(!error.contains(f.path.to_str().unwrap()));
            assert!(!f.backup_path(BACKUP).exists());
        }
    }

    #[test]
    fn corrupted_or_cross_job_backups_are_never_accepted() {
        let f = Fixture::new();
        f.store(&store());
        capture(&f.state, &f.journal, 0).unwrap();
        std::fs::write(f.backup_path(BACKUP), b"synthetic-private-credential").unwrap();
        assert!(compare(&f.state, &f.journal).is_err());
        let bytes = serde_json::to_vec_pretty(&store()).unwrap();
        std::fs::write(f.backup_path(BACKUP), &bytes).unwrap();
        let mut index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.backup_path(INDEX)).unwrap()).unwrap();
        index["job_id"] = Uuid::new_v4().to_string().into();
        std::fs::write(f.backup_path(INDEX), serde_json::to_vec(&index).unwrap()).unwrap();
        assert_eq!(
            compare(&f.state, &f.journal).err().unwrap().to_string(),
            "update_startup_binding_mismatch"
        );
    }

    #[test]
    fn symlink_backup_and_oversized_source_fail_closed() {
        let f = Fixture::new();
        f.store(&store());
        symlink(
            f.path.join("state/srv_fixture-agent-startup.json"),
            f.backup_path(BACKUP),
        )
        .unwrap();
        assert!(capture(&f.state, &f.journal, 0).is_err());
        let f = Fixture::new();
        f.state
            .create_file("srv_fixture-agent-startup.json")
            .unwrap()
            .set_len(LIMIT + 1)
            .unwrap();
        assert_eq!(
            capture(&f.state, &f.journal, 0).err().unwrap().to_string(),
            "update_startup_state_too_large"
        );
        assert!(!f.backup_path(BACKUP).exists());
    }
}
