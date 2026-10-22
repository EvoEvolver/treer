//! Windows disk-only history; never parsed or selected as recovery input.
use crate::store::{check_file, is_not_found, temporary, Store, LIVE_LIMIT as MAX_LIVE};
use anyhow::{bail, Result};
use std::{
    fs,
    io::{Read, Write},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub const DISK_LIMIT: u64 = 128 * 1024 * 1024;
pub const LIVE_LIMIT: u64 = MAX_LIVE;
pub const HISTORY_LIMIT: u64 = DISK_LIMIT - 2 * LIVE_LIMIT - 2 * 8192;
const FILE_COUNT: usize = 64;

fn checkpoint(_phase: &str) {
    #[cfg(test)]
    if std::env::var("HISTORY_TEST_PHASE").as_deref() == Ok(_phase) {
        std::process::exit(86);
    }
}
fn recognized_temporary(name: &str) -> Option<(&'static str, u64)> {
    for (kind, target, limit) in [
        ("spike", "rust-spike.json", LIVE_LIMIT),
        ("stderr", "rust-provider-stderr.log", 8192),
        ("lease", ".registry.lock", 0),
    ] {
        let prefix = format!(".treer-transfer-xfer_rust-{kind}-");
        if let Some(id) = name
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(".tmp"))
        {
            if uuid::Uuid::parse_str(id)
                .ok()
                .is_some_and(|id_value| id_value.to_string() == id)
            {
                return Some((target, limit));
            }
        }
    }
    None
}
pub fn prepare(root: &Path, cwd: &Path) -> Result<()> {
    let store = Store::open(root, cwd)?;
    retire_temporaries(&store).map_err(|_| anyhow::anyhow!("journal_temporary_cleanup_failed"))
}
fn retire_temporaries(store: &Store) -> Result<()> {
    // Caller holds the process lifetime lease. No publication is in flight.
    for (count, entry) in fs::read_dir(&store.root)?.enumerate() {
        if count == 256 {
            bail!("journal_history_invalid");
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((target, limit)) = recognized_temporary(name) else {
            continue;
        };
        let file = treer_platform::machine_file::open_protected(&entry.path())?;
        check_file(&file, &entry.path(), limit)?;
        drop(file);
        store.directory().cleanup_staging(target, name)?;
    }
    Ok(())
}
fn sequence(name: &str) -> Option<u64> {
    let number = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".json.tmp"));
    let number = if let Some(number) = number {
        number
    } else {
        let suffix = name.strip_prefix(".treer-transfer-xfer_rust-history-")?;
        let (number, uuid) = suffix.split_once('-')?;
        let id = uuid.strip_suffix(".tmp")?;
        if uuid::Uuid::parse_str(id).ok()?.to_string() != id {
            return None;
        }
        number
    };
    if number.len() != 20 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}
fn entries(store: &Store) -> Result<Vec<(u64, PathBuf, u64)>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(&store.root)? {
        if result.len() == FILE_COUNT {
            bail!("journal_history_invalid");
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("journal_history_invalid"))?;
        let number = sequence(&name).ok_or_else(|| anyhow::anyhow!("journal_history_invalid"))?;
        // Exact-name validation precedes opening. Retained parent handles and
        // the protected-open API reject links/reparse points, including temps.
        let file = treer_platform::machine_file::open_protected(&entry.path())?;
        check_file(&file, &entry.path(), LIVE_LIMIT)?;
        result.push((number, entry.path(), file.metadata()?.len()));
    }
    result.sort_by_key(|entry| entry.0);
    if result.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("journal_history_invalid");
    }
    Ok(result)
}
pub fn retain(root: &Path, cwd: &Path) -> Result<()> {
    retain_with_limit(root, cwd, HISTORY_LIMIT)
        .map_err(|_| anyhow::anyhow!("journal_history_unavailable"))
}
fn retain_with_limit(root: &Path, cwd: &Path, limit: u64) -> Result<()> {
    let store = Store::open(root, cwd)?;
    retire_temporaries(&store)?;
    let mut source = match store.read_checked("rust-spike.json", LIVE_LIMIT) {
        Ok(source) => source,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let before = source.metadata()?;
    if before.len() > limit {
        bail!("journal_history_invalid");
    }
    let directory = root.join("rust-journal-history");
    match fs::create_dir(&directory) {
        Ok(()) => {
            // Hold the new physical directory before applying its private
            // descriptor. Existing directories are never permission-repaired.
            let guard = treer_platform::machine_file::Root::open(&directory, &[])?;
            treer_platform::secure_directory(&directory)?;
            drop(guard);
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let history = Store::open(&directory, store.cwd())?;
    let mut previous = entries(&history)?;
    let next = previous
        .last()
        .map_or(Some(1), |entry| entry.0.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("journal_history_invalid"))?;
    let mut total: u64 = previous.iter().map(|entry| entry.2).sum();
    while total + before.len() > limit || previous.len() >= FILE_COUNT {
        let (_, path, bytes) = previous.remove(0);
        // Every candidate was checked for private DACL, file type and links.
        fs::remove_file(path)?;
        total -= bytes;
        checkpoint("retire");
    }
    let name = format!("{next:020}.json");
    let temporary = temporary(&format!("history-{next:020}"));
    let mut staged = history.directory().stage(&name, &temporary, 0o600)?;
    checkpoint("create");
    let mut remaining = before.len();
    let mut buffer = [0_u8; 32 * 1024];
    while remaining != 0 {
        let count = source.read(&mut buffer[..remaining.min(32 * 1024) as usize])?;
        if count == 0 {
            bail!("journal_history_source_changed");
        }
        staged.file_mut().write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    let after = source.metadata()?;
    if before.len() != after.len()
        || before.last_write_time() != after.last_write_time()
        || before.creation_time() != after.creation_time()
    {
        bail!("journal_history_source_changed");
    }
    checkpoint("copy");
    staged.file_mut().sync_all()?;
    checkpoint("file-sync");
    staged.commit(false)?;
    checkpoint("rename");
    // commit includes native write-through publication; no Unix directory
    // fsync is silently replaced with a no-op on Windows.
    checkpoint("directory-sync");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Fixture;
    use serde_json::json;

    #[test]
    fn history_rotates_bounded_metadata_without_loading_archive_json() {
        let f = Fixture::new();
        let store = f.store();
        store.save(&json!({"n":1})).unwrap();
        retain_with_limit(&f.root, &f.cwd, 16).unwrap();
        store.save(&json!({"n":2})).unwrap();
        retain_with_limit(&f.root, &f.cwd, 16).unwrap();
        store.save(&json!({"n":3})).unwrap();
        retain_with_limit(&f.root, &f.cwd, 16).unwrap();
        let history = Store::open(&f.root.join("rust-journal-history"), &f.cwd).unwrap();
        let retained = entries(&history).unwrap();
        assert_eq!(retained.iter().map(|e| e.0).collect::<Vec<_>>(), vec![2, 3]);
        assert!(retained.iter().map(|e| e.2).sum::<u64>() <= 16);
        assert_eq!(store.load().unwrap().unwrap()["n"], 3);
    }
    #[test]
    fn exact_abandoned_temporaries_only_and_invalid_archive_refuse() {
        let f = Fixture::new();
        let store = f.store();
        store.save(&json!({"live":true})).unwrap();
        let temp = temporary("spike");
        let path = f.root.join(&temp);
        fs::write(&path, b"partial").unwrap();
        treer_platform::secure_file(&path).unwrap();
        let other = f
            .root
            .join(".treer-transfer-xfer_rust-spike-not-a-uuid.tmp");
        fs::write(&other, b"keep").unwrap();
        prepare(&f.root, &f.cwd).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(other).unwrap(), b"keep");
        retain(&f.root, &f.cwd).unwrap();
        let dir = f.root.join("rust-journal-history");
        fs::write(dir.join("unrecognized"), b"do not retire").unwrap();
        assert!(retain(&f.root, &f.cwd).is_err());
        assert_eq!(store.load().unwrap().unwrap()["live"], true);
        assert_eq!(
            fs::read(dir.join("unrecognized")).unwrap(),
            b"do not retire"
        );
    }
    #[test]
    fn history_rejects_linked_or_public_files_without_touching_live() {
        let f = Fixture::new();
        f.store().save(&json!({"live":true})).unwrap();
        retain(&f.root, &f.cwd).unwrap();
        let path = f
            .root
            .join("rust-journal-history/00000000000000000001.json");
        fs::hard_link(&path, f.base.join("history-alias")).unwrap();
        assert!(retain(&f.root, &f.cwd).is_err());
        fs::remove_file(f.base.join("history-alias")).unwrap();
        treer_platform::apply_sddl_dacl(&path, "D:P(A;;GA;;;WD)").unwrap();
        assert!(retain(&f.root, &f.cwd).is_err());
        assert_eq!(f.store().load().unwrap().unwrap()["live"], true);
    }
    #[test]
    fn metadata_budget_retires_sparse_history_and_preserves_live_checkpoint() {
        let f = Fixture::new();
        let store = f.store();
        store.save(&json!({"live":true})).unwrap();
        let directory = f.root.join("rust-journal-history");
        treer_platform::secure_directory(&directory).unwrap();
        let history = Store::open(&directory, &f.cwd).unwrap();
        for n in 1..=4 {
            let mut stage = history
                .directory()
                .stage(&format!("{n:020}.json"), &temporary("test"), 0o600)
                .unwrap();
            stage.file_mut().set_len(LIVE_LIMIT).unwrap();
            stage.commit(false).unwrap();
        }
        retain(&f.root, &f.cwd).unwrap();
        let retained = entries(&history).unwrap();
        assert_eq!(retained.first().unwrap().0, 4);
        assert_eq!(retained.last().unwrap().0, 5);
        assert!(
            retained.iter().map(|e| e.2).sum::<u64>() + 2 * LIVE_LIMIT + 2 * 8192 <= DISK_LIMIT
        );
        assert_eq!(store.load().unwrap().unwrap()["live"], true);
    }

    #[test]
    fn crash_at_each_rotation_phase_preserves_live_and_bounded_restart() {
        for phase in [
            "retire",
            "create",
            "copy",
            "file-sync",
            "rename",
            "directory-sync",
        ] {
            let f = Fixture::new();
            f.store().save(&json!({"ok":1})).unwrap();
            retain_with_limit(&f.root, &f.cwd, 16).unwrap();
            retain_with_limit(&f.root, &f.cwd, 16).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "disk_history::tests::crash_fixture_child",
                ])
                .env("HISTORY_TEST_ROOT", &f.root)
                .env("HISTORY_TEST_CWD", &f.cwd)
                .env("HISTORY_TEST_PHASE", phase)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(86), "{phase}");
            assert_eq!(f.store().load().unwrap().unwrap()["ok"], 1);
            retain_with_limit(&f.root, &f.cwd, 16).unwrap();
            let history = Store::open(&f.root.join("rust-journal-history"), &f.cwd).unwrap();
            assert!(entries(&history).unwrap().iter().map(|e| e.2).sum::<u64>() <= 16);
        }
    }
    #[test]
    #[ignore = "owned child of rotation crash test"]
    fn crash_fixture_child() {
        let root = PathBuf::from(std::env::var_os("HISTORY_TEST_ROOT").unwrap());
        let cwd = PathBuf::from(std::env::var_os("HISTORY_TEST_CWD").unwrap());
        retain_with_limit(&root, &cwd, 16).unwrap();
        panic!("crash injection did not execute");
    }

    #[test]
    fn history_name_and_temporary_grammar_are_closed() {
        assert_eq!(sequence("00000000000000000001.json"), Some(1));
        assert!(sequence("../00000000000000000001.json").is_none());
        assert!(sequence("00000000000000000001.json:alias").is_none());
        assert!(recognized_temporary(".rust-spike-unrelated.tmp").is_none());
        assert!(recognized_temporary(&temporary("spike")).is_some());
    }
}
