//! Bounded procfs state observation while the owned group leader is unreaped.
//! Never read cmdline/environ/exe, and never signal a discovered numeric PID.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

fn invalid() -> io::Error {
    io::Error::other("child group quiescence unavailable")
}

fn state(bytes: &[u8], expected: u32) -> io::Result<(u32, bool)> {
    // comm is arbitrary bytes, including spaces, ')' and non-UTF8. Only parse
    // the ASCII suffix after the LAST ') '; don't decode or retain comm.
    let prefix = format!("{expected} (");
    if !bytes.starts_with(prefix.as_bytes()) {
        return Err(invalid());
    }
    let end = bytes
        .windows(2)
        .rposition(|b| b == b") ")
        .ok_or_else(invalid)?;
    let suffix = std::str::from_utf8(&bytes[end + 2..]).map_err(|_| invalid())?;
    let mut fields = suffix.split_ascii_whitespace();
    let state = fields.next().ok_or_else(invalid)?;
    let parent = fields.next().ok_or_else(invalid)?;
    let group = fields.next().ok_or_else(invalid)?;
    if state.len() != 1 || parent.parse::<u32>().is_err() {
        return Err(invalid());
    }
    let group = group.parse::<u32>().map_err(|_| invalid())?;
    Ok((group, matches!(state, "Z" | "X")))
}

fn read_state(path: &Path, pid: u32) -> io::Result<(u32, bool)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 8192 {
        return Err(invalid());
    }
    state(&bytes, pid)
}

fn threads_quiescent(process: &Path, group: u32) -> io::Result<bool> {
    let mut observed = false;
    let entries = match std::fs::read_dir(process.join("task")) {
        Ok(entries) => entries,
        // In particular, Linux can make task inventory unavailable after the
        // main thread exits while siblings still execute. Never infer death.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(invalid()),
    };
    for (count, entry) in entries.enumerate() {
        if count >= 65536 {
            return Err(invalid());
        }
        let entry = entry.map_err(|_| invalid())?;
        let tid = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(invalid)?;
        let (thread_group, dead) = match read_state(&entry.path().join("stat"), tid) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(invalid()),
        };
        if thread_group != group {
            return Err(invalid());
        }
        if !dead {
            return Ok(false);
        }
        observed = true;
    }
    Ok(observed)
}

pub(super) fn quiescent(group: rustix::process::Pid) -> io::Result<bool> {
    quiescent_at(Path::new("/proc"), group.as_raw_nonzero().get() as u32)
}

fn quiescent_at(root: &Path, group: u32) -> io::Result<bool> {
    let mut leader = false;
    // Procfs has no atomic per-group inventory API. SIGKILL was submitted first
    // while the leader stays reserved. Cooperating members must stay in their
    // group and remain signalable. Linux kill(-pgid) holds tasklist_lock while
    // submitting signals; copy_process tests fatal_signal_pending under that
    // lock before publishing a fork. Sources: torvalds/linux kernel/signal.c
    // kill_something_info/__kill_pgrp_info; kernel/fork.c copy_process. This is
    // not an atomic procfs snapshot or containment of uncooperative children.
    // Missing/changed/denied metadata is never used as proof of exit.
    for (count, entry) in std::fs::read_dir(root).map_err(|_| invalid())?.enumerate() {
        if count >= 65536 {
            return Err(invalid());
        }
        let entry = entry.map_err(|_| invalid())?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let (member_group, dead) = match read_state(&entry.path().join("stat"), pid) {
            Ok(state) => state,
            // Unrelated processes can disappear; their missing stat is not
            // used to attest our group. Require the owned leader below.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(invalid()),
        };
        if member_group == group {
            // /proc/<tgid>/stat describes only its leader thread. A zombie
            // leader can coexist with a live sibling; inspect EVERY task.
            if !dead || !threads_quiescent(&entry.path(), group)? {
                return Ok(false);
            }
            leader |= pid == group;
        }
    }
    Ok(leader)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zombie_leader_with_live_sibling_never_grants_quiescence() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let root =
            std::env::temp_dir().join(format!("treer-task-inventory-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let fixture = Fixture(root);
        let process = fixture.0.join("123");
        let leader = process.join("task/123");
        let sibling = process.join("task/124");
        std::fs::create_dir_all(&leader).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(process.join("stat"), b"123 (leader) Z 1 123").unwrap();
        std::fs::write(leader.join("stat"), b"123 (leader) Z 1 123").unwrap();
        // Fixed procfs fixture for pthread_exit(main) with a surviving sibling.
        // Also represents the post-SIGKILL window before that sibling exits;
        // no dependence on a scheduler race or uninterruptible real process.
        for status in ["R", "D", "S", "T", "I"] {
            std::fs::write(sibling.join("stat"), format!("124 (worker) {status} 1 123")).unwrap();
            assert!(!quiescent_at(&fixture.0, 123).unwrap());
        }
        for status in ["Z", "X"] {
            std::fs::write(sibling.join("stat"), format!("124 (worker) {status} 1 123")).unwrap();
            assert!(quiescent_at(&fixture.0, 123).unwrap());
        }
        std::fs::write(sibling.join("stat"), b"124 (worker) Z 1 999").unwrap();
        assert!(quiescent_at(&fixture.0, 123).is_err());
        std::fs::write(sibling.join("stat"), b"123 (worker) Z 1 123").unwrap();
        assert!(quiescent_at(&fixture.0, 123).is_err());
        std::fs::remove_file(sibling.join("stat")).unwrap();
        assert!(!quiescent_at(&fixture.0, 123).unwrap());
        std::fs::remove_dir_all(process.join("task")).unwrap();
        std::fs::create_dir(process.join("task")).unwrap();
        assert!(!quiescent_at(&fixture.0, 123).unwrap());
        std::fs::remove_dir(process.join("task")).unwrap();
        assert!(!quiescent_at(&fixture.0, 123).unwrap());
    }

    #[test]
    fn stat_parser_handles_opaque_comm_and_rejects_bad_identity_or_fields() {
        assert_eq!(
            state(b"123 (x) \xff () Z 12 123 1", 123).unwrap(),
            (123, true)
        );
        assert_eq!(state(b"123 (x) S 12 123 1", 123).unwrap(), (123, false));
        assert_eq!(state(b"123 (x) T 12 123 1", 123).unwrap(), (123, false));
        for bytes in [
            b"124 (x) Z 12 123".as_slice(),
            b"123 (x) Z 12",
            b"123 (x) ZZ 12 123",
            b"123 (x) Z p 123",
            b"123 (x) Z 12 g",
        ] {
            assert!(state(bytes, 123).is_err());
        }
    }
}
