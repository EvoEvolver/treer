//! Live disposable collision fixture, never a Controller reason adapter.
use super::super::context_record::{self, Image, Record};
use super::*;

mod retained_head;

// Private test-only provenance data, not authority or an unforgeable token.
// Deliberately no conversion from arbitrary digest arrays at call sites.
struct MatchedDigest([u8; 32]);

impl MatchedDigest {
    fn bytes(&self) -> [u8; 32] {
        self.0
    }
}

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
        self.consume_matched_digest(expected, sink, |_| after_read())
    }

    // Only typed digest data from the first matched read reaches this private
    // test hook. The sink still receives no bytes, path, token or capability.
    fn consume_matched_digest(
        &self,
        expected: &Binding,
        sink: &mut Sink,
        after_match: impl FnOnce(MatchedDigest) -> io::Result<()>,
    ) -> io::Result<()> {
        self.consume_with_guard(expected, sink, after_match)
    }

    // The private hook may retain custody across the second evidence read and
    // synchronous sink. This generic lifetime is not a recovered capability.
    fn consume_with_guard<G>(
        &self,
        expected: &Binding,
        sink: &mut Sink,
        after_match: impl FnOnce(MatchedDigest) -> io::Result<G>,
    ) -> io::Result<()> {
        if self.spent.replace(true) {
            return Err(denied());
        }
        self.binding.check(self.composition)?;
        let bytes = self.pin.read()?;
        let expected = expected.with_failure(self.failure)?;
        // The callback is not allowed to observe an unmatched context. In
        // particular, future inert decision creation must follow this check.
        if !context_record::matches_expected(&bytes, &expected) {
            return Err(denied());
        }
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let guard = after_match(MatchedDigest(digest))?;
        self.binding.check(self.composition)?;
        let reread = self.pin.read()?;
        if <[u8; 32]>::from(Sha256::digest(&reread)) != digest {
            return Err(denied());
        }
        self.binding.check(self.composition)?;
        sink.accept()?;
        // Explicitly retain custody until after the synchronous sink completes.
        drop(guard);
        Ok(())
    }
}

#[test]
fn native_operation_guard_released_after_acceptance_or_refusal() {
    struct Guard<'a> {
        drops: &'a Cell<usize>,
        accepted: &'a Cell<usize>,
        accepted_at_drop: &'a Cell<usize>,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.accepted_at_drop.set(self.accepted.get());
            self.drops.set(self.drops.get() + 1);
        }
    }
    for operation in [Operation::MoveOld, Operation::Publish] {
        for scenario in 0..3 {
            let refuse = scenario == 1;
            let fixture = Fixture::new(operation);
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
            let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
                panic!("native collision required");
            };
            let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
            let drops = std::rc::Rc::new(Cell::new(0));
            let accepted = std::rc::Rc::new(Cell::new(0));
            let accepted_at_drop = Cell::new(usize::MAX);
            let observe_drops = drops.clone();
            let observe_accepted = accepted.clone();
            let mut sink = Sink {
                accepted: 0,
                refuse,
                on_accept: Some(Box::new(move |count| {
                    // This executes inside the real private sink, not its hook.
                    assert_eq!(observe_drops.get(), 0);
                    observe_accepted.set(count);
                    assert_ne!(scenario, 2, "injected sink panic");
                })),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stored.consume_with_guard(&binding, &mut sink, |_| {
                    assert!(stored.spent.get());
                    assert_eq!(drops.get(), 0);
                    Ok(Guard {
                        drops: &drops,
                        accepted: &accepted,
                        accepted_at_drop: &accepted_at_drop,
                    })
                })
            }));
            assert_eq!(result.is_err(), scenario == 2);
            if let Ok(result) = result {
                assert_eq!(result.is_ok(), !refuse);
            }
            assert_eq!(sink.accepted, usize::from(!refuse));
            assert_eq!(drops.get(), 1);
            assert_eq!(accepted_at_drop.get(), usize::from(!refuse));
            assert!(stored.spent.get());
            assert!(stored
                .consume_with_guard::<Guard<'_>>(&binding, &mut sink, |_| panic!("spent"))
                .is_err());
            assert_eq!(drops.get(), 1);
            assert!(!fixture.attempt.path.join("consume.intent").exists());
            eprintln!(
                "guard_fixture refused={} drops=1 spent=1 decision=0 sink={} observed_at_drop={} panic={}",
                u8::from(refuse),
                sink.accepted,
                accepted_at_drop.get(),
                u8::from(scenario == 2)
            );
            drop(stored);
            cleanup(fixture);
        }
    }
}

#[test]
fn native_operation_matched_digest_handoff_and_spent_failure() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        for refuse in [false, true] {
            let fixture = Fixture::new(operation);
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
            let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
                panic!("native collision required");
            };
            let expected = binding.with_failure(&failure).unwrap().encode().unwrap();
            let expected_digest: [u8; 32] = Sha256::digest(&expected).into();
            let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
            let calls = Cell::new(0);
            let mut sink = Sink::default();
            let result = stored.consume_matched_digest(&binding, &mut sink, |digest| {
                calls.set(calls.get() + 1);
                assert_eq!(digest.bytes(), expected_digest);
                assert!(stored.spent.get());
                if refuse {
                    return Err(denied());
                }
                Ok(())
            });
            assert_eq!(calls.get(), 1);
            assert_eq!(result.is_ok(), !refuse);
            assert_eq!(sink.accepted, usize::from(!refuse));
            assert!(stored
                .consume_matched_digest(&binding, &mut sink, |_| panic!("spent"))
                .is_err());
            assert_eq!(sink.accepted, usize::from(!refuse));
            assert_eq!(stored.pin.read().unwrap(), expected);
            assert!(!fixture.attempt.path.join("consume.intent").exists());
            eprintln!(
                "context_digest_fixture operation={} hook_refused={} digest_match=1 hook_calls=1 spent=1 sink={}",
                if operation == Operation::MoveOld { "MoveOld" } else { "Publish" },
                u8::from(refuse),
                sink.accepted
            );
            drop(stored);
            cleanup(fixture);
        }
    }
}

#[test]
fn native_operation_bound_context_actual_evidence_read_failure() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        let fixture = Fixture::new(operation);
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
        let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
            panic!("native collision required");
        };
        let mut stored = BoundStored::store(&composition, &binding, &failure).unwrap();
        let expected_bytes = stored.pin.read().unwrap();
        assert_eq!(
            stored.pin.read_trace(),
            evidence_file::ReadTrace {
                validated: true,
                read_calls: 2,
                read_error: None,
                post_validated: true,
                hash_compared: true,
            }
        );
        eprintln!("evidence_fixture stage=ReadControl win32=0");
        let mut raw = ptr::null_mut();
        // Keep ACL/metadata rights; specifically remove FILE_READ_DATA. The
        // original remains held until the duplicate and full-ID proof exist.
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                stored.pin.fixture_file().as_raw_handle(),
                GetCurrentProcess(),
                &mut raw,
                FILE_READ_ATTRIBUTES | READ_CONTROL,
                0,
                0,
            )
        })
        .unwrap();
        let reduced = unsafe { File::from_raw_handle(raw) };
        assert_eq!(
            file_id(&reduced).unwrap(),
            file_id(stored.pin.fixture_file()).unwrap()
        );
        let original = stored.pin.fixture_replace_file(reduced);
        let mut sink = Sink {
            accepted: 0,
            refuse: false,
            ..Sink::default()
        };
        assert!(stored
            .consume(&binding, &mut sink, || panic!("read must fail first"))
            .is_err());
        assert_eq!(sink.accepted, 0);
        assert_eq!(
            stored.pin.read_trace(),
            evidence_file::ReadTrace {
                validated: true,
                read_calls: 1,
                read_error: Some(ERROR_ACCESS_DENIED),
                post_validated: false,
                hash_compared: false,
            }
        );
        drop(stored.pin.fixture_replace_file(original));
        // The original pin proves exact bytes/length/hash unchanged. Reading it
        // grants no fresh consumption authority after the failed verification.
        assert_eq!(stored.pin.read().unwrap(), expected_bytes);
        assert!(stored
            .consume(&binding, &mut sink, || panic!("spent"))
            .is_err());
        assert_eq!(sink.accepted, 0);
        assert!(BoundStored::store(&composition, &binding, &failure).is_err());
        drop(stored);
        cleanup(fixture);
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
                ..Sink::default()
            };
            let hook_ran = Cell::new(false);
            let result = stored.consume(&expected, &mut sink, || {
                hook_ran.set(true);
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
            assert!(hook_ran.get());
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
            assert!(stored
                .consume(&expected, &mut sink, || panic!(
                    "unmatched context reached hook"
                ))
                .is_err());
            assert_eq!(sink.accepted, 0);
            assert!(stored.spent.get());
            assert!(!fixture.attempt.path.join("consume.intent").exists());
            assert!(stored
                .consume(&binding, &mut sink, || panic!("refusal spent evidence"))
                .is_err());
            drop(stored);
            cleanup(fixture);
        }
        eprintln!(
            "context_order_fixture operation={} mismatches=10 hook=0 sink=0 spent=1 decision=0",
            if operation == Operation::MoveOld {
                "MoveOld"
            } else {
                "Publish"
            }
        );
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

#[test]
fn native_operation_bound_context_distinguishes_actual_access_denied() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        let mut denied_fixture = Fixture::new(operation);
        // Reduce rights on the same object, without reopening by pathname or
        // closing the original before the duplicate exists. Hash/identity/ACL
        // observations still work, but a native rename needs DELETE access.
        let mut raw = ptr::null_mut();
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                denied_fixture.source.as_raw_handle(),
                GetCurrentProcess(),
                &mut raw,
                FILE_GENERIC_READ,
                0,
                0,
            )
        })
        .unwrap();
        let readonly = unsafe { File::from_raw_handle(raw) };
        assert_eq!(
            file_id(&readonly).unwrap(),
            file_id(&denied_fixture.source).unwrap()
        );
        denied_fixture.source = readonly;
        let binding = Binding::before_acquisition(&denied_fixture, 1).unwrap();
        let context = denied_fixture.context();
        let composition = Composition::new(
            &context,
            &denied_fixture.attempt,
            &denied_fixture.source,
            &denied_fixture.target,
            context.intent,
        )
        .unwrap();
        binding.check(&composition).unwrap();
        let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
            panic!("actual native permission failure required");
        };
        assert_eq!(failure.status, ERROR_ACCESS_DENIED);
        // Do not broaden the collision-only acceptance policy because the new
        // status is genuine. It still cannot mint evidence accepted by it.
        assert!(binding.with_failure(&failure).is_err());
        assert!(BoundStored::store(&composition, &binding, &failure).is_err());
        assert!(!denied_fixture.attempt.path.join("reason.evidence").exists());
        assert!(matches!(
            composition.acquisition.capture(|_| panic!("spent")),
            Captured::Refused
        ));

        let collision_fixture = Fixture::new(operation);
        let collision_binding = Binding::before_acquisition(&collision_fixture, 1).unwrap();
        let collision_context = collision_fixture.context();
        let collision = Composition::new(
            &collision_context,
            &collision_fixture.attempt,
            &collision_fixture.source,
            &collision_fixture.target,
            collision_context.intent,
        )
        .unwrap();
        collision_binding.check(&collision).unwrap();
        let Captured::Failure(captured_collision) = collision.acquisition.capture(|_| Ok(()))
        else {
            panic!("actual native collision required");
        };
        let expected = collision_binding.with_failure(&captured_collision).unwrap();
        assert_ne!(captured_collision.status, failure.status);
        let mut substituted = expected.clone();
        substituted.status = failure.status;
        assert!(!context_record::matches_expected(
            &substituted.encode().unwrap(),
            &expected
        ));
        assert!(BoundStored::store(&collision, &collision_binding, &failure).is_err());
        let stored =
            BoundStored::store(&collision, &collision_binding, &captured_collision).unwrap();
        let mut sink = Sink::default();
        stored
            .consume(&collision_binding.clone(), &mut sink, || Ok(()))
            .unwrap();
        assert_eq!(sink.accepted, 1);
        drop(stored);
        cleanup(denied_fixture);
        cleanup(collision_fixture);
    }
}
