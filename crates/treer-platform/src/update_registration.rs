//! Inert registration-byte model, compiled only in the platform test binary.
//! Inputs and observations are synthetic. No filesystem, process, clock source,
//! frame binding, permit, lock or production caller is exposed here.
use super::update_deadline::{Deadline, Window};

// Distinct from both TRUB pipe frames and the earlier 56-byte observation.
// This fixture format is not a production on-disk compatibility contract.
const MAGIC: &[u8; 4] = b"TURF";
const VERSION: u16 = 1;
const SIZE: usize = 184;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    created: u64,
    volume: u32,
    file: u64,
    image: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observation {
    attempt: [u8; 16],
    intent: [u8; 32],
    owner: ProcessIdentity,
    child: ProcessIdentity,
}

struct Registration {
    observation: Observation,
    window: Window,
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], ()> {
        let (head, rest) = self.0.split_at_checked(N).ok_or(())?;
        self.0 = rest;
        head.try_into().map_err(|_| ())
    }

    fn process(&mut self) -> Result<ProcessIdentity, ()> {
        Ok(ProcessIdentity {
            pid: u32::from_le_bytes(self.take()?),
            created: u64::from_le_bytes(self.take()?),
            volume: u32::from_le_bytes(self.take()?),
            file: u64::from_le_bytes(self.take()?),
            image: self.take()?,
        })
    }
}

impl Registration {
    fn parse(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() != SIZE {
            return Err(());
        }
        let mut reader = Reader(bytes);
        if reader.take::<4>()? != *MAGIC
            || u16::from_le_bytes(reader.take()?) != VERSION
            || usize::from(u16::from_le_bytes(reader.take()?)) != SIZE
        {
            return Err(());
        }
        let observation = Observation {
            attempt: reader.take()?,
            intent: reader.take()?,
            owner: reader.process()?,
            child: reader.process()?,
        };
        let window = Window {
            start: u64::from_le_bytes(reader.take()?),
            expiry: u64::from_le_bytes(reader.take()?),
        };
        if !reader.0.is_empty()
            || observation.attempt == [0; 16]
            || observation.intent == [0; 32]
            || observation.owner.pid == observation.child.pid
            || [observation.owner, observation.child]
                .iter()
                .any(|p| p.pid == 0 || p.created == 0 || p.image == [0; 32])
        {
            return Err(());
        }
        Ok(Self {
            observation,
            window,
        })
    }

    // A decoded record is untrusted. Compare all fields with independent local
    // observations before accepting its window. The owner supplies its original
    // ceiling; neither a valid encoding nor a later child budget can extend it.
    fn validate(
        &self,
        expected: &Observation,
        now: u64,
        original_expiry: Option<u64>,
    ) -> Result<Deadline, ()> {
        if self.observation != *expected {
            return Err(());
        }
        self.window.accept(now, original_expiry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Observation, Vec<u8>) {
        let owner = ProcessIdentity {
            pid: 101,
            created: 200,
            volume: 0, // Opaque identity fields are compared, not guessed.
            file: 300,
            image: [4; 32],
        };
        let expected = Observation {
            attempt: [1; 16],
            intent: [2; 32],
            owner,
            child: ProcessIdentity {
                pid: 102,
                created: 201,
                ..owner
            },
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(SIZE as u16).to_le_bytes());
        bytes.extend_from_slice(&expected.attempt);
        bytes.extend_from_slice(&expected.intent);
        for process in [expected.owner, expected.child] {
            bytes.extend_from_slice(&process.pid.to_le_bytes());
            bytes.extend_from_slice(&process.created.to_le_bytes());
            bytes.extend_from_slice(&process.volume.to_le_bytes());
            bytes.extend_from_slice(&process.file.to_le_bytes());
            bytes.extend_from_slice(&process.image);
        }
        bytes.extend_from_slice(&100u64.to_le_bytes());
        bytes.extend_from_slice(&5_100u64.to_le_bytes());
        assert_eq!(bytes.len(), SIZE);
        (expected, bytes)
    }

    #[test]
    fn independent_reads_preserve_owner_ceiling_and_do_not_renew_child_budget() {
        let (expected, bytes) = fixture();
        let owner = Registration::parse(&bytes).unwrap();
        let child = Registration::parse(&bytes.clone()).unwrap();
        let mut owner = owner.validate(&expected, 150, Some(5_000)).unwrap();
        let mut child = child.validate(&expected, 4_900, None).unwrap();
        assert_eq!(owner.remaining_ms(4_999), Ok(1));
        assert!(owner.check(5_000).is_err());
        assert_eq!(child.remaining_ms(5_000), Ok(100));
        assert!(child.check(5_100).is_err());
        assert!(owner.check(151).is_err());
    }

    #[test]
    fn rejects_truncation_trailing_data_unknown_header_and_pipe_frames() {
        let (_, bytes) = fixture();
        for length in 0..SIZE {
            assert!(Registration::parse(&bytes[..length]).is_err());
        }
        let mut oversized = bytes.clone();
        oversized.push(0);
        assert!(Registration::parse(&oversized).is_err());
        for offset in 0..8 {
            let mut changed = bytes.clone();
            changed[offset] ^= 1;
            assert!(Registration::parse(&changed).is_err());
        }
        assert!(Registration::parse(&[0; super::super::update_binding::FRAME_SIZE]).is_err());
        let mut disguised = bytes;
        disguised[..5].copy_from_slice(b"TRUB\x01");
        assert!(Registration::parse(&disguised).is_err());
    }

    #[test]
    fn every_binding_byte_requires_an_exact_independent_observation() {
        let (expected, bytes) = fixture();
        // Attempt, intent and both process identities: no self-reported field
        // may substitute for the locally observed value, even if well formed.
        for offset in 8..168 {
            let mut changed = bytes.clone();
            changed[offset] ^= 0x80;
            if let Ok(record) = Registration::parse(&changed) {
                assert!(record.validate(&expected, 200, Some(5_100)).is_err());
            }
        }
        let record = Registration::parse(&bytes).unwrap();
        let mut other_attempt = expected;
        other_attempt.attempt = [3; 16];
        assert!(record.validate(&other_attempt, 200, None).is_err());
        let mut swapped = expected;
        std::mem::swap(&mut swapped.owner, &mut swapped.child);
        assert!(record.validate(&swapped, 200, None).is_err());
    }

    #[test]
    fn rejects_empty_binding_and_impossible_live_process_pair() {
        let (_, bytes) = fixture();
        for range in [
            8..24,
            24..56,
            56..60,
            60..68,
            80..112,
            112..116,
            116..124,
            136..168,
        ] {
            let mut changed = bytes.clone();
            changed[range].fill(0);
            assert!(Registration::parse(&changed).is_err());
        }
        let mut same_pid = bytes;
        let owner_pid = same_pid[56..60].to_vec();
        same_pid[112..116].copy_from_slice(&owner_pid);
        assert!(Registration::parse(&same_pid).is_err());
    }

    #[test]
    fn malformed_future_expired_or_extended_windows_cannot_gain_time() {
        let (expected, bytes) = fixture();
        for (start, expiry, now) in [
            (100u64, 100u64, 100u64),
            (100, 99, 100),
            (100, 5_101, 100),
            (201, 5_100, 200),
            (100, 5_100, 5_100),
            (u64::MAX - 2, u64::MAX, u64::MAX - 1),
        ] {
            let mut changed = bytes.clone();
            changed[168..176].copy_from_slice(&start.to_le_bytes());
            changed[176..184].copy_from_slice(&expiry.to_le_bytes());
            assert!(Registration::parse(&changed)
                .unwrap()
                .validate(&expected, now, None)
                .is_err());
        }
        let mut changed = bytes;
        changed[168..176].copy_from_slice(&200u64.to_le_bytes());
        changed[176..184].copy_from_slice(&5_200u64.to_le_bytes());
        let record = Registration::parse(&changed).unwrap();
        let mut owner = record.validate(&expected, 200, Some(5_100)).unwrap();
        assert_eq!(owner.remaining_ms(5_099), Ok(1));
        assert!(owner.check(5_100).is_err());
        assert!(record.validate(&expected, 5_100, Some(5_100)).is_err());
    }
}
