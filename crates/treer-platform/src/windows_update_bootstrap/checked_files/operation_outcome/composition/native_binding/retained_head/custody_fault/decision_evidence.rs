//! Post-decision evidence custody loss, not a native second-ReadFile error.
use super::*;

fn open_result(path: &Path, access: u32) -> u32 {
    let raw = unsafe {
        CreateFileW(
            wide(path.as_os_str()).as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return io::Error::last_os_error().raw_os_error().unwrap() as u32;
    }
    drop(unsafe { File::from_raw_handle(raw) });
    0
}

#[test]
fn native_operation_decision_pin_refuses_post_transition_evidence_acl_loss() {
    struct DropCount<'a>(&'a Cell<usize>);
    impl Drop for DropCount<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
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
        let evidence_path = fixture.attempt.path.join("reason.evidence");
        let head_path = fixture.attempt.path.join("receipt.json");
        let path = fixture.attempt.path.join("consume.intent");
        let identity = |path: &Path| {
            let reader = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .unwrap();
            file_id(&reader).unwrap()
        };
        let original_evidence_id = identity(&evidence_path);
        let original_head_id = identity(&head_path);
        let fault = AclFault::prepare(&evidence_path).unwrap();
        let drops = Cell::new(0);
        let returned_guard = Cell::new(false);
        let decision_id = Cell::new(None);
        let mut expected = None;
        let mut sink = Sink {
            on_accept: Some(Box::new(|_| panic!("evidence ACL loss reached sink"))),
            ..Sink::default()
        };
        let result = stored.consume_with_guard(&retained.binding, &mut sink, |matched| {
            let bytes = retained
                .decision_data(&stored, matched)?
                .encode()
                .ok_or_else(denied)?;
            assert_eq!(bytes.len(), 177);
            let pin = ownership.pin_decision(&bytes)?;
            pin.validate()?;
            retained.check(&composition)?;
            decision_id.set(Some(identity(&path)));
            assert_eq!(std::fs::read(&path)?, bytes);
            assert_eq!(open_result(&path, GENERIC_WRITE), ERROR_SHARING_VIOLATION);
            assert_eq!(open_result(&path, DELETE), ERROR_SHARING_VIOLATION);
            expected = Some(bytes);
            let guard = (pin, DropCount(&drops));
            fault.protection(false)?;
            assert_eq!(drops.get(), 0);
            returned_guard.set(true);
            Ok(guard)
        });
        assert!(
            returned_guard.get(),
            "validated decision guard must reach reread"
        );
        assert!(result.is_err());
        assert_eq!(drops.get(), 1);
        assert_eq!(sink.accepted, 0);
        assert!(stored.spent.get());
        // The hook returned a real decision pin and both other checks pass.
        // The second evidence read reached custody validation and refused
        // BEFORE ReadFile. Capture this trace before any successful reread.
        retained.binding.check(&composition).unwrap();
        retained.check(&composition).unwrap();
        let trace = stored.pin.read_trace();
        assert!(!trace.validated);
        assert_eq!(trace.read_calls, 0);
        fault.protection(true).unwrap();
        assert_eq!(stored.pin.read().unwrap(), evidence);
        assert_eq!(identity(&evidence_path), original_evidence_id);
        assert_eq!(std::fs::read(&head_path).unwrap(), head_bytes);
        assert_eq!(identity(&head_path), original_head_id);
        let expected = expected.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(Some(identity(&path)), decision_id.get());
        assert_eq!(open_result(&path, GENERIC_WRITE), 0);
        assert_eq!(open_result(&path, DELETE), 0);
        assert!(stored
            .consume(&retained.binding, &mut sink, || panic!(
                "spent retry entered hook"
            ))
            .is_err());
        assert_eq!(drops.get(), 1);
        let recreate = ownership
            .pin_decision(&expected)
            .err()
            .expect("occupied decision slot");
        assert_eq!(recreate.raw_os_error(), Some(ERROR_FILE_EXISTS as i32));
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(Some(identity(&path)), decision_id.get());
        eprintln!("decision_evidence_acl_fixture operation={} injected=1 pin_validated=1 binding=ok retained=ok validated={} reads={} drops={} sink=0 spent=1 bytes={} head_identity=unchanged evidence_identity=unchanged decision_identity=unchanged post_write=0 post_delete=0 retry=refused recreate_win32={}", if operation == Operation::MoveOld { "MoveOld" } else { "Publish" }, u8::from(trace.validated), trace.read_calls, drops.get(), expected.len(), recreate.raw_os_error().unwrap());
        drop(sink);
        drop(fault);
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
