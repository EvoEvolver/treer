//! Private native-fixture composition. No Controller conversion or publication.
use super::*;

mod native_binding;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Old,
    New,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Intent {
    operation: Operation,
    role: Role,
    revision: u64,
}

// Independently supplied before acquisition; snapshots are expectations, not
// authority reconstructed from the outcome bytes or whatever target is found.
struct Context<'a> {
    attempt: &'a Attempt,
    attempt_identity: (u32, u64, u32, u32),
    intent: Intent,
    source: RecordedImage,
    target: RecordedImage,
}

struct Composition<'a> {
    context: &'a Context<'a>,
    acquisition: Acquisition<'a>,
}

impl<'a> Composition<'a> {
    fn new(
        context: &'a Context<'a>,
        attempt: &'a Attempt,
        source: &'a File,
        target: &'a File,
        intent: Intent,
    ) -> io::Result<Self> {
        if !ptr::eq(context.attempt, attempt)
            || context.intent != intent
            || intent.revision == 0
            || intent.role
                != match intent.operation {
                    Operation::MoveOld => Role::Old,
                    Operation::Publish => Role::New,
                }
        {
            return Err(denied());
        }
        // Validate both independent expectations BEFORE Acquisition::new can
        // snapshot a destination, and again for all subsequent custody checks.
        Self::check_images(context, source, target)?;
        let acquisition = Acquisition::new(
            attempt,
            source,
            target,
            intent.operation,
            context.source.hash,
        )?;
        let value = Self {
            context,
            acquisition,
        };
        value.check()?;
        Ok(value)
    }

    fn check_images(context: &Context<'_>, source: &File, target: &File) -> io::Result<()> {
        if identity(&context.attempt.directory)? != context.attempt_identity {
            return Err(denied());
        }
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let directory = evidence_file::directory(context.attempt, &owner)?;
        let (from, to) = context.intent.operation.slots();
        same(source, &context.source, &directory.join(from.name()))?;
        same(target, &context.target, &directory.join(to.name()))
    }

    fn check(&self) -> io::Result<()> {
        Self::check_images(
            self.context,
            self.acquisition.source,
            self.acquisition.target,
        )?;
        self.acquisition.reobserve()
    }
}

struct Verifier<'a> {
    composition: &'a Composition<'a>,
    used: Cell<bool>,
}

// No Clone/serde/byte/path constructors. Exact context, acquisition and pin
// remain borrowed through the single private in-memory consumption.
struct Accepted<'a> {
    verifier: &'a Verifier<'a>,
    stored: &'a Stored<'a>,
}

impl<'a> Verifier<'a> {
    fn verify(
        &'a self,
        context: &Context<'_>,
        stored: &'a Stored<'a>,
        after_read: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Accepted<'a>> {
        // Refusal and read failure spend the invocation too; creating another
        // consumer cannot renew acquisition or evidence-store latches.
        if self.used.replace(true)
            || stored.verification_used.replace(true)
            || !ptr::eq(context, self.composition.context)
            || !ptr::eq(stored.failure.acquisition, &self.composition.acquisition)
        {
            return Err(denied());
        }
        self.composition.check()?;
        let bytes = stored.read()?;
        after_read()?;
        self.composition.check()?;
        if decode(&bytes)
            != Some((
                self.composition.context.intent.operation,
                stored.failure.status,
            ))
        {
            return Err(denied());
        }
        Ok(Accepted {
            verifier: self,
            stored,
        })
    }
}

#[derive(Default)]
struct Sink {
    accepted: usize,
    refuse: bool,
}

impl Accepted<'_> {
    fn consume(self, sink: &mut Sink) -> io::Result<()> {
        self.verifier.composition.check()?;
        if !ptr::eq(
            self.stored.failure.acquisition,
            &self.verifier.composition.acquisition,
        ) || sink.refuse
        {
            return Err(denied());
        }
        sink.accepted += 1;
        Ok(())
    }
}

// Distinct source/destination bytes, with expectations retained before any
// acquisition. Occupied destinations are negative fixtures, not rollback layouts.
struct Fixture {
    _root: super::super::super::tests::PrivateImage,
    attempt: Attempt,
    source: File,
    target: File,
    operation: Operation,
    source_expected: RecordedImage,
    target_expected: RecordedImage,
}

impl Fixture {
    fn new(operation: Operation) -> Self {
        let old = b"old fixture image".as_slice();
        let new = b"new fixture image with distinct bytes".as_slice();
        let (source_bytes, target_bytes) = if operation == Operation::MoveOld {
            (old, new)
        } else {
            (new, old)
        };
        let root = super::super::super::tests::PrivateImage::new();
        std::fs::write(&root.path, target_bytes).unwrap();
        let attempt =
            Attempt::create(&root.directory, "attempt-000000000000000000000000000000a1").unwrap();
        let input = open_pinned(&root.path, false).unwrap();
        let hash = Sha256::digest(target_bytes).into();
        let target = attempt.copy_for_rename(&input, hash).unwrap();
        let target_expected = RecordedImage::capture(&target, hash).unwrap();
        assert_eq!(target_expected.size, target_bytes.len() as u64);
        let (from, to) = operation.slots();
        attempt
            .rename_checked(&target, Slot::Candidate, to, hash)
            .unwrap();
        drop(input);
        std::fs::write(&root.path, source_bytes).unwrap();
        let input = open_pinned(&root.path, false).unwrap();
        let hash = Sha256::digest(source_bytes).into();
        let source = attempt.copy_for_rename(&input, hash).unwrap();
        let source_expected = RecordedImage::capture(&source, hash).unwrap();
        assert_eq!(source_expected.size, source_bytes.len() as u64);
        if operation == Operation::MoveOld {
            attempt
                .rename_checked(&source, Slot::Candidate, from, hash)
                .unwrap();
        }
        Self {
            _root: root,
            attempt,
            source,
            target,
            operation,
            source_expected,
            target_expected,
        }
    }

    fn context(&self) -> Context<'_> {
        let expectation = |record: &RecordedImage| RecordedImage {
            identity: record.identity,
            size: record.size,
            hash: record.hash,
        };
        Context {
            attempt: &self.attempt,
            attempt_identity: identity(&self.attempt.directory).unwrap(),
            intent: Intent {
                operation: self.operation,
                role: if self.operation == Operation::MoveOld {
                    Role::Old
                } else {
                    Role::New
                },
                revision: 7,
            },
            source: expectation(&self.source_expected),
            target: expectation(&self.target_expected),
        }
    }
}

#[test]
fn native_operation_composition_refuses_independent_context_mismatches() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        let fixture = Fixture::new(operation);
        for mismatch in 0..12 {
            let mut context = fixture.context();
            let mut intent = context.intent;
            match mismatch {
                0 => context.source.identity.1 ^= 1,
                1 => context.source.size += 1,
                2 => context.source.hash[0] ^= 1,
                3 => context.target.identity.1 ^= 1,
                4 => context.target.size += 1,
                5 => context.target.hash[0] ^= 1,
                6 => {
                    context.intent.role = if intent.role == Role::Old {
                        Role::New
                    } else {
                        Role::Old
                    }
                }
                7 => {
                    context.intent.operation = if operation == Operation::MoveOld {
                        Operation::Publish
                    } else {
                        Operation::MoveOld
                    }
                }
                8 => context.intent.revision += 1,
                9 => {
                    context.intent.revision = 0;
                    intent = context.intent;
                }
                10 => {
                    context.intent.role = if intent.role == Role::Old {
                        Role::New
                    } else {
                        Role::Old
                    };
                    intent = context.intent;
                }
                _ => context.attempt_identity.1 ^= 1,
            }
            assert!(Composition::new(
                &context,
                &fixture.attempt,
                &fixture.source,
                &fixture.target,
                intent
            )
            .is_err());
            assert!(!fixture.attempt.path.join("reason.evidence").exists());
        }
        cleanup(fixture);
    }
}

fn cleanup(fixture: Fixture) {
    let path = fixture.attempt.path.clone();
    drop(fixture);
    for slot in [Slot::Candidate, Slot::Active, Slot::Backup] {
        let file = path.join(slot.name());
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
    let evidence = path.join("reason.evidence");
    if evidence.exists() {
        std::fs::remove_file(evidence).unwrap();
    }
    std::fs::remove_dir(&path).unwrap();
    // PrivateImage dropped before its child directory was removed.
    std::fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[test]
fn native_operation_composition_consumes_once_and_spends_failures() {
    for case in 0..10 {
        let scenario = case % 5;
        let fixture = Fixture::new(if case < 5 {
            Operation::Publish
        } else {
            Operation::MoveOld
        });
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
            panic!("expected native collision");
        };
        let stored = failure.store(false).unwrap();
        let verifier = Verifier {
            composition: &composition,
            used: Cell::new(false),
        };
        let mut sink = Sink {
            refuse: scenario == 1,
            ..Sink::default()
        };
        if scenario == 2 {
            let other = fixture.context();
            assert!(verifier.verify(&other, &stored, || Ok(())).is_err());
        } else if scenario == 3 {
            assert!(verifier
                .verify(&context, &stored, || Err(denied()))
                .is_err());
        } else if scenario == 4 {
            // Owned DELETE handle interference specifically between read and
            // the second custody check; external mutation openers stay denied.
            assert!(verifier
                .verify(&context, &stored, || {
                    let (_, target) = fixture.operation.slots();
                    let spare = if fixture.operation == Operation::Publish {
                        Slot::Backup
                    } else {
                        Slot::Candidate
                    };
                    fixture.attempt.rename_checked(
                        &fixture.target,
                        target,
                        spare,
                        context.target.hash,
                    )
                })
                .is_err());
        } else {
            let accepted = verifier.verify(&context, &stored, || Ok(())).unwrap();
            assert_eq!(accepted.consume(&mut sink).is_ok(), scenario == 0);
        }
        assert_eq!(sink.accepted, usize::from(scenario == 0));
        assert!(verifier
            .verify(&context, &stored, || panic!("spent verifier"))
            .is_err());
        let another = Verifier {
            composition: &composition,
            used: Cell::new(false),
        };
        assert!(another
            .verify(&context, &stored, || panic!("spent stored evidence"))
            .is_err());
        assert!(matches!(
            composition
                .acquisition
                .capture(|_| panic!("spent acquisition")),
            Captured::Refused
        ));
        assert!(failure.store(false).is_err());
        drop(stored);
        cleanup(fixture);
    }
}

#[test]
fn native_operation_composition_refuses_cross_attempt_and_same_bytes_other_identity() {
    let first = Fixture::new(Operation::Publish);
    let second = Fixture::new(Operation::Publish);
    let context = first.context();
    let other_context = second.context();
    assert_eq!(context.target.hash, other_context.target.hash);
    assert_ne!(context.target.identity, other_context.target.identity);
    let mut wrong_identity = first.context();
    wrong_identity.target.identity = other_context.target.identity;
    assert!(Composition::new(
        &wrong_identity,
        &first.attempt,
        &first.source,
        &first.target,
        context.intent
    )
    .is_err());
    assert!(Composition::new(
        &context,
        &second.attempt,
        &second.source,
        &second.target,
        context.intent
    )
    .is_err());
    let first_composition = Composition::new(
        &context,
        &first.attempt,
        &first.source,
        &first.target,
        context.intent,
    )
    .unwrap();
    let other_composition = Composition::new(
        &other_context,
        &second.attempt,
        &second.source,
        &second.target,
        other_context.intent,
    )
    .unwrap();
    let Captured::Failure(other_failure) = other_composition.acquisition.capture(|_| Ok(())) else {
        panic!("expected native collision");
    };
    let other_stored = other_failure.store(false).unwrap();
    let verifier = Verifier {
        composition: &first_composition,
        used: Cell::new(false),
    };
    assert!(verifier
        .verify(&context, &other_stored, || panic!("foreign evidence"))
        .is_err());
    drop(other_stored);
    cleanup(first);
    cleanup(second);
}
