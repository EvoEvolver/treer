//! Partial inert composition: no recovered or production reason authority.
use super::*;
use crate::windows_update_bootstrap::checked_files::{
    head_record,
    ownership::{head_pin::HeadPin, FixtureLock},
};

mod custody_fault;
mod decision_data;
mod decision_pin;

fn different_file_id(original: [u8; 16], source: [u8; 16]) -> [u8; 16] {
    // Three valid candidates cannot all equal the two excluded identities.
    [[1; 16], [2; 16], [3; 16]]
        .into_iter()
        .find(|candidate| *candidate != original && *candidate != source)
        .unwrap()
}

fn expected_head(binding: &Binding) -> Vec<u8> {
    let image = |value: &Image| head_record::Image {
        volume: value.volume,
        file: value.file,
        size: value.size,
        hash: value.hash,
    };
    head_record::Head {
        attempt: binding.0.attempt,
        component: binding.0.component,
        role: binding.0.role,
        operation: binding.0.operation,
        revision: binding.0.revision,
        source: image(&binding.0.source),
        target: image(&binding.0.target),
    }
    .encode()
    .unwrap()
}

struct Retained<'a> {
    head: &'a HeadPin<'a>,
    expected: Vec<u8>,
    binding: Binding,
}

impl<'a> Retained<'a> {
    fn before_acquisition(fixture: &Fixture, head: &'a HeadPin<'a>) -> io::Result<Self> {
        let mut binding = Binding::before_acquisition(fixture, 1)?;
        let expected = expected_head(&binding);
        binding.0.predecessor = head.digest_for(&fixture.attempt, &expected)?;
        Ok(Self {
            head,
            expected,
            binding,
        })
    }

    fn check(&self, composition: &Composition<'_>) -> io::Result<()> {
        self.binding.check(composition)?;
        if self
            .head
            .digest_for(composition.acquisition.attempt, &self.expected)?
            != self.binding.0.predecessor
        {
            return Err(denied());
        }
        Ok(())
    }

    fn consume(&self, stored: &BoundStored<'_>, sink: &mut Sink) -> io::Result<()> {
        self.consume_after_read(stored, sink, || Ok(()))
    }

    fn consume_after_read(
        &self,
        stored: &BoundStored<'_>,
        sink: &mut Sink,
        after_read: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        // A failed precheck spends the underlying evidence as well. The same
        // in-memory fixture cannot retry after any head uncertainty.
        if let Err(error) = self.check(stored.composition) {
            stored.spent.set(true);
            return Err(error);
        }
        stored.consume(&self.binding, sink, || {
            after_read()?;
            self.check(stored.composition)
        })
    }
}

#[test]
fn native_operation_retained_head_digest_and_single_use() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        let fixture = Fixture::new(operation);
        let ownership = FixtureLock::acquire(&fixture.attempt, true).unwrap();
        let independent = Binding::before_acquisition(&fixture, 1).unwrap();
        let expected = expected_head(&independent);
        let head = ownership.publish_head(&expected).unwrap();
        let retained = Retained::before_acquisition(&fixture, &head).unwrap();
        assert_eq!(
            retained.binding.0.predecessor,
            <[u8; 32]>::from(Sha256::digest(&expected))
        );
        let context = fixture.context();
        let composition = Composition::new(
            &context,
            &fixture.attempt,
            &fixture.source,
            &fixture.target,
            context.intent,
        )
        .unwrap();
        retained.check(&composition).unwrap();
        let Captured::Failure(failure) = composition.acquisition.capture(|_| Ok(())) else {
            panic!("native collision required");
        };
        let stored = BoundStored::store(&composition, &retained.binding, &failure).unwrap();
        let mut sink = Sink::default();
        retained.consume(&stored, &mut sink).unwrap();
        assert_eq!(sink.accepted, 1);
        assert!(retained.consume(&stored, &mut sink).is_err());
        assert_eq!(sink.accepted, 1);
        drop(stored);
        drop(retained);
        drop(head);
        drop(ownership);
        std::fs::remove_file(fixture.attempt.path.join("receipt.json")).unwrap();
        std::fs::remove_file(fixture.attempt.path.join("fixture.lock")).unwrap();
        cleanup(fixture);
    }
}

#[test]
fn native_operation_retained_head_refuses_foreign_attempt_and_context() {
    let fixture = Fixture::new(Operation::MoveOld);
    let foreign = Fixture::new(Operation::MoveOld);
    let ownership = FixtureLock::acquire(&fixture.attempt, true).unwrap();
    let independent = Binding::before_acquisition(&fixture, 1).unwrap();
    let bytes = expected_head(&independent);
    let head = ownership.publish_head(&bytes).unwrap();
    assert!(Retained::before_acquisition(&foreign, &head).is_err());
    // Each otherwise valid field change is a different independent head.
    for field in 0..4 {
        let mut changed = independent.clone();
        match field {
            0 => changed.0.revision += 1,
            1 => changed.0.component = 2,
            2 => changed.0.source.hash[0] ^= 1,
            _ => {
                changed.0.target.file =
                    different_file_id(independent.0.target.file, independent.0.source.file);
            }
        }
        assert!(head
            .digest_for(&fixture.attempt, &expected_head(&changed))
            .is_err());
    }
    assert!(!fixture.attempt.path.join("reason.evidence").exists());
    drop(head);
    drop(ownership);
    std::fs::remove_file(fixture.attempt.path.join("receipt.json")).unwrap();
    std::fs::remove_file(fixture.attempt.path.join("fixture.lock")).unwrap();
    cleanup(fixture);
    cleanup(foreign);
}

#[test]
fn native_operation_retained_head_file_id_mutation_stays_encodable() {
    // Deterministic counterexample: the old single-bit mutation makes two
    // different, valid IDs equal. No assumption about the NTFS allocator.
    let mut source = [0; 16];
    source[0] = 1;
    source[6] = 42;
    let mut target = source;
    target[0] ^= 1;
    let image = |file| head_record::Image {
        volume: 9,
        file,
        size: 17,
        hash: [4; 32],
    };
    let mut head = head_record::Head {
        attempt: [0x11; 32],
        component: 1,
        role: 1,
        operation: 1,
        revision: 7,
        source: image(source),
        target: image(target),
    };
    assert!(head.encode().is_some());
    head.target.file[0] ^= 1;
    assert_eq!(head.target.file, head.source.file);
    assert!(head.encode().is_none());
    for (original, source) in [(target, source), ([1; 16], [2; 16]), ([2; 16], [3; 16])] {
        let changed = different_file_id(original, source);
        assert_ne!(changed, original);
        assert_ne!(changed, source);
        assert_ne!(changed, [0; 16]);
        assert_ne!(changed, [u8::MAX; 16]);
        head.source.file = source;
        head.target.file = changed;
        assert!(head.encode().is_some());
    }
    eprintln!("head_mismatch_fixture old_mutation_collision=1 old_encode_refused=1 distinct_candidate_valid=1");
}
