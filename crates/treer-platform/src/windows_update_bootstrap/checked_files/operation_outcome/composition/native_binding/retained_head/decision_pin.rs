//! Partial native composition, never production authority.
use super::*;

mod post_create;
mod preexisting;

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
fn native_operation_decision_pin_sink_lifetime_and_single_use() {
    struct DropCount(std::rc::Rc<Cell<usize>>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    for operation in [Operation::MoveOld, Operation::Publish] {
        for scenario in 0..3 {
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
            let evidence = retained
                .binding
                .with_failure(&failure)
                .unwrap()
                .encode()
                .unwrap();
            assert_eq!(stored.pin.read().unwrap(), evidence);
            let head_file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(fixture.attempt.path.join("receipt.json"))
                .unwrap();
            let (volume, id) = file_id(&head_file).unwrap();
            drop(head_file);
            // Independent wire expectation; never parsed from consume.intent.
            let mut expected = b"TRCONS01".to_vec();
            expected.extend(177_u16.to_le_bytes());
            expected.extend([0x6a; 32]);
            expected.extend(independent.0.attempt);
            expected.extend(volume.to_le_bytes());
            expected.extend(id);
            expected.extend(Sha256::digest(&head_bytes));
            expected.extend(Sha256::digest(&evidence));
            expected.extend([
                independent.0.component,
                independent.0.role,
                independent.0.operation,
            ]);
            expected.extend(independent.0.revision.to_le_bytes());
            expected.extend(failure.status.to_le_bytes());
            assert_eq!(expected.len(), 177);
            let path = fixture.attempt.path.join("consume.intent");
            let renamed = fixture.attempt.path.join("sink.rename");
            let observed = std::rc::Rc::new(Cell::new(false));
            let drops = std::rc::Rc::new(Cell::new(0));
            let dac = std::rc::Rc::new(Cell::new(u32::MAX));
            let observed_sink = observed.clone();
            let drops_sink = drops.clone();
            let dac_sink = dac.clone();
            let sink_path = path.clone();
            let sink_renamed = renamed.clone();
            let mut sink = Sink {
                refuse: scenario == 1,
                on_accept: Some(Box::new(move |_| {
                    assert_eq!(drops_sink.get(), 0);
                    assert_eq!(
                        open_result(&sink_path, GENERIC_WRITE),
                        ERROR_SHARING_VIOLATION
                    );
                    assert_eq!(open_result(&sink_path, DELETE), ERROR_SHARING_VIOLATION);
                    let rename = std::fs::rename(&sink_path, &sink_renamed).unwrap_err();
                    assert_eq!(rename.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
                    // Sharing is not an ACL-mutation barrier. Record this
                    // access probe without changing the DACL or claiming denial.
                    dac_sink.set(open_result(&sink_path, WRITE_DAC));
                    observed_sink.set(true);
                    if scenario == 2 {
                        panic!("injected decision sink panic");
                    }
                })),
                ..Sink::default()
            };
            let hook = Cell::new(false);
            let decision_id = Cell::new(None);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stored.consume_with_guard(&retained.binding, &mut sink, |matched| {
                    let decision = retained.decision_data(&stored, matched)?;
                    let bytes = decision.encode().ok_or_else(denied)?;
                    assert_eq!(bytes, expected);
                    assert!(!path.exists());
                    let pin = ownership.pin_decision(&bytes)?;
                    pin.validate()?;
                    retained.check(&composition)?;
                    let reader = OpenOptions::new()
                        .read(true)
                        .share_mode(FILE_SHARE_READ)
                        .open(&path)?;
                    decision_id.set(Some(file_id(&reader)?));
                    assert_eq!(std::fs::read(&path)?, expected);
                    hook.set(true);
                    Ok((pin, DropCount(drops.clone())))
                })
            }));
            assert!(hook.get());
            match scenario {
                0 => result.unwrap().unwrap(),
                1 => assert!(result.unwrap().is_err()),
                _ => assert!(result.is_err()),
            }
            assert_eq!(observed.get(), scenario != 1);
            assert_eq!(sink.accepted, usize::from(scenario != 1));
            assert_eq!(drops.get(), 1);
            assert!(stored.spent.get());
            assert_eq!(open_result(&path, GENERIC_WRITE), 0);
            assert_eq!(open_result(&path, DELETE), 0);
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            let reader = OpenOptions::new().read(true).open(&path).unwrap();
            assert_eq!(Some(file_id(&reader).unwrap()), decision_id.get());
            drop(reader);
            assert!(!renamed.exists());
            assert!(stored
                .consume(&retained.binding, &mut sink, || panic!("spent"))
                .is_err());
            assert!(ownership.pin_decision(&expected).is_err());
            assert_eq!(stored.pin.read().unwrap(), evidence);
            assert_eq!(std::fs::read(&path).unwrap(), expected);
            eprintln!(
                "decision_pin_fixture row={} operation={} injected={} write_open={} delete_open={} rename={} write_dac={} post_write=0 post_delete=0 sink={} drops=1 spent=1 decision=complete acl=unchanged",
                ["accept", "refuse", "panic"][scenario],
                if operation == Operation::MoveOld { "MoveOld" } else { "Publish" },
                u8::from(scenario != 0),
                if observed.get() { "32" } else { "na" },
                if observed.get() { "32" } else { "na" },
                if observed.get() { "32" } else { "na" },
                if observed.get() { dac.get().to_string() } else { "na".into() },
                sink.accepted
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
}
