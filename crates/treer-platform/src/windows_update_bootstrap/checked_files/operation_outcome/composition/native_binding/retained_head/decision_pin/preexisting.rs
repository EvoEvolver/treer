//! A closed complete decision is burned, even for a fresh matching context.
use super::*;

#[test]
fn native_operation_decision_pin_refuses_preexisting_complete() {
    for operation in [Operation::MoveOld, Operation::Publish] {
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
        let stored = BoundStored::store(&composition, &retained.binding, &failure).unwrap();
        let evidence = stored.pin.read().unwrap();
        // Deliberately independent of this context: valid bytes at an occupied
        // slot never become authority, even if a later decision would differ.
        let seeded =
            crate::windows_update_bootstrap::checked_files::consume_record::fixture_bytes();
        assert_eq!(seeded.len(), 177);
        let seed = ownership.pin_decision(&seeded).unwrap();
        seed.validate().unwrap();
        drop(seed);
        let path = fixture.attempt.path.join("consume.intent");
        let original = OpenOptions::new().read(true).open(&path).unwrap();
        let identity = file_id(&original).unwrap();
        drop(original);
        assert_eq!(std::fs::read(&path).unwrap(), seeded);
        let hook = Cell::new(false);
        let mut sink = Sink {
            on_accept: Some(Box::new(|_| panic!("occupied decision reached sink"))),
            ..Sink::default()
        };
        let result = stored.consume_with_guard(&retained.binding, &mut sink, |matched| {
            let bytes = retained
                .decision_data(&stored, matched)?
                .encode()
                .ok_or_else(denied)?;
            assert_ne!(
                bytes, seeded,
                "occupied record is independent of this context"
            );
            hook.set(true);
            ownership.pin_decision(&bytes)
        });
        assert!(hook.get(), "must reach exclusive creation boundary");
        let error = result.expect_err("occupied decision must refuse creation");
        let win32 = error.raw_os_error().unwrap();
        assert_eq!(win32, ERROR_FILE_EXISTS as i32);
        assert_eq!(sink.accepted, 0);
        assert!(stored.spent.get());
        assert!(stored
            .consume(&retained.binding, &mut sink, || panic!(
                "spent retry entered hook"
            ))
            .is_err());
        assert!(ownership.pin_decision(&seeded).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), seeded);
        let preserved = OpenOptions::new().read(true).open(&path).unwrap();
        assert_eq!(file_id(&preserved).unwrap(), identity);
        drop(preserved);
        assert_eq!(stored.pin.read().unwrap(), evidence);
        assert_eq!(open_result(&path, GENERIC_WRITE), 0);
        assert_eq!(open_result(&path, DELETE), 0);
        eprintln!(
            "decision_existing_fixture row=complete operation={} create_win32={} hook=1 sink=0 spent=1 bytes={} would_differ=1 identity=unchanged retry=refused",
            if operation == Operation::MoveOld { "MoveOld" } else { "Publish" },
            win32,
            seeded.len()
        );
        drop(sink);
        drop(stored);
        drop(retained);
        drop(head);
        drop(ownership);
        for name in ["consume.intent", "receipt.json", "fixture.lock"] {
            std::fs::remove_file(fixture.attempt.path.join(name)).unwrap();
        }
        cleanup(fixture);
    }
}
