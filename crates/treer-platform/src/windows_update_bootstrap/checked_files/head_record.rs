//! Inert fixture data only. Decoding cannot create custody or failure authority.
use super::IMAGE_LIMIT;

const MAGIC: &[u8; 8] = b"TRHEAD01";
const LENGTH: usize = 181;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Image {
    volume: u64,
    file: [u8; 16],
    size: u64,
    hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Head {
    attempt: [u8; 32],
    component: u8,
    role: u8,
    operation: u8,
    revision: u64,
    source: Image,
    target: Image,
}

fn identifier<const N: usize>(value: &[u8; N]) -> bool {
    *value != [0; N] && *value != [u8::MAX; N]
}

impl Head {
    fn valid(&self) -> bool {
        identifier(&self.attempt)
            && matches!(self.component, 1 | 2)
            && matches!((self.role, self.operation), (1, 1) | (2, 2))
            && self.revision != 0
            && [&self.source, &self.target].into_iter().all(|image| {
                image.volume != 0
                    && image.volume != u64::MAX
                    && identifier(&image.file)
                    && (1..=IMAGE_LIMIT).contains(&image.size)
            })
            && (self.source.volume, self.source.file) != (self.target.volume, self.target.file)
    }

    fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let mut bytes = Vec::with_capacity(LENGTH);
        bytes.extend(MAGIC);
        bytes.extend((LENGTH as u16).to_le_bytes());
        bytes.extend(self.attempt);
        bytes.extend([self.component, self.role, self.operation]);
        bytes.extend(self.revision.to_le_bytes());
        for image in [&self.source, &self.target] {
            bytes.extend(image.volume.to_le_bytes());
            bytes.extend(image.file);
            bytes.extend(image.size.to_le_bytes());
            bytes.extend(image.hash);
        }
        Some(bytes)
    }
}

fn take<const N: usize>(input: &mut &[u8]) -> Option<[u8; N]> {
    let bytes = input.get(..N)?.try_into().ok()?;
    *input = &input[N..];
    Some(bytes)
}

fn image(input: &mut &[u8]) -> Option<Image> {
    Some(Image {
        volume: u64::from_le_bytes(take(input)?),
        file: take(input)?,
        size: u64::from_le_bytes(take(input)?),
        hash: take(input)?,
    })
}

fn parse(mut input: &[u8]) -> Option<Head> {
    if input.len() != LENGTH
        || take::<8>(&mut input)? != *MAGIC
        || usize::from(u16::from_le_bytes(take(&mut input)?)) != LENGTH
    {
        return None;
    }
    let attempt = take(&mut input)?;
    let [component, role, operation] = take(&mut input)?;
    let head = Head {
        attempt,
        component,
        role,
        operation,
        revision: u64::from_le_bytes(take(&mut input)?),
        source: image(&mut input)?,
        target: image(&mut input)?,
    };
    (input.is_empty() && head.valid()).then_some(head)
}

// Syntactic check for the next disposable storage fixture, not authorization.
pub(super) fn valid_bytes(bytes: &[u8]) -> bool {
    parse(bytes).is_some()
}

pub(super) fn fixture_bytes() -> Vec<u8> {
    fixture().encode().unwrap()
}

// Exact data equality is necessary, but never supplies independent expectations.
fn matches_expected(bytes: &[u8], expected: &Head) -> bool {
    parse(bytes).as_ref() == Some(expected)
}

fn fixture() -> Head {
    Head {
        attempt: [0x11; 32],
        component: 1,
        role: 1,
        operation: 1,
        revision: 7,
        source: Image {
            volume: 9,
            file: [3; 16],
            size: 17,
            hash: [4; 32],
        },
        target: Image {
            volume: 9,
            file: [5; 16],
            size: 31,
            hash: [6; 32],
        },
    }
}

#[test]
fn native_head_codec_literal_golden_and_exact_fields() {
    // Literal independent wire vector: do not derive this with the encoder.
    let hex = concat!(
        "5452484541443031b500",
        "1111111111111111111111111111111111111111111111111111111111111111",
        "0101010700000000000000",
        "0900000000000000030303030303030303030303030303031100000000000000",
        "0404040404040404040404040404040404040404040404040404040404040404",
        "0900000000000000050505050505050505050505050505051f00000000000000",
        "0606060606060606060606060606060606060606060606060606060606060606",
    );
    let golden: Vec<u8> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let head = fixture();
    assert_eq!(golden.len(), LENGTH);
    assert_eq!(head.encode().unwrap(), golden);
    assert_eq!(parse(&golden), Some(head.clone()));
    for offset in 0..LENGTH {
        let mut changed = golden.clone();
        changed[offset] ^= 1;
        assert!(!matches_expected(&changed, &head), "offset {offset}");
    }
    // A supported selector/revision change can be valid data, but not this head.
    let mut different = head.clone();
    different.component = 2;
    different.role = 2;
    different.operation = 2;
    different.revision = u64::MAX;
    assert!(!matches_expected(&different.encode().unwrap(), &head));
}

#[test]
fn native_head_codec_rejects_framing_and_outcome_records() {
    let bytes = fixture().encode().unwrap();
    for end in 0..LENGTH {
        assert!(parse(&bytes[..end]).is_none());
    }
    for suffix in [vec![0], vec![183, 0, 0, 0], bytes.clone()] {
        let mut trailing = bytes.clone();
        trailing.extend(suffix);
        assert!(parse(&trailing).is_none());
    }
    for (offset, value) in [
        (7, b'2'),
        (8, 0),
        (9, 1),
        (42, 0),
        (42, 3),
        (43, 0),
        (43, 2),
        (44, 3),
    ] {
        let mut changed = bytes.clone();
        changed[offset] = value;
        assert!(parse(&changed).is_none());
    }
    assert!(parse(&[1, 183, 0, 0, 0]).is_none());
    let mut foreign_magic = bytes;
    foreign_magic[..8].copy_from_slice(b"TRCTX001");
    assert!(parse(&foreign_magic).is_none());
}

#[test]
fn native_head_codec_rejects_invalid_objects_and_accepts_bounds() {
    let bytes = fixture().encode().unwrap();
    for (start, end, value) in [
        (10, 42, 0),
        (10, 42, 255),
        (45, 53, 0),
        (53, 61, 0),
        (53, 61, 255),
        (61, 77, 0),
        (61, 77, 255),
        (77, 85, 0),
        (117, 125, 0),
        (117, 125, 255),
        (125, 141, 0),
        (125, 141, 255),
        (141, 149, 0),
    ] {
        let mut changed = bytes.clone();
        changed[start..end].fill(value);
        assert!(parse(&changed).is_none(), "range {start}..{end}");
    }
    let mut same_object = fixture();
    same_object.target.file = same_object.source.file;
    assert!(same_object.encode().is_none());
    let mut duplicate = bytes.clone();
    duplicate[125..141].copy_from_slice(&bytes[61..77]);
    assert!(parse(&duplicate).is_none());
    let mut other_volume = same_object;
    other_volume.target.volume = 0x1122_3344_5566_7788;
    assert_eq!(parse(&other_volume.encode().unwrap()), Some(other_volume));
    for offset in [77, 141] {
        let mut oversize = bytes.clone();
        oversize[offset..offset + 8].copy_from_slice(&(IMAGE_LIMIT + 1).to_le_bytes());
        assert!(parse(&oversize).is_none());
    }
    let mut bounds = fixture();
    bounds.source.size = 1;
    bounds.target.size = IMAGE_LIMIT;
    bounds.source.hash = [0; 32];
    bounds.target.hash = [255; 32];
    assert_eq!(parse(&bounds.encode().unwrap()), Some(bounds));
}
