//! Bounded container validation, matching the Node attachment boundary.
//! This checks framing and dimensions, not compressed pixel decoding.
#[cfg(any(unix, test))]
use anyhow::{bail, Result};
pub const FILE_LIMIT: usize = 10 * 1024 * 1024;
#[cfg(unix)]
pub const PROMPT_LIMIT: usize = 24 * 1024 * 1024;
pub const COUNT_LIMIT: usize = 8;

#[cfg(any(unix, test))]
fn dim(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && w <= 16384 && h <= 16384 && u64::from(w) * u64::from(h) <= 100_000_000
}
#[cfg(any(unix, test))]
fn be(b: &[u8]) -> u32 {
    b.iter().fold(0, |n, b| n * 256 + u32::from(*b))
}
#[cfg(any(unix, test))]
fn le(b: &[u8]) -> u32 {
    b.iter().rev().fold(0, |n, b| n * 256 + u32::from(*b))
}
#[cfg(any(unix, test))]
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320_u32.wrapping_mul(crc & 1));
        }
    }
    !crc
}
#[cfg(any(unix, test))]
pub fn format(bytes: &[u8]) -> Result<(&'static str, &'static str)> {
    if bytes.is_empty() {
        bail!("attachment_empty");
    }
    if bytes.len() > FILE_LIMIT {
        bail!("attachment_too_large");
    }
    inspect(bytes, 0).ok_or_else(|| anyhow::anyhow!("attachment_invalid_image"))
}
#[cfg(any(unix, test))]
fn inspect(b: &[u8], depth: usize) -> Option<(&'static str, &'static str)> {
    if depth > 1 {
        return None;
    }
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (mut p, mut header, mut pixels) = (8_usize, false, false);
        while p + 12 <= b.len() {
            let n = be(&b[p..p + 4]) as usize;
            let end = p.checked_add(12)?.checked_add(n)?;
            if end > b.len() {
                return None;
            }
            let kind = &b[p + 4..p + 8];
            if !kind.iter().all(u8::is_ascii_alphabetic)
                || crc32(&b[p + 4..end - 4]) != be(&b[end - 4..end])
                || !header && kind != b"IHDR"
            {
                return None;
            }
            if kind == b"IHDR" {
                if header || n != 13 || !dim(be(&b[p + 8..p + 12]), be(&b[p + 12..p + 16])) {
                    return None;
                }
                header = true;
            }
            if kind == b"IDAT" {
                if n == 0 {
                    return None;
                }
                pixels = true;
            }
            if kind == b"IEND" {
                return (n == 0 && pixels && end == b.len()).then_some(("image/png", "png"));
            }
            p = end;
        }
        return None;
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        if b.len() < 14 || !dim(le(&b[6..8]), le(&b[8..10])) {
            return None;
        }
        let mut p = 13
            + if b[10] & 128 != 0 {
                3 * (1 << ((b[10] & 7) + 1))
            } else {
                0
            };
        let mut images = 0;
        while p < b.len() {
            let kind = b[p];
            p += 1;
            match kind {
                0x3b => return (images > 0 && p == b.len()).then_some(("image/gif", "gif")),
                0x21 => {
                    b.get(p)?;
                    p += 1;
                    blocks(b, &mut p)?;
                }
                0x2c => {
                    if p + 10 > b.len() || !dim(le(&b[p + 4..p + 6]), le(&b[p + 6..p + 8])) {
                        return None;
                    }
                    let flags = b[p + 8];
                    p += 9 + if flags & 128 != 0 {
                        3 * (1 << ((flags & 7) + 1))
                    } else {
                        0
                    };
                    if !(2..=8).contains(b.get(p)?) {
                        return None;
                    }
                    p += 1;
                    blocks(b, &mut p)?;
                    images += 1;
                }
                _ => return None,
            }
        }
        return None;
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        if b.len() < 30 || le(&b[4..8]) as usize + 8 != b.len() {
            return None;
        }
        let (mut p, mut pixels) = (12_usize, false);
        while p + 8 <= b.len() {
            let kind = &b[p..p + 4];
            let n = le(&b[p + 4..p + 8]) as usize;
            let data = p + 8;
            let end = data.checked_add(n)?;
            if end > b.len() {
                return None;
            }
            match kind {
                b"VP8 " => {
                    if n < 10
                        || &b[data + 3..data + 6] != b"\x9d\x01\x2a"
                        || !dim(
                            le(&b[data + 6..data + 8]) & 0x3fff,
                            le(&b[data + 8..data + 10]) & 0x3fff,
                        )
                    {
                        return None;
                    }
                    pixels = true;
                }
                b"VP8L" => {
                    if n < 5 || b[data] != 0x2f {
                        return None;
                    }
                    let bits = le(&b[data + 1..data + 5]);
                    if !dim((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1) {
                        return None;
                    }
                    pixels = true;
                }
                b"VP8X" => {
                    if n != 10
                        || !dim(
                            le(&b[data + 4..data + 7]) + 1,
                            le(&b[data + 7..data + 10]) + 1,
                        )
                    {
                        return None;
                    }
                }
                b"ANMF" => {
                    if n < 24
                        || depth != 0
                        || !dim(
                            le(&b[data + 6..data + 9]) + 1,
                            le(&b[data + 9..data + 12]) + 1,
                        )
                    {
                        return None;
                    }
                    let frame = &b[data + 16..end];
                    let mut nested = b"RIFF".to_vec();
                    nested.extend_from_slice(&((frame.len() + 4) as u32).to_le_bytes());
                    nested.extend_from_slice(b"WEBP");
                    nested.extend_from_slice(frame);
                    inspect(&nested, depth + 1)?;
                    pixels = true;
                }
                b"ICCP" | b"EXIF" | b"XMP " | b"ALPH" | b"ANIM" => {}
                _ => return None,
            }
            p = end + (n & 1);
        }
        return (pixels && p == b.len()).then_some(("image/webp", "webp"));
    }
    if b.len() > 4 && b.starts_with(b"\xff\xd8") {
        let (mut p, mut header, mut pixels) = (2_usize, false, false);
        while p < b.len() {
            if b[p] != 0xff {
                return None;
            }
            p += 1;
            while b.get(p) == Some(&0xff) {
                p += 1;
            }
            let marker = *b.get(p)?;
            p += 1;
            if marker == 0xd9 {
                return (header && pixels && p == b.len()).then_some(("image/jpeg", "jpg"));
            }
            if marker == 0 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || p + 2 > b.len() {
                return None;
            }
            let n = be(&b[p..p + 2]) as usize;
            let end = p.checked_add(n)?;
            if n < 2 || end > b.len() {
                return None;
            }
            if [
                0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
            ]
            .contains(&marker)
            {
                if n < 8 || !dim(be(&b[p + 5..p + 7]), be(&b[p + 3..p + 5])) {
                    return None;
                }
                header = true;
            }
            p = end;
            if marker == 0xda {
                if !header {
                    return None;
                }
                pixels = true;
                while p < b.len() {
                    if b[p] != 0xff {
                        p += 1;
                        continue;
                    }
                    let next = *b.get(p + 1)?;
                    if next == 0 || (0xd0..=0xd7).contains(&next) {
                        p += 2;
                    } else {
                        break;
                    }
                }
            }
        }
    }
    None
}
#[cfg(any(unix, test))]
fn blocks(b: &[u8], p: &mut usize) -> Option<()> {
    while *p < b.len() {
        let n = b[*p] as usize;
        *p += 1;
        if n == 0 {
            return Some(());
        }
        *p = p.checked_add(n)?;
        if *p > b.len() {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn containers_reject_every_truncated_prefix_and_trailing_payload() {
        let gif=b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x01\x4c\x00\x3b".to_vec();
        let jpeg = vec![
            255, 216, 255, 192, 0, 8, 8, 0, 1, 0, 1, 1, 255, 218, 0, 2, 1, 255, 217,
        ];
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&22_u32.to_le_bytes());
        webp.extend_from_slice(b"WEBPVP8 ");
        webp.extend_from_slice(&10_u32.to_le_bytes());
        webp.extend_from_slice(&[0, 0, 0, 0x9d, 1, 0x2a, 1, 0, 1, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in [
            (b"IHDR", vec![0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]),
            (b"IDAT", vec![1]),
            (b"IEND", vec![]),
        ] {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut payload = kind.to_vec();
            payload.extend(data);
            png.extend_from_slice(&payload);
            png.extend_from_slice(&crc32(&payload).to_be_bytes());
        }
        for bytes in [gif, jpeg, webp, png] {
            assert!(format(&bytes).is_ok());
            for n in 0..bytes.len() {
                assert!(format(&bytes[..n]).is_err(), "prefix {n}");
            }
            let mut appended = bytes.clone();
            appended.extend_from_slice(b"MZ");
            assert!(format(&appended).is_err());
        }
        assert!(format(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xffIHDR").is_err());
        assert!(format(b"MZfake").is_err());
    }
}
