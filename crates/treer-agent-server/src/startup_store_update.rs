//! Loaded-instance CAS under the SAME mutex as normal Store writers. No path
//! constructor, worker Store reload, lifecycle authority or sender release.
use super::*;
use sha2::{Digest, Sha256};
#[cfg(not(windows))]
use std::io::Read;
use treer_protocol::controller_update_worker_inputs::Snapshot;

fn refused() -> io::Error {
    io::Error::other("startup_update_compare_write_unconfirmed")
}

fn parse(bytes: Option<&[u8]>, workspace: &str, server: &str) -> io::Result<StoreState> {
    let Some(bytes) = bytes else {
        return Ok(StoreState::default());
    };
    if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
        return Err(refused());
    }
    let file: StoreFile = serde_json::from_slice(bytes).map_err(|_| refused())?;
    if !matches!(file.version, 1 | STORE_VERSION)
        || file.entries.len() > 4096
        || file.denied_agent_ids.len() > MAX_DENIED_AGENT_IDS
        || (file.version == 1 && !file.denied_agent_ids.is_empty())
    {
        return Err(refused());
    }
    let valid = |id: &str| crate::windows_update_intent::identifier(id, "ag_");
    if file.denied_agent_ids.iter().any(|id| !valid(id)) {
        return Err(refused());
    }
    let mut state = StoreState {
        denied_agent_ids: file.denied_agent_ids,
        ..Default::default()
    };
    for entry in file.entries {
        if !valid(&entry.spec.agent_id)
            || entry.workspace_id != workspace
            || entry.spec.server_id != server
            || entry.spec.generation == 0
            || state.denied_agent_ids.contains(&entry.spec.agent_id)
            || state
                .entries
                .insert(entry.spec.agent_id.clone(), entry)
                .is_some()
        {
            return Err(refused());
        }
    }
    Ok(state)
}

fn equivalent(a: &StoreState, b: &StoreState) -> io::Result<bool> {
    Ok(a.denied_agent_ids == b.denied_agent_ids
        && serde_json::to_value(&a.entries).map_err(|_| refused())?
            == serde_json::to_value(&b.entries).map_err(|_| refused())?)
}

fn read(path: &Path) -> io::Result<(Option<Snapshot>, Option<Vec<u8>>)> {
    #[cfg(windows)]
    let (id, bytes) = {
        // Reuse the public checked physical snapshot seam. The loaded Arc is
        // still the write authority; opening a directory never reloads a Store.
        let directory = treer_platform::windows_update_backup::BackupDirectory::open(
            path.parent().ok_or_else(refused)?,
        )
        .map_err(|_| refused())?;
        let observed = directory
            .startup_snapshot(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(refused)?,
            )
            .map_err(|_| refused())?;
        let Some(observed) = observed else {
            return Ok((None, None));
        };
        observed
    };
    #[cfg(not(windows))]
    let (id, bytes) = {
        use std::os::unix::fs::MetadataExt;
        let mut file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((None, None)),
            Err(_) => return Err(refused()),
        };
        let id = file.metadata()?.ino();
        let mut bytes = Vec::new();
        (&mut file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused())?;
        (id, bytes)
    };
    if id == 0 || bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
        return Err(refused());
    }
    Ok((
        Some(Snapshot {
            file_id: id,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        }),
        Some(bytes),
    ))
}

/// Non-Clone/non-wire token retains the exact loaded Arc and one operation.
pub(crate) struct Compare {
    store: StartupStore,
    revision: u64,
    expected: Option<Snapshot>,
    workspace: String,
    server: String,
    operation: Uuid,
    attempted: bool,
}

/// A successful SAME-instance write, not a deserializable ACK or release grant.
pub(crate) struct Committed {
    store: StartupStore,
    revision: u64,
    snapshot: Option<Snapshot>,
    operation: Uuid,
    fenced: bool,
}

impl StartupStore {
    pub(crate) fn update_compare(
        &self,
        operation: Uuid,
        workspace: &str,
        server: &str,
        expected: &Option<Snapshot>,
    ) -> io::Result<Compare> {
        if operation.get_version_num() != 4
            || operation.get_variant() != uuid::Variant::RFC4122
            || !crate::windows_update_intent::identifier(workspace, "ws_")
            || !crate::windows_update_intent::identifier(server, "srv_")
        {
            return Err(refused());
        }
        let state = self.write_state()?;
        let (snapshot, raw) = read(&self.inner.path)?;
        if &snapshot != expected || !equivalent(&state, &parse(raw.as_deref(), workspace, server)?)?
        {
            return Err(refused());
        }
        Ok(Compare {
            store: self.clone(),
            revision: state.revision,
            expected: snapshot,
            workspace: workspace.into(),
            server: server.into(),
            operation,
            attempted: false,
        })
    }
}

impl Compare {
    /// No await/relock between compare, fence validation, existing persist and
    /// memory commit. Every attempt is consumed, including pre-write refusal.
    pub(crate) fn commit(&mut self, replacement: Option<&[u8]>) -> io::Result<Committed> {
        self.commit_with(replacement, persist)
    }

    /// Explicit mutation preparation: install the SAME loaded-instance writer
    /// fence under the CAS mutex before any possible write. No Drop/unfreeze or
    /// wire constructor exists; unknown requires explicit future reconciliation.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn commit_fenced(&mut self, replacement: Option<&[u8]>) -> io::Result<Committed> {
        self.commit_using(replacement, persist, true)
    }

    fn commit_with(
        &mut self,
        replacement: Option<&[u8]>,
        write: impl FnOnce(&Path, &StoreState) -> io::Result<()>,
    ) -> io::Result<Committed> {
        self.commit_using(replacement, write, false)
    }

    fn commit_using(
        &mut self,
        replacement: Option<&[u8]>,
        write: impl FnOnce(&Path, &StoreState) -> io::Result<()>,
        fenced: bool,
    ) -> io::Result<Committed> {
        if self.attempted {
            return Err(refused());
        }
        self.attempted = true;
        let mut state = self.store.write_state()?;
        let (observed, raw) = read(&self.store.inner.path)?;
        let current = parse(raw.as_deref(), &self.workspace, &self.server)?;
        let mut updated = parse(replacement, &self.workspace, &self.server)?;
        if state.revision != self.revision
            || observed != self.expected
            || !equivalent(&state, &current)?
            || !state.denied_agent_ids.is_subset(&updated.denied_agent_ids)
        {
            return Err(refused());
        }
        updated.revision = state.revision.checked_add(1).ok_or_else(refused)?;
        if fenced {
            state.update_owner = Some(self.operation);
            updated.update_owner = Some(self.operation);
        }
        // Once persist can have effects, an IO/readback failure also fences all
        // normal writers on THIS loaded instance; no blind overwrite after unknown.
        state.update_unknown = true;
        write(&self.store.inner.path, &updated).map_err(|_| refused())?;
        let (snapshot, raw) = read(&self.store.inner.path)?;
        if !equivalent(
            &updated,
            &parse(raw.as_deref(), &self.workspace, &self.server)?,
        )? {
            return Err(refused());
        }
        let revision = updated.revision;
        *state = updated;
        Ok(Committed {
            store: self.store.clone(),
            revision,
            snapshot,
            operation: self.operation,
            fenced,
        })
    }
}

impl Committed {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn fenced_snapshot(&self, operation: [u8; 16]) -> io::Result<&Option<Snapshot>> {
        if !self.fenced {
            return Err(refused());
        }
        self.snapshot(operation)
    }
    pub(crate) fn snapshot(&self, operation: [u8; 16]) -> io::Result<&Option<Snapshot>> {
        if operation != *self.operation.as_bytes() {
            return Err(refused());
        }
        let mut state = self.store.inner.state.lock().map_err(|_| refused())?;
        if state.update_unknown || state.update_owner != self.fenced.then_some(self.operation) {
            return Err(refused());
        }
        let check = || -> io::Result<()> {
            if state.revision != self.revision || read(&self.store.inner.path)?.0 != self.snapshot {
                return Err(refused());
            }
            Ok(())
        };
        if check().is_err() {
            if self.fenced {
                state.update_unknown = true;
            }
            return Err(refused());
        }
        Ok(&self.snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_update_mutation_fence_committed_owner_blocks_clone_writers_after_drop() {
        let f = Fixture::new();
        f.store.deny_absent("ag_deleted").unwrap();
        let mut compare = f.compare();
        let operation = *compare.operation.as_bytes();
        let committed = compare
            .commit_fenced(Some(&raw(&["ag_deleted", "ag_added"])))
            .unwrap();
        assert!(committed.snapshot(operation).unwrap().is_some());
        assert!(committed.snapshot(*Uuid::new_v4().as_bytes()).is_err());
        assert!(
            committed.snapshot(operation).is_ok(),
            "foreign scope cannot poison the owner"
        );
        assert!(f.store.clone().is_denied("ag_added").unwrap());
        let disk: serde_json::Value =
            serde_json::from_slice(&fs::read(&f.store.inner.path).unwrap()).unwrap();
        assert_eq!(disk["version"], 2);
        assert!(
            disk.get("update_owner").is_none(),
            "memory fence is not startup wire data"
        );
        for _ in 0..2 {
            assert!(f.store.clone().deny_absent("ag_later").is_err());
            assert!(f.store.upsert(entry()).is_err());
            assert!(f.store.remove_deleted("ag_deleted").is_err());
            assert!(f.store.remove("ag_missing").is_err());
            assert!(f
                .store
                .update("ag_missing", |_| panic!("fenced writer callback"))
                .is_err());
            assert!(f
                .store
                .update_compare(Uuid::new_v4(), "ws_fixture", "srv_fixture", &None)
                .is_err());
        }
        drop(committed);
        assert!(f.store.clone().deny_absent("ag_after_drop").is_err());
        assert!(f.store.is_denied("ag_deleted").unwrap());
    }

    #[test]
    fn windows_update_mutation_fence_prewrite_refusal_or_io_unknown_has_distinct_custody() {
        // A rejected monotone compare has no effect and must not acquire a fence.
        let f = Fixture::new();
        f.store.deny_absent("ag_deleted").unwrap();
        let mut compare = f.compare();
        assert!(compare.commit_fenced(Some(&raw(&[]))).is_err());
        assert!(compare.commit_fenced(Some(&raw(&["ag_deleted"]))).is_err());
        f.store.deny_absent("ag_normal").unwrap();
        assert!(f.store.inner.state.lock().unwrap().update_owner.is_none());
        for readback_failure in [false, true] {
            let f = Fixture::new();
            let mut compare = f.compare();
            let operation = compare.operation;
            assert!(compare
                .commit_using(
                    Some(&raw(&["ag_kept"])),
                    |path, state| {
                        if readback_failure {
                            persist(path, state)?;
                            fs::write(path, b"injected unknown")?;
                            Ok(())
                        } else {
                            Err(io::Error::other("injected persistence failure"))
                        }
                    },
                    true
                )
                .is_err());
            drop(compare);
            let state = f.store.inner.state.lock().unwrap();
            assert_eq!(state.update_owner, Some(operation));
            assert!(state.update_unknown);
            drop(state);
            assert!(f.store.clone().deny_absent("ag_later").is_err());
        }
    }

    #[test]
    fn windows_update_mutation_fence_changed_raw_permanently_poisoned_not_repaired() {
        let f = Fixture::new();
        let mut compare = f.compare();
        let operation = *compare.operation.as_bytes();
        let committed = compare.commit_fenced(Some(&raw(&["ag_kept"]))).unwrap();
        let original = fs::read(&f.store.inner.path).unwrap();
        fs::write(&f.store.inner.path, raw(&["ag_foreign"])).unwrap();
        assert!(committed.snapshot(operation).is_err());
        fs::write(&f.store.inner.path, original).unwrap();
        assert!(committed.snapshot(operation).is_err());
        assert!(f.store.clone().deny_absent("ag_later").is_err());
        assert!(f.store.is_denied("ag_kept").unwrap());
    }
    struct Fixture {
        root: PathBuf,
        store: StartupStore,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("treer-startup-cas-{}", Uuid::new_v4().simple()));
            fs::create_dir(&root).unwrap();
            #[cfg(windows)]
            treer_platform::secure_directory(&root).unwrap();
            let store = StartupStore::load(root.join("srv_fixture-agent-startup.json")).unwrap();
            Self { root, store }
        }
        fn compare(&self) -> Compare {
            self.store
                .update_compare(
                    Uuid::new_v4(),
                    "ws_fixture",
                    "srv_fixture",
                    &read(&self.store.inner.path).unwrap().0,
                )
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    fn raw(denials: &[&str]) -> Vec<u8> {
        serde_json::to_vec(
            &serde_json::json!({"version":2,"entries":[],"denied_agent_ids":denials}),
        )
        .unwrap()
    }
    fn entry() -> StoredAgentStartup {
        serde_json::from_value(serde_json::json!({
            "agent_id":"ag_active", "server_id":"srv_fixture", "kind":"command",
            "name":"fixture", "cwd":".", "command":"fixture", "args":[],
            "publish_ports":[], "enabled":true, "generation":1,
            "workspace_id":"ws_fixture", "workload_credential":"fixture-only",
            "last_host_epoch":"fixture"
        }))
        .unwrap()
    }
    #[test]
    fn windows_update_startup_cas_loaded_clone_commit_preserves_normal_writer_fences() {
        let f = Fixture::new();
        let mut token = f.compare();
        let op = *token.operation.as_bytes();
        let mut replacement: serde_json::Value =
            serde_json::from_slice(&raw(&["ag_deleted"])).unwrap();
        replacement["entries"] = serde_json::json!([entry()]);
        let committed = token
            .commit(Some(&serde_json::to_vec(&replacement).unwrap()))
            .unwrap();
        assert_eq!(f.store.get("ag_active").unwrap().spec.generation, 1);
        assert!(committed.snapshot(op).unwrap().is_some());
        assert!(committed.snapshot(*Uuid::new_v4().as_bytes()).is_err());
        assert!(f.store.clone().is_denied("ag_deleted").unwrap());
        let bytes = fs::read(&f.store.inner.path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["version"],
            2
        );
        assert!(token.commit(Some(&raw(&[]))).is_err());
        f.store.clone().deny_absent("ag_normal").unwrap();
        assert!(committed.snapshot(op).is_err());
        let loaded = StartupStore::load(f.store.inner.path.clone()).unwrap();
        assert!(loaded.is_denied("ag_deleted").unwrap());
        assert!(loaded.is_denied("ag_normal").unwrap());
    }
    #[test]
    fn windows_update_startup_cas_serialises_clone_race_and_rejects_aba_revision() {
        let f = Fixture::new();
        let mut token = f.compare();
        let writer = f.store.clone();
        std::thread::spawn(move || writer.deny_absent("ag_concurrent").unwrap())
            .join()
            .unwrap();
        assert!(token.commit(Some(&raw(&[]))).is_err());
        assert!(f.store.is_denied("ag_concurrent").unwrap());
        // Even identical raw state restored by normal writers cannot reset the
        // revision consumed by an earlier token (logical ABA).
        let mut token = f.compare();
        let original = fs::read(&f.store.inner.path).unwrap();
        {
            let mut state = f.store.write_state().unwrap();
            let same = state.clone();
            commit(&f.store.inner.path, &mut state, same).unwrap();
        }
        assert_eq!(fs::read(&f.store.inner.path).unwrap(), original);
        assert!(token.commit(Some(&original)).is_err());
    }
    #[test]
    fn windows_update_startup_cas_refuses_fence_regression_scope_and_raw_divergence() {
        let f = Fixture::new();
        f.store.deny_absent("ag_deleted").unwrap();
        let original = fs::read(&f.store.inner.path).unwrap();
        for replacement in [
            None,
            Some(raw(&[])),
            Some(br#"{"version":1,"entries":[],"denied_agent_ids":["ag_deleted"]}"#.to_vec()),
        ] {
            assert!(f.compare().commit(replacement.as_deref()).is_err());
            assert_eq!(fs::read(&f.store.inner.path).unwrap(), original);
            assert!(f.store.is_denied("ag_deleted").unwrap());
        }
        let expected = read(&f.store.inner.path).unwrap().0;
        assert!(f
            .store
            .update_compare(Uuid::nil(), "ws_fixture", "srv_fixture", &expected)
            .is_err());
        for field in ["workspace_id", "server_id"] {
            let mut wrong = serde_json::to_value(entry()).unwrap();
            wrong[field] = serde_json::json!("foreign");
            let replacement = serde_json::to_vec(&serde_json::json!({
                "version":2, "entries":[wrong], "denied_agent_ids":["ag_deleted"]
            }))
            .unwrap();
            assert!(f.compare().commit(Some(&replacement)).is_err());
            assert_eq!(fs::read(&f.store.inner.path).unwrap(), original);
        }
        let duplicate = serde_json::to_vec(&serde_json::json!({
            "version":2, "entries":[entry(), entry()], "denied_agent_ids":["ag_deleted"]
        }))
        .unwrap();
        assert!(f.compare().commit(Some(&duplicate)).is_err());
        // A changed physical/raw file cannot be accepted by memory-only equality.
        let mut token = f.compare();
        fs::write(&f.store.inner.path, [original.as_slice(), b" "].concat()).unwrap();
        assert!(token.commit(Some(&original)).is_err());
        // A newly loaded memory/path is not adopted to repair this operation.
        assert!(token.commit(Some(&original)).is_err());
    }
    #[test]
    fn windows_update_startup_cas_io_or_readback_unknown_fences_all_normal_writers() {
        for readback_failure in [false, true] {
            let f = Fixture::new();
            let mut token = f.compare();
            let result = token.commit_with(Some(&raw(&["ag_keep"])), |path, state| {
                if readback_failure {
                    persist(path, state)?;
                    fs::write(path, b"invalid injected readback")?;
                    Ok(())
                } else {
                    Err(io::Error::other("injected persistence failure"))
                }
            });
            assert!(result.is_err());
            assert!(token.commit(Some(&raw(&["ag_keep"]))).is_err());
            assert!(f.store.clone().deny_absent("ag_later").is_err());
            assert!(f.store.upsert(entry()).is_err());
            assert!(f.store.remove_deleted("ag_keep").is_err());
            assert!(f.store.remove("ag_keep").is_err());
            assert!(f
                .store
                .update("ag_keep", |_| panic!("unknown must refuse before closure"))
                .is_err());
            assert!(f
                .store
                .update_compare(Uuid::new_v4(), "ws_fixture", "srv_fixture", &None)
                .is_err());
        }
    }
}
