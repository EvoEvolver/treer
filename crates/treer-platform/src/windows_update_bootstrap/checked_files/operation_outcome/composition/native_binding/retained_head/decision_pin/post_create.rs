//! Injected refusal after successful creation/read-back, not an OS I/O error.
use super::*;

#[test]
fn native_operation_decision_pin_preserves_post_create_refusal() {
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
        let path = fixture.attempt.path.join("consume.intent");
        let injected = Cell::new(false);
        let identity = Cell::new(None);
        let mut expected = None;
        let mut sink = Sink {
            on_accept: Some(Box::new(|_| panic!("post-create refusal reached sink"))),
            ..Sink::default()
        };
        let result = stored.consume_with_guard(&retained.binding, &mut sink, |matched| {
            let bytes = retained
                .decision_data(&stored, matched)?
                .encode()
                .ok_or_else(denied)?;
            assert_eq!(bytes.len(), 177);
            assert!(!path.exists());
            let pin = ownership.pin_decision(&bytes)?;
            pin.validate()?;
            retained.check(&composition)?;
            let reader = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)?;
            identity.set(Some(file_id(&reader)?));
            assert_eq!(std::fs::read(&path)?, bytes);
            // Establish a live restrictive pin before the labelled refusal.
            assert_eq!(open_result(&path, GENERIC_WRITE), ERROR_SHARING_VIOLATION);
            assert_eq!(open_result(&path, DELETE), ERROR_SHARING_VIOLATION);
            expected = Some(bytes);
            injected.set(true);
            Err::<(), _>(denied())
        });
        assert!(injected.get());
        assert!(result.is_err());
        assert_eq!(sink.accepted, 0);
        assert!(stored.spent.get());
        assert!(stored
            .consume(&retained.binding, &mut sink, || panic!(
                "spent retry entered hook"
            ))
            .is_err());
        let expected = expected.unwrap();
        assert!(ownership.pin_decision(&expected).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let reader = OpenOptions::new().read(true).open(&path).unwrap();
        assert_eq!(Some(file_id(&reader).unwrap()), identity.get());
        drop(reader);
        assert_eq!(stored.pin.read().unwrap(), evidence);
        assert_eq!(open_result(&path, GENERIC_WRITE), 0);
        assert_eq!(open_result(&path, DELETE), 0);
        eprintln!(
            "decision_post_create_fixture operation={} injected=1 validated=1 live_write=32 live_delete=32 post_write=0 post_delete=0 sink=0 spent=1 bytes={} identity=unchanged retry=refused",
            if operation == Operation::MoveOld { "MoveOld" } else { "Publish" },
            expected.len()
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
