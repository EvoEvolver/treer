//! Live disposable collision fixture, never a Controller reason adapter.
use super::super::context_record::{self, Image, Record};
use super::*;

fn file_id(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info: FILE_ID_INFO = unsafe { mem::zeroed() };
    check(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            mem::size_of::<FILE_ID_INFO>() as u32,
        )
    })?;
    let id = info.FileId.Identifier;
    if info.VolumeSerialNumber == 0
        || info.VolumeSerialNumber == u64::MAX
        || id == [0; 16]
        || id == [u8::MAX; 16]
    {
        return Err(denied());
    }
    Ok((info.VolumeSerialNumber, id))
}

// Status is intentionally absent from the expected context. It is populated
// ONLY from the retained actual Failure, never the file or an expected value.
#[derive(Clone)]
struct Binding(Record);

impl Binding {
    fn before_acquisition(fixture: &Fixture, component: u8) -> io::Result<Self> {
        let image = |file: &File, expected: &RecordedImage| -> io::Result<Image> {
            let (volume, file) = file_id(file)?;
            Ok(Image {
                volume,
                file,
                size: expected.size,
                hash: expected.hash,
            })
        };
        let (volume, id) = file_id(&fixture.attempt.directory)?;
        let mut attempt = Sha256::new();
        attempt.update(b"test-only-native-attempt-v1\0");
        attempt.update(volume.to_le_bytes());
        attempt.update(id);
        let operation = if fixture.operation == Operation::MoveOld {
            1
        } else {
            2
        };
        Ok(Self(Record {
            attempt: attempt.finalize().into(),
            predecessor: Sha256::digest(b"test-only-committed-predecessor-v1\0fixture-head").into(),
            component,
            role: operation,
            operation,
            revision: 7,
            source: image(&fixture.source, &fixture.source_expected)?,
            target: image(&fixture.target, &fixture.target_expected)?,
            status: 0,
        }))
    }

    fn with_failure(&self, failure: &Failure<'_>) -> io::Result<Record> {
        // This fixture permits only occupied-slot, non-replacing rename errors.
        // It is NOT an exhaustive production Win32 error classifier.
        let operation = if failure.acquisition.operation == Operation::MoveOld {
            1
        } else {
            2
        };
        if self.0.status != 0
            || self.0.operation != operation
            || failure.status != ERROR_ALREADY_EXISTS
        {
            return Err(denied());
        }
        let mut record = self.0.clone();
        record.status = failure.status;
        record.encode().ok_or_else(denied)?;
        Ok(record)
    }

    fn check(&self, composition: &Composition<'_>) -> io::Result<()> {
        composition.check()?;
        let (volume, id) = file_id(&composition.acquisition.attempt.directory)?;
        let mut attempt = Sha256::new();
        attempt.update(b"test-only-native-attempt-v1\0");
        attempt.update(volume.to_le_bytes());
        attempt.update(id);
        if self.0.attempt != <[u8; 32]>::from(attempt.finalize())
            || self.0.revision != composition.context.intent.revision
        {
            return Err(denied());
        }
        for (file, expected, independent) in [
            (
                composition.acquisition.source,
                &self.0.source,
                &composition.context.source,
            ),
            (
                composition.acquisition.target,
                &self.0.target,
                &composition.context.target,
            ),
        ] {
            if file_id(file)? != (expected.volume, expected.file)
                || expected.size != independent.size
                || expected.hash != independent.hash
            {
                return Err(denied());
            }
        }
        Ok(())
    }
}

struct BoundStored<'a> {
    composition: &'a Composition<'a>,
    binding: &'a Binding,
    failure: &'a Failure<'a>,
    pin: evidence_file::EvidencePin<'a>,
    spent: Cell<bool>,
}

impl<'a> BoundStored<'a> {
    fn store(
        composition: &'a Composition<'a>,
        binding: &'a Binding,
        failure: &'a Failure<'a>,
    ) -> io::Result<Self> {
        if !ptr::eq(&composition.acquisition, failure.acquisition) {
            return Err(denied());
        }
        binding.check(composition)?;
        let bytes = binding.with_failure(failure)?.encode().ok_or_else(denied)?;
        let pin = composition.acquisition.attempt.store_evidence(
            &bytes,
            Sha256::digest(&bytes).into(),
            |_| Ok(()),
        )?;
        binding.check(composition)?;
        Ok(Self {
            composition,
            binding,
            failure,
            pin,
            spent: Cell::new(false),
        })
    }

    // The closure receives no bytes/capability. Success merely increments a
    // private test counter; there is no export, publication or restart path.
    fn consume(
        &self,
        expected: &Binding,
        sink: &mut Sink,
        after_read: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if self.spent.replace(true) {
            return Err(denied());
        }
        self.binding.check(self.composition)?;
        let bytes = self.pin.read()?;
        after_read()?;
        self.binding.check(self.composition)?;
        let expected = expected.with_failure(self.failure)?;
        if !context_record::matches_expected(&bytes, &expected) || sink.refuse {
            return Err(denied());
        }
        sink.accepted += 1;
        Ok(())
    }
}

#[test]
fn native_operation_bound_context_custody_and_single_use() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        for scenario in 0..5 {
            let fixture = Fixture::new(operation);
            // Independent expected identities, sizes, hashes and synthetic
            // committed inputs are fixed before acquisition, not readback.
            let binding = Binding::before_acquisition(&fixture, 1).unwrap();
            let context = fixture.context();
            let composition = Composition::new(
                &context,
                &fixture.attempt,
                &fixture.source,
                &fixture.target,
                context.intent,
            )
            .unwrap();
            binding.check(&composition).unwrap();
            let mut wrong_identity = binding.clone();
            wrong_identity.0.source.file[15] ^= 1;
            assert!(wrong_identity.check(&composition).is_err());
            assert!(!fixture.attempt.path.join("reason.evidence").exists());
            let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
                panic!("native collision required");
            };
            let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
            let expected = binding.clone(); // Distinct allocation, equal bindings.
            let mut sink = Sink {
                accepted: 0,
                refuse: scenario == 1,
            };
            let result = stored.consume(&expected, &mut sink, || {
                if scenario == 2 {
                    return Err(denied());
                }
                if scenario == 3 {
                    let spare = if operation == Operation::MoveOld {
                        Slot::Candidate
                    } else {
                        Slot::Backup
                    };
                    fixture.attempt.rename_checked(
                        &fixture.target,
                        operation.slots().1,
                        spare,
                        context.target.hash,
                    )?;
                }
                if scenario == 4 {
                    // Both external write and deletion are denied by the pin.
                    assert!(OpenOptions::new()
                        .write(true)
                        .open(fixture.attempt.path.join("reason.evidence"))
                        .is_err());
                    assert!(
                        std::fs::remove_file(fixture.attempt.path.join("reason.evidence")).is_err()
                    );
                }
                Ok(())
            });
            assert_eq!(result.is_ok(), scenario == 0 || scenario == 4);
            assert_eq!(sink.accepted, usize::from(result.is_ok()));
            assert!(stored
                .consume(&expected, &mut sink, || panic!("spent"))
                .is_err());
            assert!(BoundStored::store(&composition, &binding, &failure).is_err());
            assert!(matches!(
                composition.acquisition.capture(|_| panic!("spent")),
                Captured::Refused
            ));
            drop(stored);
            cleanup(fixture);
        }
    }
}

#[test]
fn native_operation_bound_context_refuses_foreign_bindings() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        for mismatch in 0..10 {
            let fixture = Fixture::new(operation);
            let binding = Binding::before_acquisition(&fixture, 2).unwrap();
            let context = fixture.context();
            let composition = Composition::new(
                &context,
                &fixture.attempt,
                &fixture.source,
                &fixture.target,
                context.intent,
            )
            .unwrap();
            binding.check(&composition).unwrap();
            let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
                panic!("native collision required");
            };
            let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
            let mut expected = binding.clone();
            match mismatch {
                0 => expected.0.attempt[0] ^= 1,
                1 => expected.0.predecessor[0] ^= 1,
                2 => expected.0.component = 1,
                3 => expected.0.revision += 1,
                4 => expected.0.source.file[15] ^= 1,
                5 => expected.0.target.file[15] ^= 1,
                6 => expected.0.source.hash[0] ^= 1,
                7 => expected.0.target.size += 1,
                8 => {
                    expected.0.operation = if operation == Operation::MoveOld {
                        2
                    } else {
                        1
                    };
                    expected.0.role = expected.0.operation;
                }
                9 => expected.0.status = ERROR_ACCESS_DENIED, // Caller status forbidden, even if nonzero.
                _ => unreachable!(),
            }
            let mut sink = Sink::default();
            assert!(stored.consume(&expected, &mut sink, || Ok(())).is_err());
            assert_eq!(sink.accepted, 0);
            assert!(stored
                .consume(&binding, &mut sink, || panic!("refusal spent evidence"))
                .is_err());
            drop(stored);
            cleanup(fixture);
        }
    }
}

#[test]
fn native_operation_bound_context_refuses_foreign_failure_and_status() {
    let first = Fixture::new(Operation::MoveOld);
    let second = Fixture::new(Operation::Publish);
    let binding = Binding::before_acquisition(&first, 1).unwrap();
    let foreign_binding = Binding::before_acquisition(&second, 1).unwrap();
    assert_ne!(binding.0.attempt, foreign_binding.0.attempt);
    let context = first.context();
    let foreign_context = second.context();
    let composition = Composition::new(
        &context,
        &first.attempt,
        &first.source,
        &first.target,
        context.intent,
    )
    .unwrap();
    let foreign = Composition::new(
        &foreign_context,
        &second.attempt,
        &second.source,
        &second.target,
        foreign_context.intent,
    )
    .unwrap();
    binding.check(&composition).unwrap();
    foreign_binding.check(&foreign).unwrap();
    let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
        panic!("native collision required");
    };
    let Captured::Failure(other) = foreign.acquisition.capture(|_| Ok(())) else {
        panic!("native collision required");
    };
    assert!(BoundStored::store(&composition, &binding, &other).is_err());
    assert!(binding.with_failure(&other).is_err());
    // Both occupied-slot operations actually returned the SAME error. Changing
    // only the operation must still fail; status coincidence proves nothing.
    assert_eq!(failure.status, other.status);
    let actual = binding.with_failure(&failure).unwrap();
    let mut wrong_status = actual.clone();
    wrong_status.status = ERROR_ACCESS_DENIED;
    assert!(!context_record::matches_expected(
        &wrong_status.encode().unwrap(),
        &actual
    ));
    let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
    let mut sink = Sink::default();
    assert!(stored
        .consume(&foreign_binding, &mut sink, || Ok(()))
        .is_err());
    assert_eq!(sink.accepted, 0);
    assert!(stored
        .consume(&binding, &mut sink, || panic!("spent"))
        .is_err());
    drop(stored);
    cleanup(first);
    cleanup(second);
}
