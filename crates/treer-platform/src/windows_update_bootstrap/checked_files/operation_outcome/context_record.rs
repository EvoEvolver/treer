//! Inert, private fixture codec. Parsing never constructs a native capability.
//! No native acquisition, evidence write or production caller uses this yet.
use super::*;

const DOMAIN: &[u8; 8] = b"TRCTX001";
const PREDECESSOR: &[u8; 8] = b"PREDE001";
const LENGTH: usize = 225;

#[derive(Clone, PartialEq, Eq, Debug)]
struct Image {
    // FILE_ID_INFO: 64-bit VolumeSerialNumber and all 128 FileId bits.
    // Never a path or the older BY_HANDLE_FILE_INFORMATION file index.
    volume: u64,
    file: [u8; 16],
    size: u64,
    hash: [u8; 32],
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Record {
    attempt: [u8; 32],
    predecessor: [u8; 32],
    component: u8, // 1 Controller, 2 Cli; independent of image role.
    role: u8,      // 1 Old, 2 New.
    operation: u8, // 1 MoveOld, 2 Publish.
    revision: u64,
    source: Image,
    target: Image,
    status: u32,
}

fn identifier<const N: usize>(value: &[u8; N]) -> bool {
    *value != [0; N] && *value != [u8::MAX; N]
}

impl Image {
    fn valid(&self) -> bool {
        self.volume != 0
            && self.volume != u64::MAX
            && identifier(&self.file)
            && self.size > 0
            && self.size <= IMAGE_LIMIT
    }
}

impl Record {
    fn valid(&self) -> bool {
        identifier(&self.attempt)
            && identifier(&self.predecessor)
            && matches!(self.component, 1 | 2)
            && matches!((self.role, self.operation), (1, 1) | (2, 2))
            && self.revision != 0
            && self.status != 0
            && self.source.valid()
            && self.target.valid()
            && (self.source.volume, self.source.file) != (self.target.volume, self.target.file)
    }

    fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let mut out = Vec::with_capacity(LENGTH);
        out.extend(DOMAIN);
        out.extend((LENGTH as u16).to_le_bytes());
        out.extend(self.attempt);
        out.extend(PREDECESSOR);
        out.extend(self.predecessor);
        out.extend([self.component, self.role, self.operation]);
        out.extend(self.revision.to_le_bytes());
        for image in [&self.source, &self.target] {
            out.extend(image.volume.to_le_bytes());
            out.extend(image.file);
            out.extend(image.size.to_le_bytes());
            out.extend(image.hash);
        }
        out.extend(self.status.to_le_bytes());
        Some(out)
    }
}

fn take<const N: usize>(input: &mut &[u8]) -> Option<[u8; N]> {
    let value = input.get(..N)?.try_into().ok()?;
    *input = &input[N..];
    Some(value)
}

fn image(input: &mut &[u8]) -> Option<Image> {
    Some(Image {
        volume: u64::from_le_bytes(take(input)?),
        file: take(input)?,
        size: u64::from_le_bytes(take(input)?),
        hash: take(input)?,
    })
}

fn parse(mut input: &[u8]) -> Option<Record> {
    if input.len() != LENGTH
        || take::<8>(&mut input)? != *DOMAIN
        || usize::from(u16::from_le_bytes(take(&mut input)?)) != LENGTH
    {
        return None;
    }
    let attempt = take(&mut input)?;
    if take::<8>(&mut input)? != *PREDECESSOR {
        return None;
    }
    let predecessor = take(&mut input)?;
    let [component, role, operation] = take(&mut input)?;
    let record = Record {
        attempt,
        predecessor,
        component,
        role,
        operation,
        revision: u64::from_le_bytes(take(&mut input)?),
        source: image(&mut input)?,
        target: image(&mut input)?,
        status: u32::from_le_bytes(take(&mut input)?),
    };
    (input.is_empty() && record.valid()).then_some(record)
}

// Equality is deliberately allocation-independent, but is only a data check.
// A future verifier must ALSO compare against the retained actual native error
// and check live custody. An arbitrary expected Record confers no trust.
fn matches_expected(bytes: &[u8], expected: &Record) -> bool {
    parse(bytes).as_ref() == Some(expected)
}

fn fixture() -> Record {
    Record {
        attempt: [0x11; 32],
        predecessor: [0x22; 32],
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
        status: 183,
    }
}

#[test]
fn native_operation_context_record_exact_binding() {
    let record = fixture();
    let encoded = record.encode().unwrap();
    assert_eq!(encoded.len(), LENGTH);
    assert!(matches_expected(&encoded, &record.clone()));
    // Every byte belongs to a binding or a mandatory framing field.
    for offset in 0..LENGTH {
        let mut changed = encoded.clone();
        changed[offset] ^= 1;
        assert!(!matches_expected(&changed, &record), "offset {offset}");
    }
    let mut swapped = record.clone();
    std::mem::swap(&mut swapped.attempt, &mut swapped.predecessor);
    assert!(!matches_expected(&swapped.encode().unwrap(), &record));
    for end in 0..LENGTH {
        assert!(parse(&encoded[..end]).is_none());
    }
    let mut trailing = encoded;
    trailing.push(0);
    assert!(parse(&trailing).is_none());
    for (start, end, value) in [
        (0, 8, 0),
        (8, 10, 0),
        (10, 42, 0),
        (42, 50, 0),
        (50, 82, 255),
        (82, 83, 0),
        (83, 84, 0),
        (84, 85, 0),
        (85, 93, 0),
        (93, 101, 0),
        (101, 117, 255),
        (117, 125, 0),
        (157, 165, 255),
        (165, 181, 0),
        (181, 189, 255),
        (221, 225, 0),
    ] {
        let mut invalid = record.encode().unwrap();
        invalid[start..end].fill(value);
        assert!(parse(&invalid).is_none(), "range {start}..{end}");
    }
}

#[test]
fn native_operation_context_record_refuses_invalid_fields() {
    for index in 0..17 {
        let mut record = fixture();
        match index {
            0 => record.attempt = [0; 32],
            1 => record.predecessor = [u8::MAX; 32],
            2 => record.component = 0,
            3 => record.role = 2,
            4 => record.operation = 3,
            5 => record.revision = 0,
            6 => record.status = 0,
            7 => record.source.volume = 0,
            8 => record.target.volume = u64::MAX,
            9 => record.source.file = [0; 16],
            10 => record.target.file = [u8::MAX; 16],
            11 => record.source.size = 0,
            12 => record.target.size = IMAGE_LIMIT + 1,
            13 => record.target.file = record.source.file,
            14 => record.attempt = [u8::MAX; 32],
            15 => record.predecessor = [0; 32],
            16 => record.component = 3,
            _ => unreachable!(),
        }
        assert!(record.encode().is_none(), "case {index}");
    }
    let mut publish = fixture();
    publish.role = 2;
    publish.operation = 2;
    publish.component = 2;
    publish.status = 5;
    assert!(parse(&publish.encode().unwrap()).is_some());
    assert!(!matches_expected(&publish.encode().unwrap(), &fixture()));
}

#[test]
fn native_operation_context_record_old_new_mutual_refusal() {
    let old = [1, 183, 0, 0, 0];
    assert!(decode(&old).is_some());
    assert!(parse(&old).is_none());
    assert!(decode(&fixture().encode().unwrap()).is_none());
}
