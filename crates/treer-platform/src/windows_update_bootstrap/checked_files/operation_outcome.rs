//! Disposable native outcome acquisition; never a Controller reason source.
use super::*;
use std::cell::Cell;

mod composition;
mod context_record;

pub(super) enum RenameError {
    Before(io::Error),
    Native(u32),
    Uncertain(io::Error),
}

impl From<io::Error> for RenameError {
    fn from(error: io::Error) -> Self {
        Self::Before(error)
    }
}

// Slot selection, not a caller-supplied failure label, determines the stage.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    MoveOld,
    Publish,
}

// Decoding bytes grants no provenance: only the retained native outcome and
// its evidence pin can supply the comparison below.
fn decode(bytes: &[u8]) -> Option<(Operation, u32)> {
    let [stage, a, b, c, d] = bytes else {
        return None;
    };
    let operation = match stage {
        1 => Operation::MoveOld,
        2 => Operation::Publish,
        _ => return None,
    };
    let status = u32::from_le_bytes([*a, *b, *c, *d]);
    (status != 0).then_some((operation, status))
}

impl Operation {
    fn slots(self) -> (Slot, Slot) {
        match self {
            Self::MoveOld => (Slot::Active, Slot::Backup),
            Self::Publish => (Slot::Candidate, Slot::Active),
        }
    }
}

struct Acquisition<'a> {
    attempt: &'a Attempt,
    source: &'a File,
    target: &'a File,
    operation: Operation,
    source_record: RecordedImage,
    target_record: RecordedImage,
    used: Cell<bool>,
}

// A captured failure borrows all the custody used to obtain it. No Clone,
// serde, path constructor, caller status constructor or public conversion.
struct Failure<'a> {
    acquisition: &'a Acquisition<'a>,
    status: u32,
}

enum Captured<'a> {
    Success,
    Refused,
    Uncertain,
    Failure(Failure<'a>),
}

fn same(file: &File, expected: &RecordedImage, path: &Path) -> io::Result<()> {
    let current = RecordedImage::capture(file, expected.hash)?;
    if current.identity != expected.identity
        || current.size != expected.size
        || final_path(file)? != path
    {
        return Err(denied());
    }
    Ok(())
}

impl<'a> Acquisition<'a> {
    // This first negative fixture requires a pinned occupied destination.
    // No absence claim or production operation-intent revision is accepted.
    fn new(
        attempt: &'a Attempt,
        source: &'a File,
        target: &'a File,
        operation: Operation,
        hash: [u8; 32],
    ) -> io::Result<Self> {
        let source_record = RecordedImage::capture(source, hash)?;
        let mut reader = target;
        reader.seek(SeekFrom::Start(0))?;
        let target_record = RecordedImage::capture(target, digest(target)?)?;
        if source_record.identity == target_record.identity {
            return Err(denied());
        }
        let value = Self {
            attempt,
            source,
            target,
            operation,
            source_record,
            target_record,
            used: Cell::new(false),
        };
        value.reobserve()?;
        Ok(value)
    }

    fn reobserve(&self) -> io::Result<()> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let directory = evidence_file::directory(self.attempt, &owner)?;
        let (from, to) = self.operation.slots();
        same(
            self.source,
            &self.source_record,
            &directory.join(from.name()),
        )?;
        same(self.target, &self.target_record, &directory.join(to.name()))
    }

    fn capture(&'a self, after_call: impl FnMut(bool) -> io::Result<()>) -> Captured<'a> {
        // Taken before any native call; all failures leave this object spent.
        if self.used.replace(true) || self.reobserve().is_err() {
            return Captured::Refused;
        }
        let (from, to) = self.operation.slots();
        match self.attempt.rename_captured(
            self.source,
            from,
            to,
            self.source_record.hash,
            after_call,
        ) {
            Ok(()) => Captured::Success,
            Err(RenameError::Before(_)) => Captured::Refused,
            Err(RenameError::Uncertain(_)) => Captured::Uncertain,
            Err(RenameError::Native(status)) => {
                if status == 0 || self.reobserve().is_err() {
                    Captured::Uncertain
                } else {
                    Captured::Failure(Failure {
                        acquisition: self,
                        status,
                    })
                }
            }
        }
    }
}

struct Stored<'a> {
    failure: &'a Failure<'a>,
    pin: evidence_file::EvidencePin<'a>,
    verification_used: Cell<bool>,
}
impl Failure<'_> {
    // Fixed bounded fixture encoding, not the Controller observation schema.
    fn bytes(&self) -> [u8; 5] {
        let mut bytes = [0; 5];
        bytes[0] = match self.acquisition.operation {
            Operation::MoveOld => 1,
            Operation::Publish => 2,
        };
        bytes[1..].copy_from_slice(&self.status.to_le_bytes());
        bytes
    }
    fn store(&self, fail_after_create: bool) -> io::Result<Stored<'_>> {
        self.acquisition.reobserve()?;
        let bytes = self.bytes();
        let pin = self.acquisition.attempt.store_evidence(
            &bytes,
            Sha256::digest(bytes).into(),
            |_| {
                if fail_after_create {
                    Err(denied())
                } else {
                    Ok(())
                }
            },
        )?;
        Ok(Stored {
            failure: self,
            pin,
            verification_used: Cell::new(false),
        })
    }
}
impl Stored<'_> {
    fn read(&self) -> io::Result<Vec<u8>> {
        self.failure.acquisition.reobserve()?;
        let bytes = self.pin.read()?;
        if decode(&bytes) != Some((self.failure.acquisition.operation, self.failure.status)) {
            return Err(denied());
        }
        Ok(bytes)
    }
}

#[test]
fn native_operation_record_decoder_rejects_malformed_bytes_without_authority() {
    // Every stage byte is classified, including all unsupported tags. Status is
    // unsigned little-endian; neither its magnitude nor parse success grants
    // custody of the actual operation.
    for stage in 0..=u8::MAX {
        let bytes = [stage, 0x78, 0x56, 0x34, 0x12];
        let result = decode(&bytes);
        match stage {
            1 => assert!(result == Some((Operation::MoveOld, 0x12345678))),
            2 => assert!(result == Some((Operation::Publish, 0x12345678))),
            _ => assert!(result.is_none()),
        }
    }
    for operation in [Operation::MoveOld, Operation::Publish] {
        let stage = match operation {
            Operation::MoveOld => 1,
            Operation::Publish => 2,
        };
        assert!(decode(&[stage, 0, 0, 0, 0]).is_none());
        for status in [1u32, 183, u32::MAX] {
            let mut bytes = vec![stage];
            bytes.extend_from_slice(&status.to_le_bytes());
            assert!(decode(&bytes) == Some((operation, status)));
            for length in 0..bytes.len() {
                assert!(decode(&bytes[..length]).is_none());
            }
            bytes.push(0);
            assert!(decode(&bytes).is_none());
        }
    }
}

#[test]
fn native_operation_capture_is_single_use_and_evidence_stays_borrowed() {
    let fixture = super::super::tests::PrivateImage::new();
    for (index, operation) in [Operation::MoveOld, Operation::Publish]
        .into_iter()
        .enumerate()
    {
        let name = format!("attempt-{:032x}", 70 + index);
        let path = fixture.directory.join(&name);
        let attempt = Attempt::create(&fixture.directory, &name).unwrap();
        let input = open_pinned(&fixture.path, false).unwrap();
        let hash = digest(&input).unwrap();
        let target = attempt.copy_for_rename(&input, hash).unwrap();
        let (_, to) = operation.slots();
        attempt
            .rename_checked(&target, Slot::Candidate, to, hash)
            .unwrap();
        let source = attempt.copy_for_rename(&input, hash).unwrap();
        let (from, _) = operation.slots();
        if operation == Operation::MoveOld {
            attempt
                .rename_checked(&source, Slot::Candidate, from, hash)
                .unwrap();
        }
        let acquisition = Acquisition::new(&attempt, &source, &target, operation, hash).unwrap();
        let calls = Cell::new(0);
        let Captured::Failure(failure) = acquisition.capture(|success| {
            assert!(!success);
            calls.set(calls.get() + 1);
            Ok(())
        }) else {
            panic!("expected checked native collision");
        };
        assert!(failure.status > 0);
        assert!(matches!(
            acquisition.capture(|_| {
                calls.set(calls.get() + 1);
                Ok(())
            }),
            Captured::Refused
        ));
        assert_eq!(calls.get(), 1);
        let stored = failure.store(false).unwrap();
        assert_eq!(stored.read().unwrap(), failure.bytes());
        assert!(failure.store(false).is_err());
        for slot in [from, to] {
            for access in [FILE_WRITE_DATA, DELETE] {
                assert!(OpenOptions::new()
                    .access_mode(access)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                    .open(path.join(slot.name()))
                    .is_err());
            }
        }
        drop(stored);
        drop((source, target, input, attempt));
        for slot in [from, to] {
            std::fs::remove_file(path.join(slot.name())).unwrap();
        }
        std::fs::remove_file(path.join("reason.evidence")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}

#[test]
fn native_operation_capture_refuses_interference_and_failed_storage_without_retry() {
    let fixture = super::super::tests::PrivateImage::new();
    for injection in 0..3 {
        let name = format!("attempt-{:032x}", 80 + injection);
        let path = fixture.directory.join(&name);
        let attempt = Attempt::create(&fixture.directory, &name).unwrap();
        let input = open_pinned(&fixture.path, false).unwrap();
        let hash = digest(&input).unwrap();
        let target = attempt.copy_for_rename(&input, hash).unwrap();
        attempt
            .rename_checked(&target, Slot::Candidate, Slot::Active, hash)
            .unwrap();
        let source = attempt.copy_for_rename(&input, hash).unwrap();
        let acquisition =
            Acquisition::new(&attempt, &source, &target, Operation::Publish, hash).unwrap();
        if injection == 0 {
            // Explicit test interference uses an already owned DELETE handle;
            // ordinary external openers still cannot acquire mutation access.
            assert!(matches!(
                acquisition.capture(|success| {
                    assert!(!success);
                    attempt.rename_checked(&target, Slot::Active, Slot::Backup, hash)
                }),
                Captured::Uncertain
            ));
            assert!(!path.join("reason.evidence").exists());
        } else if injection == 1 {
            let Captured::Failure(failure) = acquisition.capture(|_| Ok(())) else {
                panic!("expected collision");
            };
            assert!(failure.store(true).is_err());
            assert!(failure.store(false).is_err());
            assert_eq!(
                std::fs::metadata(path.join("reason.evidence"))
                    .unwrap()
                    .len(),
                0
            );
        } else {
            attempt
                .rename_checked(&target, Slot::Active, Slot::Backup, hash)
                .unwrap();
            assert!(matches!(
                acquisition.capture(|_| panic!("precondition must not call Windows")),
                Captured::Refused
            ));
        }
        assert!(matches!(
            acquisition.capture(|_| panic!("cannot retry")),
            Captured::Refused
        ));
        drop((source, target, input, attempt));
        std::fs::remove_file(path.join("candidate.exe")).unwrap();
        std::fs::remove_file(path.join(if injection == 1 {
            "controller.exe"
        } else {
            "controller.prev.exe"
        }))
        .unwrap();
        if injection == 1 {
            std::fs::remove_file(path.join("reason.evidence")).unwrap();
        }
        std::fs::remove_dir(path).unwrap();
    }
}

#[test]
fn native_operation_success_and_postcheck_refusal_are_not_failure_observations() {
    let fixture = super::super::tests::PrivateImage::new();
    for uncertain in [false, true] {
        let name = format!("attempt-{:032x}", 90 + usize::from(uncertain));
        let path = fixture.directory.join(&name);
        let attempt = Attempt::create(&fixture.directory, &name).unwrap();
        let input = open_pinned(&fixture.path, false).unwrap();
        let hash = digest(&input).unwrap();
        let source = attempt.copy_for_rename(&input, hash).unwrap();
        let result =
            attempt.rename_captured(&source, Slot::Candidate, Slot::Active, hash, |success| {
                assert!(success);
                if uncertain {
                    Err(denied())
                } else {
                    Ok(())
                }
            });
        if uncertain {
            assert!(matches!(result, Err(RenameError::Uncertain(_))));
        } else {
            assert!(result.is_ok());
        }
        assert!(!path.join("reason.evidence").exists());
        assert!(final_path(&source).unwrap().ends_with("controller.exe"));
        drop((source, input, attempt));
        std::fs::remove_file(path.join("controller.exe")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}
