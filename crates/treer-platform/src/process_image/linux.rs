use super::{invalid, Identity};
use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
};

fn read(path: &str, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

fn stat_identity(bytes: &[u8], pid: u32) -> io::Result<(u64, u32)> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let prefix = format!("{pid} (");
    if !text.starts_with(&prefix) {
        return Err(invalid());
    }
    // comm may itself contain ')' or spaces; the last ') ' precedes field 3.
    let end = text.rfind(") ").ok_or_else(invalid)?;
    let fields: Vec<_> = text[end + 2..].split_ascii_whitespace().collect();
    if fields.len() < 20 || fields[0] == "Z" || fields[0] == "X" {
        return Err(invalid());
    }
    let ticks: u64 = fields[19].parse().map_err(|_| invalid())?;
    if ticks == 0 {
        return Err(invalid());
    }
    Ok((ticks, fields[1].parse().map_err(|_| invalid())?))
}

pub(super) fn creation(pid: u32) -> io::Result<Identity> {
    if std::fs::metadata(format!("/proc/{pid}"))?.uid() != rustix::process::geteuid().as_raw() {
        return Err(invalid());
    }
    let (ticks, parent) = stat_identity(&read(&format!("/proc/{pid}/stat"), 8192)?, pid)?;
    let boot = read("/proc/sys/kernel/random/boot_id", 37)?;
    let boot = std::str::from_utf8(&boot)
        .map_err(|_| invalid())?
        .trim_end_matches('\n');
    if boot.len() != 36
        || !boot.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
    {
        return Err(invalid());
    }
    Ok(Identity {
        creation: format!("linux:{boot}:{ticks}"),
        parent,
    })
}

pub(super) fn children(pid: u32) -> io::Result<Vec<u32>> {
    let mut pids = std::collections::BTreeSet::new();
    for (count, task) in std::fs::read_dir(format!("/proc/{pid}/task"))?.enumerate() {
        if count >= 1024 {
            return Err(invalid());
        }
        let task = task?;
        let tid: u32 = task
            .file_name()
            .to_str()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let bytes = match read(&format!("/proc/{pid}/task/{tid}/children"), 64 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for child in std::str::from_utf8(&bytes)
            .map_err(|_| invalid())?
            .split_ascii_whitespace()
        {
            let child: u32 = child.parse().map_err(|_| invalid())?;
            if child == 0 || child > i32::MAX as u32 {
                return Err(invalid());
            }
            pids.insert(child);
            if pids.len() > 4096 {
                return Err(invalid());
            }
        }
    }
    Ok(pids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stat_comm_delimiters_cannot_shift_creation_field_and_truncation_fails() {
        let tail = (0..18).map(|_| "1").collect::<Vec<_>>().join(" ");
        assert_eq!(
            stat_identity(format!("42 (odd ) name) S {tail} 12345 9\n").as_bytes(), 42).unwrap(),
            (12345, 1)
        );
        for bytes in [
            b"42 (odd) S 1 2".as_slice(),
            b"42 (odd) Z 1 2",
            b"41 (odd) S 1 2",
        ] {
            assert!(stat_identity(bytes, 42).is_err());
        }
    }
}
