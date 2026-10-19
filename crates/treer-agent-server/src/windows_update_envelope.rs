//! Test-only draft envelope, not a supported on-disk receipt schema.
//! Payloads stay borrowed and opaque; no production reader or writer uses it.
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
    let word = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let declared = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if &bytes[..8] != MAGIC
        || word(8) != 2 // schema version
        || word(10) != 1 // Windows two-rename layout
        || word(12) != kind as u16
        || word(14) != 0 // reserved, must be zero
        || declared as usize != bytes.len()
    {
        return Err("invalid_windows_update_envelope");
    }
    Ok(&bytes[HEADER..])
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
