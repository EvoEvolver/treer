//! Disk-only checkpoint history. Never used as recovery input or loaded in RAM.
use crate::store::Store;
use anyhow::{bail, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const DISK_LIMIT: u64 = 128 * 1024 * 1024;
pub const LIVE_LIMIT: u64 = 32 * 1024 * 1024;
pub const HISTORY_LIMIT: u64 = DISK_LIMIT - 2 * LIVE_LIMIT - 2 * 8192;
const FILE_COUNT: usize = 64;
fn checkpoint(_phase: &str) {
    #[cfg(test)]
    if std::env::var("HISTORY_TEST_PHASE").as_deref() == Ok(_phase) {
        std::process::exit(86);
    }
}

fn valid_file(file: &fs::Metadata) -> bool {
    file.is_file()
        && file.nlink() == 1
        && file.uid() == rustix::process::geteuid().as_raw()
        && file.mode() & 0o077 == 0
        && file.len() <= LIVE_LIMIT
}
pub fn prepare(root: &Path, cwd: &Path) -> Result<()> {
    Store::open(root, cwd)?;
    retire_temporaries(root).map_err(|_| anyhow::anyhow!("journal_temporary_cleanup_failed"))
}
fn retire_temporaries(root: &Path) -> Result<()> {
    // Called before startup writes and by the runtime checkpoint writer while
    // its lifetime lease is held. No publication is in progress here.
    for (count, entry) in fs::read_dir(root)?.enumerate() {
        if count == 256 {
            bail!("journal_history_invalid");
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let candidate = name
            .strip_prefix(".rust-spike-")
            .map(|s| (s, LIVE_LIMIT))
            .or_else(|| name.strip_prefix(".rust-stderr-").map(|s| (s, 8192)));
        let Some((suffix, limit)) = candidate else {
            continue;
        };
        let Some(id) = suffix.strip_suffix(".tmp") else {
            continue;
        };
        if uuid::Uuid::parse_str(id)
            .ok()
            .is_none_or(|uuid| uuid.to_string() != id)
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !valid_file(&metadata) || metadata.len() > limit {
            bail!("journal_history_invalid");
        }
        fs::remove_file(entry.path())?;
    }
    File::open(root)?.sync_all()?;
    Ok(())
}
fn entries(dir: &Path) -> Result<Vec<(u64, PathBuf, u64)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entries.len() == FILE_COUNT {
            bail!("journal_history_invalid");
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("journal_history_invalid"))?;
        let sequence = name
            .strip_suffix(".json")
            .or_else(|| name.strip_suffix(".json.tmp"))
            .filter(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| anyhow::anyhow!("journal_history_invalid"))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if !valid_file(&metadata) {
            bail!("journal_history_invalid");
        }
        entries.push((sequence, entry.path(), metadata.len()));
    }
    entries.sort_by_key(|entry| entry.0);
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("journal_history_invalid");
    }
    Ok(entries)
}
pub fn retain(root: &Path, cwd: &Path) -> Result<()> {
    retain_with_limit(root, cwd, HISTORY_LIMIT)
        .map_err(|_| anyhow::anyhow!("journal_history_unavailable"))
}
fn retain_with_limit(root: &Path, cwd: &Path, limit: u64) -> Result<()> {
    Store::open(root, cwd)?;
    retire_temporaries(root)?;
    let mut source = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("rust-spike.json"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let before = source.metadata()?;
    if !valid_file(&before) || before.len() > limit {
        bail!("journal_history_invalid");
    }
    let dir = root.join("rust-journal-history");
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {
            File::open(root)?.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    Store::open(&dir, cwd)?;
    let mut entries = entries(&dir)?;
    let next = entries
        .last()
        .map_or(Some(1), |e| e.0.checked_add(1))
        .ok_or_else(|| anyhow::anyhow!("journal_history_invalid"))?;
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    // Drop only validated, exact history names, oldest first. Live receipts and
    // migration backups are never rotation targets. Partial crash copies are
    // history too; their bytes count, but they are never parsed or replayed.
    while total + before.len() > limit || entries.len() >= FILE_COUNT {
        let (_, path, bytes) = entries.remove(0);
        fs::remove_file(path)?;
        total -= bytes;
        checkpoint("retire");
    }
    File::open(&dir)?.sync_all()?;
    let path = dir.join(format!("{next:020}.json.tmp"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    checkpoint("create");
    // A fixed 32 KiB buffer, regardless of retained disk history size.
    let result = (|| -> Result<()> {
        let mut remaining = before.len();
        let mut buffer = [0_u8; 32 * 1024];
        while remaining != 0 {
            let count = source.read(&mut buffer[..remaining.min(32 * 1024) as usize])?;
            if count == 0 {
                bail!("journal_history_source_changed");
            }
            output.write_all(&buffer[..count])?;
            remaining -= count as u64;
        }
        let after = source.metadata()?;
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            bail!("journal_history_source_changed");
        }
        checkpoint("copy");
        output.sync_all()?;
        checkpoint("file-sync");
        File::open(&dir)?.sync_all()?;
        fs::rename(&path, dir.join(format!("{next:020}.json")))?;
        checkpoint("rename");
        File::open(&dir)?.sync_all()?;
        checkpoint("directory-sync");
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        root: PathBuf,
        cwd: PathBuf,
        base: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let base = fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("rust-history-{}", uuid::Uuid::new_v4()));
            fs::DirBuilder::new().mode(0o700).create(&base).unwrap();
            let root = base.join("private");
            let cwd = base.join("cwd");
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            fs::create_dir(&cwd).unwrap();
            Self { root, cwd, base }
        }
        fn live(&self, bytes: &[u8]) {
            fs::write(self.root.join("rust-spike.json"), bytes).unwrap();
            fs::set_permissions(
                self.root.join("rust-spike.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
    #[test]
    fn disk_history_rotates_oldest_and_never_becomes_recovery_input() {
        let f = Fixture::new();
        for bytes in [b"old1", b"old2", b"new3"] {
            f.live(bytes);
            retain_with_limit(&f.root, &f.cwd, 8).unwrap();
        }
        let dir = f.root.join("rust-journal-history");
        let entries = entries(&dir).unwrap();
        assert_eq!(entries.iter().map(|e| e.0).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(fs::read(&entries[0].1).unwrap(), b"old2");
        assert_eq!(fs::read(&entries[1].1).unwrap(), b"new3");
        // Archive JSON may even be invalid; only the live checkpoint is decoded.
        f.live(b"{\"schema\":1}");
        assert_eq!(
            Store::open(&f.root, &f.cwd)
                .unwrap()
                .load()
                .unwrap()
                .unwrap()["schema"],
            1
        );
    }
    #[test]
    fn production_128_mib_budget_uses_only_metadata_for_sparse_history() {
        let f = Fixture::new();
        let live = b"{\"schema\":1,\"records\":[{\"id\":\"oldest-live\",\"state\":\"queued\"}]}";
        f.live(live);
        let dir = f.root.join("rust-journal-history");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        for n in 1..=4 {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(format!("{n:020}.json")))
                .unwrap();
            file.set_len(LIVE_LIMIT).unwrap();
        }
        retain(&f.root, &f.cwd).unwrap();
        let entries = entries(&dir).unwrap();
        assert_eq!(entries[0].0, 4);
        assert_eq!(entries.last().unwrap().0, 5);
        assert!(entries.iter().map(|e| e.2).sum::<u64>() + 2 * LIVE_LIMIT + 2 * 8192 <= DISK_LIMIT);
        assert_eq!(fs::read(f.root.join("rust-spike.json")).unwrap(), live);
        assert_eq!(
            Store::open(&f.root, &f.cwd)
                .unwrap()
                .load()
                .unwrap()
                .unwrap()["records"][0]["state"],
            "queued"
        );
    }
    #[test]
    fn abandoned_publication_temporary_is_retired_but_unrelated_names_survive() {
        let f = Fixture::new();
        f.live(b"live");
        let temporary = f
            .root
            .join(format!(".rust-spike-{}.tmp", uuid::Uuid::new_v4()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .unwrap()
            .set_len(LIVE_LIMIT)
            .unwrap();
        let unrelated = f.root.join(".rust-spike-unrelated.tmp");
        fs::write(&unrelated, b"keep").unwrap();
        prepare(&f.root, &f.cwd).unwrap();
        assert!(!temporary.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert_eq!(fs::read(f.root.join("rust-spike.json")).unwrap(), b"live");
    }
    #[test]
    fn linked_or_unrecognized_history_refuses_without_touching_live_state() {
        let f = Fixture::new();
        f.live(b"live");
        let dir = f.root.join("rust-journal-history");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        std::os::unix::fs::symlink(
            f.root.join("rust-spike.json"),
            dir.join("00000000000000000001.json"),
        )
        .unwrap();
        assert!(retain(&f.root, &f.cwd).is_err());
        assert_eq!(fs::read(f.root.join("rust-spike.json")).unwrap(), b"live");
    }
    #[test]
    fn crash_at_each_rotation_phase_keeps_live_state_and_restarts() {
        for phase in [
            "retire",
            "create",
            "copy",
            "file-sync",
            "rename",
            "directory-sync",
        ] {
            let f = Fixture::new();
            f.live(b"{\"ok\":1}");
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
            assert_eq!(output.status.code(), Some(86), "phase {phase}");
            assert_eq!(
                Store::open(&f.root, &f.cwd)
                    .unwrap()
                    .load()
                    .unwrap()
                    .unwrap()["ok"],
                1
            );
            retain_with_limit(&f.root, &f.cwd, 16).unwrap();
            let entries = entries(&f.root.join("rust-journal-history")).unwrap();
            assert!(entries.iter().map(|e| e.2).sum::<u64>() <= 16);
        }
    }
    #[test]
    #[ignore = "child helper invoked by crash_at_each_rotation_phase_keeps_live_state_and_restarts"]
    fn crash_fixture_child() {
        let root = PathBuf::from(std::env::var("HISTORY_TEST_ROOT").unwrap());
        let cwd = PathBuf::from(std::env::var("HISTORY_TEST_CWD").unwrap());
        retain_with_limit(&root, &cwd, 16).unwrap();
        panic!("crash injection did not execute");
    }
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "run alone with --ignored --nocapture for isolated RSS measurement"]
    fn resident_memory_child() {
        fn rss() -> i64 {
            let output = std::process::Command::new("/bin/ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .unwrap();
            String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        }
        let f = Fixture::new();
        f.live(b"{\"ok\":1}");
        let mut live = OpenOptions::new()
            .append(true)
            .open(f.root.join("rust-spike.json"))
            .unwrap();
        let padding = [b' '; 32 * 1024];
        let mut remaining = LIVE_LIMIT - 8;
        while remaining != 0 {
            let count = remaining.min(padding.len() as u64) as usize;
            live.write_all(&padding[..count]).unwrap();
            remaining -= count as u64;
        }
        live.sync_all().unwrap();
        drop(live);
        let dir = f.root.join("rust-journal-history");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        for n in 1..=3 {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(format!("{n:020}.json")))
                .unwrap()
                .set_len(LIVE_LIMIT)
                .unwrap();
        }
        // Exactly 128 MiB of file lengths before rotation; the current 32 MiB
        // checkpoint is really copied, while old sparse history is never read.
        let before = rss();
        retain(&f.root, &f.cwd).unwrap();
        let after = rss();
        assert!(
            entries(&dir).unwrap().iter().map(|e| e.2).sum::<u64>() + 2 * LIVE_LIMIT + 2 * 8192
                <= DISK_LIMIT
        );
        println!("128 MiB disk fixture incl. real 32 MiB checkpoint copy: RSS before={before} KiB after={after} KiB; fixed 32 KiB copy buffer");
        assert!(
            after - before < 16 * 1024,
            "disk retention must not enter resident memory"
        );
    }
}
