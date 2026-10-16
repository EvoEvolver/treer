use super::invalid;
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

fn start_ticks(bytes: &[u8], pid: u32) -> io::Result<u64> {
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
    Ok(ticks)
}

pub(super) fn creation(pid: u32) -> io::Result<String> {
    if std::fs::metadata(format!("/proc/{pid}"))?.uid() != rustix::process::geteuid().as_raw() {
        return Err(invalid());
    }
    let ticks = start_ticks(&read(&format!("/proc/{pid}/stat"), 8192)?, pid)?;
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
    Ok(format!("linux:{boot}:{ticks}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stat_comm_delimiters_cannot_shift_creation_field_and_truncation_fails() {
        let tail = (0..18).map(|_| "1").collect::<Vec<_>>().join(" ");
        assert_eq!(
            start_ticks(format!("42 (odd ) name) S {tail} 12345 9\n").as_bytes(), 42).unwrap(),
            12345
        );
        for bytes in [
            b"42 (odd) S 1 2".as_slice(),
            b"42 (odd) Z 1 2",
            b"41 (odd) S 1 2",
        ] {
            assert!(start_ticks(bytes, 42).is_err());
        }
    }
}
