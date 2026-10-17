//! Bounded procfs state observation while the owned group leader is unreaped.
//! Never read cmdline/environ/exe, and never signal a discovered numeric PID.
use std::{
    fs::File,
    io::{self, Read},
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

pub(super) fn quiescent(group: rustix::process::Pid) -> io::Result<bool> {
    let group = group.as_raw_nonzero().get() as u32;
    let mut leader = false;
    // Procfs has no atomic per-group inventory API. SIGKILL was submitted first
    // while the leader stays reserved. Cooperating members must stay in their
    // group and remain signalable. Linux kill(-pgid) holds tasklist_lock while
    // submitting signals; copy_process tests fatal_signal_pending under that
    // lock before publishing a fork. Sources: torvalds/linux kernel/signal.c
    // kill_something_info/__kill_pgrp_info; kernel/fork.c copy_process. This is
    // not an atomic procfs snapshot or containment of uncooperative children.
    // Missing/changed/denied metadata is never used as proof of exit.
    for (count, entry) in std::fs::read_dir("/proc")
        .map_err(|_| invalid())?
        .enumerate()
    {
        if count >= 65536 {
            return Err(invalid());
        }
        let entry = entry.map_err(|_| invalid())?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let mut bytes = Vec::new();
        let file = match File::open(entry.path().join("stat")) {
            Ok(file) => file,
            // Unrelated processes can disappear; their missing stat is not
            // used to attest our group. Require the owned leader below.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(invalid()),
        };
        file.take(8193)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 8192 {
            return Err(invalid());
        }
        let (member_group, dead) = state(&bytes, pid)?;
        if member_group == group {
            if !dead {
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
