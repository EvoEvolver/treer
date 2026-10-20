//! Plain fixture data only: no decision publication or continuation authority.
use super::*;
use crate::windows_update_bootstrap::checked_files::consume_record;

// Deliberately distinct from any real supervisor lifetime or restart claim.
const SYNTHETIC_LIFETIME: [u8; 32] = [0x6a; 32];

impl Retained<'_> {
    fn decision_data(
        &self,
        stored: &BoundStored<'_>,
        matched_digest: [u8; 32],
    ) -> io::Result<consume_record::Record> {
        self.check(stored.composition)?;
        let ((head_volume, head_file), head_hash) = self
            .head
            .snapshot_for(stored.composition.acquisition.attempt, &self.expected)?;
        let context = self.binding.with_failure(stored.failure)?;
        Ok(consume_record::Record {
            lifetime: SYNTHETIC_LIFETIME,
            attempt: context.attempt,
            head_volume,
            head_file,
            head_hash,
            evidence_hash: matched_digest,
            component: context.component,
            role: context.role,
            operation: context.operation,
            revision: context.revision,
            status: context.status,
        })
    }
}

#[test]
fn native_operation_retained_decision_data_and_field_refusals() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        // Each mismatch gets a fresh actual failure and single-use evidence.
        for field in 0..12 {
            let fixture = Fixture::new(operation);
            let ownership = FixtureLock::acquire(&fixture.attempt, true).unwrap();
            let independent = Binding::before_acquisition(&fixture, 1).unwrap();
            let head_bytes = expected_head(&independent);
            let head = ownership.publish_head(&head_bytes).unwrap();
            let retained = Retained::before_acquisition(&fixture, &head).unwrap();
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
            // Independent expected wire assembly: no parsed decision readback.
            let head_file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(fixture.attempt.path.join("receipt.json"))
                .unwrap();
            let (volume, id) = file_id(&head_file).unwrap();
            drop(head_file);
            let expected_context = retained.binding.with_failure(&failure).unwrap();
            let digest: [u8; 32] = Sha256::digest(expected_context.encode().unwrap()).into();
            let mut wire = b"TRCONS01".to_vec();
            wire.extend(177_u16.to_le_bytes());
            wire.extend(SYNTHETIC_LIFETIME);
            wire.extend(independent.0.attempt);
            wire.extend(volume.to_le_bytes());
            wire.extend(id);
            wire.extend(Sha256::digest(&head_bytes));
            wire.extend(digest);
            wire.extend([
                independent.0.component,
                independent.0.role,
                independent.0.operation,
            ]);
            wire.extend(independent.0.revision.to_le_bytes());
            wire.extend(failure.status.to_le_bytes());
            assert_eq!(wire.len(), 177);
            assert!(consume_record::valid_bytes(&wire));
            let original = wire.clone();
            if field > 0 {
                // Lifetime, attempt, full head identity, both digests, selectors,
                // revision and actual status are all compared independently.
                let offset = [10, 42, 74, 82, 98, 130, 162, 163, 164, 165, 173][field - 1];
                wire[offset] ^= if offset == 162 { 3 } else { 1 };
            }
            let stored = BoundStored::store(&composition, &retained.binding, &failure).unwrap();
            let mut sink = Sink::default();
            let hook_ran = Cell::new(false);
            let result = stored.consume_matched_digest(&retained.binding, &mut sink, |matched| {
                hook_ran.set(true);
                assert!(stored.spent.get());
                assert_eq!(matched, digest);
                let decision = retained.decision_data(&stored, matched)?;
                assert_eq!(decision.encode().unwrap(), original);
                if !consume_record::matches_expected(&wire, &decision) {
                    return Err(denied());
                }
                Ok(())
            });
            assert!(hook_ran.get());
            assert_eq!(result.is_ok(), field == 0);
            assert_eq!(sink.accepted, usize::from(field == 0));
            assert!(stored
                .consume_matched_digest(&retained.binding, &mut sink, |_| panic!("spent"))
                .is_err());
            assert_eq!(sink.accepted, usize::from(field == 0));
            assert!(!fixture.attempt.path.join("consume.intent").exists());
            assert_eq!(
                stored.pin.read().unwrap(),
                expected_context.encode().unwrap()
            );
            eprintln!(
                "decision_data_fixture operation={} field={} matched={} spent=1 decision=0 sink={}",
                if operation == Operation::MoveOld {
                    "MoveOld"
                } else {
                    "Publish"
                },
                field,
                u8::from(field == 0),
                sink.accepted
            );
            drop(stored);
            drop(retained);
            drop(head);
            drop(ownership);
            std::fs::remove_file(fixture.attempt.path.join("receipt.json")).unwrap();
            std::fs::remove_file(fixture.attempt.path.join("fixture.lock")).unwrap();
            cleanup(fixture);
        }
    }
}
