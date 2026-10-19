//! Test-only draft envelope, not a supported on-disk receipt schema.
//! Slice decoding borrows opaque payloads; file reads allocate only after a
//! bounded header check. No production reader or writer uses either path.
const MAGIC: &[u8; 8] = b"TRUPDWIN";
const HEADER: usize = 20;
pub(crate) const LIMIT: usize = 65_536;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Receipt = 1,
    Marker = 2,
}

pub(crate) fn decode(bytes: &[u8], kind: Kind) -> Result<&[u8], &'static str> {
    // Check the supplied total cap before looking at a declared length or any
    // variable field. No allocation, JSON parser or payload copying occurs.
    if bytes.len() > LIMIT || bytes.len() <= HEADER {
        return Err("invalid_windows_update_envelope");
    }
    validate_header(&bytes[..HEADER], bytes.len(), kind)?;
    Ok(&bytes[HEADER..])
}

fn validate_header(bytes: &[u8], total: usize, kind: Kind) -> Result<(), &'static str> {
    let word = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let declared = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if &bytes[..8] != MAGIC
        || word(8) != 2 // schema version
        || word(10) != 1 // Windows two-rename layout
        || word(12) != kind as u16
        || word(14) != 0 // reserved, must be zero
        || declared as usize != total
    {
        return Err("invalid_windows_update_envelope");
    }
    Ok(())
}

// Bounded transport only: this does not establish ACL, NTFS identity, locking,
// pair integrity or state-machine validity. The caller must pin those separately.
pub(crate) fn read_file(file: &mut std::fs::File, kind: Kind) -> Result<Vec<u8>, &'static str> {
    use std::io::Seek;
    let metadata = file.metadata().map_err(|_| "record_metadata_refused")?;
    if !metadata.is_file() || file.stream_position().map_err(|_| "record_seek_refused")? != 0 {
        return Err("record_file_refused");
    }
    let bytes = read_bounded(file, metadata.len(), kind)?;
    if file
        .metadata()
        .map_err(|_| "record_metadata_refused")?
        .len()
        != metadata.len()
    {
        return Err("record_length_changed");
    }
    Ok(bytes)
}

fn read_bounded(
    reader: &mut impl std::io::Read,
    observed_length: u64,
    kind: Kind,
) -> Result<Vec<u8>, &'static str> {
    if observed_length <= HEADER as u64 || observed_length > LIMIT as u64 {
        return Err("invalid_windows_update_envelope");
    }
    let mut header = [0; HEADER];
    reader
        .read_exact(&mut header)
        .map_err(|_| "record_read_refused")?;
    let total = observed_length as usize; // bounded before conversion/allocation
    validate_header(&header, total, kind)?;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(&header);
    bytes.resize(total, 0);
    reader
        .read_exact(&mut bytes[HEADER..])
        .map_err(|_| "record_read_refused")?;
    // Probe at most one extra byte, never drain a growing file/stream.
    let mut extra = [0];
    loop {
        match reader.read(&mut extra) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            _ => return Err("record_trailing_or_read_refused"),
        }
    }
    Ok(bytes)
}

pub(crate) fn encode(payload: &[u8], kind: Kind) -> Result<Vec<u8>, &'static str> {
    // Exclusive upper bound is small enough for u32 on every supported target.
    if payload.is_empty() || payload.len() > LIMIT - HEADER {
        return Err("invalid_windows_update_envelope");
    }
    let total = HEADER + payload.len();
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(MAGIC);
    for word in [2_u16, 1, kind as u16, 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&(total as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

#[test]
fn draft_envelope_has_fixed_little_endian_header_and_borrowed_payload() {
    let bytes = encode(b"not JSON and not parsed", Kind::Receipt).unwrap();
    assert_eq!(
        &bytes[..HEADER],
        b"TRUPDWIN\x02\x00\x01\x00\x01\x00\x00\x00\x2b\x00\x00\x00"
    );
    let payload = decode(&bytes, Kind::Receipt).unwrap();
    assert_eq!(payload, b"not JSON and not parsed");
    assert_eq!(payload.as_ptr(), bytes[HEADER..].as_ptr());
    assert!(decode(&bytes, Kind::Marker).is_err());
    let marker = encode(b"marker", Kind::Marker).unwrap();
    assert_eq!(decode(&marker, Kind::Marker), Ok(b"marker".as_slice()));
    assert!(decode(&marker, Kind::Receipt).is_err());
}

#[test]
fn every_envelope_starts_outside_json_even_before_payload_parsing() {
    for kind in [Kind::Receipt, Kind::Marker] {
        let bytes = encode(b"{}", kind).unwrap();
        // JSON values may start with an object, array, string, literal, number
        // or whitespace. Magic must exclude all of them, not just `{`.
        assert!(!b"{[\"tfn-0123456789 \t\r\n".contains(&bytes[0]));
        assert_eq!(bytes[0], b'T');
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
    }
}

#[test]
fn draft_envelope_refuses_truncation_bad_headers_lengths_and_trailing_bytes() {
    let bytes = encode(b"opaque", Kind::Receipt).unwrap();
    for end in 0..bytes.len() {
        assert!(decode(&bytes[..end], Kind::Receipt).is_err());
    }
    for offset in 0..HEADER {
        let mut bad = bytes.clone();
        bad[offset] ^= 0x80;
        assert!(decode(&bad, Kind::Receipt).is_err(), "offset={offset}");
    }
    let mut wrong_endian = bytes.clone();
    wrong_endian[8..10].copy_from_slice(&2_u16.to_be_bytes());
    assert!(decode(&wrong_endian, Kind::Receipt).is_err());
    let mut length_overflow = bytes.clone();
    length_overflow[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&length_overflow, Kind::Receipt).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode(&trailing, Kind::Receipt).is_err());
    let mut concatenated = bytes.clone();
    concatenated.extend_from_slice(&bytes);
    assert!(decode(&concatenated, Kind::Receipt).is_err());
}

#[test]
fn draft_envelope_total_cap_includes_header() {
    assert!(encode(&[], Kind::Receipt).is_err());
    assert!(encode(&vec![0; LIMIT - HEADER + 1], Kind::Receipt).is_err());
    let bytes = encode(&vec![0; LIMIT - HEADER], Kind::Receipt).unwrap();
    assert_eq!(bytes.len(), LIMIT);
    assert_eq!(decode(&bytes, Kind::Receipt).unwrap().len(), LIMIT - HEADER);
    assert!(decode(&vec![0; LIMIT + 1], Kind::Receipt).is_err());
}

#[test]
fn bounded_reader_refuses_before_body_read_and_stops_at_one_extra_byte() {
    use std::io::Cursor;
    let bytes = encode(b"opaque", Kind::Receipt).unwrap();
    for length in [0, HEADER as u64, LIMIT as u64 + 1, u64::MAX] {
        let mut reader = Cursor::new(&bytes);
        assert!(read_bounded(&mut reader, length, Kind::Receipt).is_err());
        assert_eq!(reader.position(), 0);
    }
    for offset in 0..HEADER {
        let mut bad = bytes.clone();
        bad[offset] ^= 0x80;
        let mut reader = Cursor::new(bad);
        assert!(read_bounded(&mut reader, bytes.len() as u64, Kind::Receipt).is_err());
        assert_eq!(reader.position(), HEADER as u64);
    }
    for end in 0..bytes.len() {
        let mut reader = Cursor::new(&bytes[..end]);
        assert!(read_bounded(&mut reader, bytes.len() as u64, Kind::Receipt).is_err());
    }
    let mut appended = bytes.clone();
    appended.extend_from_slice(&[0; 100]);
    let mut reader = Cursor::new(appended);
    assert!(read_bounded(&mut reader, bytes.len() as u64, Kind::Receipt).is_err());
    assert_eq!(reader.position(), bytes.len() as u64 + 1);
    let mut reader = Cursor::new(&bytes);
    assert_eq!(
        read_bounded(&mut reader, bytes.len() as u64, Kind::Receipt),
        Ok(bytes.clone())
    );
    let max = encode(&vec![0; LIMIT - HEADER], Kind::Marker).unwrap();
    assert_eq!(
        read_bounded(&mut Cursor::new(&max), LIMIT as u64, Kind::Marker),
        Ok(max)
    );
}

#[test]
fn bounded_file_reader_checks_real_length_and_requires_start_position() {
    use std::io::{Seek, SeekFrom, Write};
    let path = std::env::temp_dir().join(format!("treer-record-{}", uuid::Uuid::new_v4()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let _cleanup = Cleanup(path);
    let bytes = encode(b"opaque", Kind::Receipt).unwrap();
    file.write_all(&bytes).unwrap();
    assert_eq!(
        read_file(&mut file, Kind::Receipt),
        Err("record_file_refused")
    );
    file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(read_file(&mut file, Kind::Receipt), Ok(bytes));
    file.set_len(LIMIT as u64 + 1).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    assert!(read_file(&mut file, Kind::Receipt).is_err());
    assert_eq!(file.stream_position().unwrap(), 0);
    drop(file);
}

#[test]
fn bounded_reader_does_not_return_partial_bytes_on_io_failure() {
    use std::io::{self, Read};
    struct Fault<'a> {
        bytes: &'a [u8],
        position: usize,
        fail_at: usize,
    }
    impl Read for Fault<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.position == self.fail_at {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let count = out
                .len()
                .min(self.fail_at - self.position)
                .min(self.bytes.len() - self.position);
            out[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }
    }
    let bytes = encode(b"opaque", Kind::Receipt).unwrap();
    // Read errors at header, payload and the final EOF probe all refuse;
    // the caller cannot receive a partially populated Vec as a successful read.
    for fail_at in 0..=bytes.len() {
        let mut reader = Fault {
            bytes: &bytes,
            position: 0,
            fail_at,
        };
        let error = read_bounded(&mut reader, bytes.len() as u64, Kind::Receipt).unwrap_err();
        assert_eq!(
            error,
            if fail_at == bytes.len() {
                "record_trailing_or_read_refused"
            } else {
                "record_read_refused"
            }
        );
        assert_eq!(reader.position, fail_at);
    }
}
