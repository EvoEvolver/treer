//! One-way local Node import. Source files are never changed by import.
use crate::store::Store;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const BACKUP: &str = "node-backup-v1";
const FILE_LIMIT: u64 = 32 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    version: u32,
    agent: String,
    cwd: PathBuf,
    files: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    bytes: u64,
    sha256: String,
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn source(cwd: &Path, agent: &str) -> Result<PathBuf> {
    if agent.is_empty()
        || agent.len() > 128
        || !agent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("node_import_identity_required");
    }
    let mut path = cwd.to_path_buf();
    for part in [".treer", "agents", agent] {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m)
                if m.is_dir()
                    && !m.file_type().is_symlink()
                    && m.uid() == rustix::process::geteuid().as_raw()
                    && m.mode() & 0o022 == 0 => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(path),
            _ => bail!("node_import_unsafe_source"),
        }
    }
    Ok(path)
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.mode() & 0o077 != 0
        || before.len() > limit
    {
        bail!("node_import_unsafe_file");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > limit
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        bail!("node_import_source_changed");
    }
    Ok(bytes)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn inventory(dir: &Path) -> Result<Vec<Entry>> {
    let mut files = Vec::new();
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("node_import_unsupported_file"))?;
        if !valid_name(&name) || files.len() == 32 {
            bail!("node_import_file_limit");
        }
        let bytes = read(&entry.path(), FILE_LIMIT)?;
        total += bytes.len() as u64;
        if total > TOTAL_LIMIT {
            bail!("node_import_file_limit");
        }
        files.push(Entry {
            name,
            bytes: bytes.len() as u64,
            sha256: digest(&bytes),
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}
fn load_backup(root: &Path, cwd: &Path, agent: &str) -> Result<Backup> {
    let dir = root.join(BACKUP);
    let m = fs::symlink_metadata(&dir)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.mode() & 0o077 != 0
        || m.uid() != rustix::process::geteuid().as_raw()
    {
        bail!("node_backup_invalid");
    }
    let bytes = read(&dir.join("manifest.json"), 64 * 1024)?;
    let backup: Backup = serde_json::from_value(
        crate::bounded_json::decode(&bytes).map_err(|_| anyhow::anyhow!("node_backup_invalid"))?,
    )?;
    if backup.version != 1 || backup.agent != agent || backup.cwd != cwd || backup.files.len() > 32
    {
        bail!("node_backup_invalid");
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0_u64;
    for entry in &backup.files {
        if !valid_name(&entry.name)
            || entry.name == "manifest.json"
            || !seen.insert(&entry.name)
            || entry.bytes > FILE_LIMIT
        {
            bail!("node_backup_invalid");
        }
        total = total
            .checked_add(entry.bytes)
            .ok_or_else(|| anyhow::anyhow!("node_backup_invalid"))?;
        if total > TOTAL_LIMIT {
            bail!("node_backup_invalid");
        }
        let bytes = read(&dir.join(&entry.name), FILE_LIMIT)?;
        if bytes.len() as u64 != entry.bytes || digest(&bytes) != entry.sha256 {
            bail!("node_backup_invalid");
        }
    }
    Ok(backup)
}
fn empty_node_images(root: &Path) -> Result<Option<(u64, u64)>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(root.join("images"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o077 != 0 {
        bail!("node_import_unsafe_images");
    }
    for entry in rustix::fs::Dir::read_from(&file)? {
        let entry = entry?;
        if ![b".".as_slice(), b"..".as_slice()].contains(&entry.file_name().to_bytes()) {
            bail!("node_import_images_not_empty");
        }
    }
    Ok(Some((m.dev(), m.ino())))
}
// Claude's local usage sidecar can survive a later Codex session. Only an
// explicitly action-free checkpoint is inert; preserve its exact backup bytes
// for Node rollback, without interpreting it as a Codex goal or replaying it.
fn inert_goal_usage(bytes: &[u8]) -> Result<()> {
    let value = crate::bounded_json::decode(bytes)?;
    crate::runtime::node_fields(&value, &["sessionId", "goalKey", "goal", "pendingExit", "tokensUsed", "timeUsedSeconds", "resultTokens"])?;
    if !value["sessionId"].as_str().is_some_and(|s| !s.is_empty() && s.len() <= 1024 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        || value["goalKey"] != ""
        || ["goal", "pendingExit", "tokensUsed", "timeUsedSeconds"].into_iter().any(|key| !value.get(key).is_some_and(Value::is_null))
        || !value["resultTokens"].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
    {
        bail!("node_import_goal_usage_not_inert");
    }
    Ok(())
}
pub fn prepare(
    root: &Path,
    cwd: &Path,
    agent: &str,
    explicit_session: Option<&str>,
    allow_import: bool,
) -> Result<()> {
    prepare_inner(root, cwd, agent, explicit_session, allow_import,None).map_err(|_| {
        anyhow::anyhow!(
            "node_state_import_failed; source preserved; inspect local backup before retry"
        )
    })
}
pub fn prepare_claude(root:&Path,cwd:&Path,agent:&str,session:Option<&str>,seed:Option<&Path>)->Result<()> {
    prepare_inner(root,cwd,agent,session,seed.is_some(),seed).map_err(|_|anyhow::anyhow!("claude_state_import_failed; source preserved; no fresh-session fallback"))
}
fn prepare_inner(
    root: &Path,
    cwd: &Path,
    agent: &str,
    explicit_session: Option<&str>,
    allow_import: bool,
    claude_seed:Option<&Path>,
) -> Result<()> {
    let store = Store::open(root, cwd)?;
    if fs::symlink_metadata(root.join(".node-backup-restored")).is_ok() {
        bail!("node_import_backup_already_restored");
    }
    match fs::symlink_metadata(root.join("rust-spike.json")) {
        Ok(_) => {
            // Runtime validates Rust state; a prior import must still match its
            // source backup, otherwise Node ran again and histories diverged.
            if fs::symlink_metadata(root.join(BACKUP)).is_ok() {
                let backup = load_backup(root, cwd, agent)?;
                let dir = source(cwd, agent)?;
                if serde_json::to_value(inventory(&dir)?)? != serde_json::to_value(&backup.files)? {
                    bail!("node_import_source_changed");
                }
                empty_node_images(root)?;
            }
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // Standalone Rust experiments have no managed Node state to import.
    if agent.is_empty() {
        if fs::symlink_metadata(cwd.join(".treer/agents/standalone")).is_ok() {
            bail!("node_import_identity_required");
        }
        return Ok(());
    }
    for name in ["queue.json", "agent-ui.json", "codex-ui.json"] {
        if fs::symlink_metadata(root.join(name)).is_ok() {
            bail!("node_import_unsupported_source_placement");
        }
    }
    let dir = source(cwd, agent)?;
    if !dir.exists() {
        return Ok(());
    }
    let files = inventory(&dir)?;
    if files.is_empty() {
        return Ok(());
    }
    if !allow_import {
        bail!("node_import_explicit_action_required");
    }
    let images = empty_node_images(root)?;
    if files.iter().any(|e| e.name == "manifest.json") {
        bail!("node_import_unsupported_file");
    }
    let backup_dir = root.join(BACKUP);
    match fs::DirBuilder::new().mode(0o700).create(&backup_dir) {
        Ok(()) => {
            for entry in &files {
                let bytes = read(&dir.join(&entry.name), FILE_LIMIT)?;
                if digest(&bytes) != entry.sha256 {
                    bail!("node_import_source_changed");
                }
                write_new(&backup_dir.join(&entry.name), &bytes)?;
            }
            write_new(
                &backup_dir.join("manifest.json"),
                &serde_json::to_vec(&Backup {
                    version: 1,
                    agent: agent.into(),
                    cwd: cwd.into(),
                    files,
                })?,
            )?;
            File::open(&backup_dir)?.sync_all()?;
            File::open(root)?.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let backup = load_backup(root, cwd, agent)?;
    let current = inventory(&dir)?;
    if serde_json::to_value(&current)? != serde_json::to_value(&backup.files)? {
        bail!("node_import_source_changed");
    }
    let thread_name = if backup.files.iter().any(|e| e.name == "agent-ui.json") {
        "agent-ui.json"
    } else {
        "codex-ui.json"
    };
    let thread: Value =
        crate::bounded_json::decode(&read(&backup_dir.join(thread_name), 64 * 1024)?)
            .map_err(|_| anyhow::anyhow!("node_import_invalid_thread"))?;
    if explicit_session.is_some_and(|s| thread["threadId"].as_str() != Some(s)) {
        bail!("node_import_session_mismatch");
    }
    let queue = if backup.files.iter().any(|e| e.name == "queue.json") {
        crate::bounded_json::decode(&read(&backup_dir.join("queue.json"), FILE_LIMIT)?)
            .map_err(|_| anyhow::anyhow!("node_import_invalid_queue"))?
    } else {
        json!({"version":1,"records":[]})
    };
    // Live/pending local goal guards still need their own parity adapter.
    // Inert usage is backup-only, never a source of Codex goal authority.
    for entry in &backup.files {
        if entry.name == format!("{thread_name}.goal-usage.json") {
            inert_goal_usage(&read(&backup_dir.join(&entry.name), 64 * 1024)?)?;
            continue;
        }
        if !["agent-ui.json", "codex-ui.json", "queue.json"].contains(&entry.name.as_str()) {
            bail!("node_import_sidecar_unsupported");
        }
    }
    let mut state = crate::runtime::import_node_queue(&thread, &queue)?;
    if let Some(seed)=claude_seed {
        let seed=crate::bounded_json::decode(&read(seed,16*1024*1024)?)?;
        let hash=backup.files.iter().find(|e|e.name=="queue.json").map(|e|e.sha256.as_str()).ok_or_else(||anyhow::anyhow!("claude_seed_queue_missing"))?;
        crate::claude::import_history(&mut state,&seed,agent,cwd,hash)?;
    }
    // Recheck source after conversion, before the only publication. Same-account
    // concurrent mutation remains an operator precondition, as with Node.
    if serde_json::to_value(inventory(&dir)?)? != serde_json::to_value(&backup.files)? {
        bail!("node_import_source_changed");
    }
    if empty_node_images(root)? != images {
        bail!("node_import_source_changed");
    }
    store.save(&state)?;
    eprintln!("Rust launcher: node_state_imported; byte_backup_retained");
    Ok(())
}
pub fn restore(root: &Path, cwd: &Path, agent: &str) -> Result<()> {
    (|| -> Result<()> {
        Store::open(root, cwd)?;
        let backup = load_backup(root, cwd, agent)?;
        let dir = source(cwd, agent)?;
        if !dir.is_dir() {
            bail!("node_restore_source_missing");
        }
        // Fence stale Rust state before the first restore rename; a crash part
        // way through restore can be retried, but never restart the old queue.
        let marker = root.join(".node-backup-restored");
        if !marker.exists() {
            write_new(&marker, b"restore_requested\n")?;
        }
        File::open(root)?.sync_all()?;
        for entry in backup.files {
            let bytes = read(&root.join(BACKUP).join(&entry.name), FILE_LIMIT)?;
            let temporary = dir.join(format!(".node-restore-{}.tmp", uuid::Uuid::new_v4()));
            write_new(&temporary, &bytes)?;
            fs::rename(&temporary, dir.join(entry.name))?;
            File::open(&dir)?.sync_all()?;
        }
        Ok(())
    })()
    .map_err(|_| {
        anyhow::anyhow!("node_state_restore_failed; retry exact backup with both launchers stopped")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        cwd: PathBuf,
        private: PathBuf,
        source: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("node-import-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let cwd = root.join("cwd");
            let private = root.join("private");
            fs::create_dir(&cwd).unwrap();
            fs::create_dir(&private).unwrap();
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
            let source = cwd.join(".treer/agents/ag_test");
            fs::create_dir_all(&source).unwrap();
            write_new(
                &source.join("agent-ui.json"),
                b"{\"threadId\":\"thread-test\"}\n",
            )
            .unwrap();
            write_new(
                &source.join("queue.json"),
                b"{\"version\":1,\"records\":[]}\n",
            )
            .unwrap();
            Self {
                root,
                cwd,
                private,
                source,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn inert_goal_sidecar_backed_up_and_restored_but_never_becomes_goal() {
        let f = Fixture::new();
        let bytes = br#"{"sessionId":"old-claude-session","goalKey":"","goal":null,"pendingExit":null,"tokensUsed":null,"timeUsedSeconds":null,"resultTokens":12345}"#;
        let name = "agent-ui.json.goal-usage.json";
        write_new(&f.source.join(name), bytes).unwrap();
        prepare(&f.private, &f.cwd, "ag_test", Some("thread-test"), true).unwrap();
        assert_eq!(fs::read(f.private.join(BACKUP).join(name)).unwrap(), bytes);
        let saved = Store::open(&f.private, &f.cwd).unwrap().load().unwrap().unwrap();
        assert!(saved["goals"].is_null());
        assert_eq!(saved["threadId"], "thread-test");
        restore(&f.private, &f.cwd, "ag_test").unwrap();
        assert_eq!(fs::read(f.source.join(name)).unwrap(), bytes);
    }
    #[test]
    fn goal_sidecar_requires_explicit_inert_shape_and_preserves_refused_backup() {
        let base = json!({"sessionId":"old-session","goalKey":"","goal":null,"pendingExit":null,"tokensUsed":null,"timeUsedSeconds":null,"resultTokens":0});
        let mut faults = Vec::new();
        for (key, value) in [("goal", json!({"status":"active"})), ("pendingExit", json!({"status":"done"})), ("goalKey", json!("old-goal")), ("tokensUsed", json!(0)), ("timeUsedSeconds", json!(0)), ("resultTokens", json!(-1)), ("resultTokens", json!("0")), ("sessionId", json!("")), ("unknown", Value::Null)] {
            let mut invalid = base.clone(); invalid[key] = value; faults.push(serde_json::to_vec(&invalid).unwrap());
        }
        let mut missing = base.clone(); missing.as_object_mut().unwrap().remove("pendingExit");
        faults.push(serde_json::to_vec(&missing).unwrap());
        faults.push(br#"{"goal":null,"goal":{"status":"active"}}"#.to_vec());
        for bytes in faults {
            let f = Fixture::new();
            let name = "agent-ui.json.goal-usage.json";
            write_new(&f.source.join(name), &bytes).unwrap();
            assert!(prepare(&f.private, &f.cwd, "ag_test", None, true).is_err());
            assert!(!f.private.join("rust-spike.json").exists());
            assert_eq!(fs::read(f.source.join(name)).unwrap(), bytes);
            assert_eq!(fs::read(f.private.join(BACKUP).join(name)).unwrap(), bytes);
        }
    }
    #[test]
    fn empty_images_only_and_no_follow() {
        let f = Fixture::new();
        let images = f.private.join("images");
        fs::DirBuilder::new().mode(0o700).create(&images).unwrap();
        assert!(empty_node_images(&f.private).unwrap().is_some());
        write_new(&images.join(".draft"), b"retained").unwrap();
        assert!(prepare(&f.private, &f.cwd, "ag_test", None, true).is_err());
        assert!(!f.private.join("rust-spike.json").exists());
        assert_eq!(fs::read(images.join(".draft")).unwrap(), b"retained");
        let linked = Fixture::new();
        std::os::unix::fs::symlink(&images, linked.private.join("images")).unwrap();
        assert!(empty_node_images(&linked.private).is_err());
    }
    #[test]
    fn import_backs_up_exact_bytes_restarts_and_restores_without_provider() {
        let f = Fixture::new();
        let original = fs::read(f.source.join("queue.json")).unwrap();
        assert!(prepare(&f.private, &f.cwd, "ag_test", None, false).is_err());
        assert!(!f.private.join(BACKUP).exists());
        prepare(&f.private, &f.cwd, "ag_test", None, true).unwrap();
        assert_eq!(
            fs::read(f.private.join(BACKUP).join("queue.json")).unwrap(),
            original
        );
        assert_eq!(fs::read(f.source.join("queue.json")).unwrap(), original);
        assert_eq!(
            Store::open(&f.private, &f.cwd)
                .unwrap()
                .load()
                .unwrap()
                .unwrap()["threadId"],
            "thread-test"
        );
        let journal = fs::read(f.private.join("rust-spike.json")).unwrap();
        fs::write(f.source.join("queue.json"), b"changed after import").unwrap();
        assert!(
            prepare(&f.private, &f.cwd, "ag_test", None, false).is_err(),
            "divergent Node history must refuse restart"
        );
        assert_eq!(
            fs::read(f.private.join("rust-spike.json")).unwrap(),
            journal
        );
        restore(&f.private, &f.cwd, "ag_test").unwrap();
        assert_eq!(fs::read(f.source.join("queue.json")).unwrap(), original);
        assert!(
            prepare(&f.private, &f.cwd, "ag_test", None, true).is_err(),
            "rollback must fence a stale Rust journal"
        );
        restore(&f.private, &f.cwd, "ag_test").unwrap();
    }
    #[test]
    fn mismatched_session_preserves_backup_and_never_publishes() {
        let f = Fixture::new();
        assert!(prepare(&f.private, &f.cwd, "ag_test", Some("wrong"), true).is_err());
        assert!(load_backup(&f.private, &f.cwd, "ag_test").is_ok());
        assert!(!f.private.join("rust-spike.json").exists());
        // A complete backup supports retry after a pre-publication failure.
        prepare(&f.private, &f.cwd, "ag_test", None, true).unwrap();
    }
    #[test]
    fn tampered_backup_and_linked_source_are_refused_unchanged() {
        let f = Fixture::new();
        assert!(prepare(&f.private, &f.cwd, "ag_test", Some("wrong"), true).is_err());
        fs::write(f.private.join(BACKUP).join("queue.json"), b"tampered").unwrap();
        assert!(prepare(&f.private, &f.cwd, "ag_test", None, true).is_err());
        assert!(restore(&f.private, &f.cwd, "ag_test").is_err());
        assert!(!f.private.join("rust-spike.json").exists());
        assert_eq!(
            fs::read(f.source.join("queue.json")).unwrap(),
            b"{\"version\":1,\"records\":[]}\n"
        );
        let linked = Fixture::new();
        fs::remove_file(linked.source.join("queue.json")).unwrap();
        std::os::unix::fs::symlink(
            linked.source.join("agent-ui.json"),
            linked.source.join("queue.json"),
        )
        .unwrap();
        assert!(prepare(&linked.private, &linked.cwd, "ag_test", None, true).is_err());
        assert!(!linked.private.join("rust-spike.json").exists());
    }
}
