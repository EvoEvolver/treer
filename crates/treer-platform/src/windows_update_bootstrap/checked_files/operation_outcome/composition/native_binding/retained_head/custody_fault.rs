//! Actual private-fixture ACL custody loss, before/after evidence read.
use super::*;

struct AclFault {
    file: File,
    descriptor: SecurityAllocation,
    acl: *mut ACL,
}

impl AclFault {
    fn prepare(path: &Path) -> io::Result<Self> {
        // WRITE_DAC changes security metadata, not file data. It does not
        // bypass the pin's exclusion of data writers or delete opens.
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                READ_CONTROL | WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(raw) };
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        protected_private_acl(&file, &owner)?;
        let mut acl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut acl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        let descriptor = SecurityAllocation(descriptor);
        if result != ERROR_SUCCESS || acl.is_null() {
            return Err(denied());
        }
        Ok(Self {
            file,
            descriptor,
            acl,
        })
    }

    fn protection(&self, protected: bool) -> io::Result<()> {
        // Reapply the original non-null owner/SYSTEM ACL, never a null/world
        // DACL. Clearing protection may transiently inherit parent ACEs;
        // this fault is confined to the synthetic private fixture object.
        let _keep_descriptor_alive = &self.descriptor;
        let result = unsafe {
            SetSecurityInfo(
                self.file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION
                    | if protected {
                        PROTECTED_DACL_SECURITY_INFORMATION
                    } else {
                        UNPROTECTED_DACL_SECURITY_INFORMATION
                    },
                ptr::null_mut(),
                ptr::null_mut(),
                self.acl,
                ptr::null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let mut actual = ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                self.file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut actual,
            )
        };
        let _allocation = SecurityAllocation(actual);
        if result != ERROR_SUCCESS {
            return Err(denied());
        }
        let mut control = 0;
        let mut revision = 0;
        check(unsafe { GetSecurityDescriptorControl(actual, &mut control, &mut revision) })?;
        if (control & SE_DACL_PROTECTED != 0) != protected {
            return Err(denied());
        }
        Ok(())
    }
}

impl Drop for AclFault {
    fn drop(&mut self) {
        let _ = self.protection(true);
    }
}

#[test]
fn native_operation_decision_pin_refuses_post_transition_head_acl_loss() {
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
        let head_path = fixture.attempt.path.join("receipt.json");
        let read_identity = |path: &Path| {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .unwrap();
            file_id(&file).unwrap()
        };
        let original_head_id = read_identity(&head_path);
        let fault = AclFault::prepare(&head_path).unwrap();
        let path = fixture.attempt.path.join("consume.intent");
        let injected = Cell::new(false);
        let decision_id = Cell::new(None);
        let mut expected = None;
        let mut sink = Sink {
            on_accept: Some(Box::new(|_| panic!("head ACL fault reached sink"))),
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
            decision_id.set(Some(read_identity(&path)));
            assert_eq!(std::fs::read(&path)?, bytes);
            expected = Some(bytes);
            fault.protection(false)?;
            injected.set(true);
            // The operation binding still passes; retained-head custody alone
            // must refuse after the decision transitioned successfully.
            retained.binding.check(&composition)?;
            retained.check(&composition)?;
            Ok(pin)
        });
        assert!(injected.get());
        assert!(result.is_err());
        assert_eq!(sink.accepted, 0);
        assert!(stored.spent.get());
        fault.protection(true).unwrap();
        retained.check(&composition).unwrap();
        assert_eq!(read_identity(&head_path), original_head_id);
        assert_eq!(std::fs::read(&head_path).unwrap(), head_bytes);
        let expected = expected.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(Some(read_identity(&path)), decision_id.get());
        drop(OpenOptions::new().write(true).open(&path).unwrap());
        assert_eq!(stored.pin.read().unwrap(), evidence);
        assert!(stored
            .consume(&retained.binding, &mut sink, || panic!(
                "spent retry entered hook"
            ))
            .is_err());
        assert!(ownership.pin_decision(&expected).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        eprintln!("decision_head_acl_fixture operation={} injected=1 pin_validated=1 binding=ok retained=refused restored=1 sink=0 spent=1 bytes={} head_identity=unchanged decision_identity=unchanged post_write=0 retry=refused", if operation == Operation::MoveOld { "MoveOld" } else { "Publish" }, expected.len());
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

#[test]
fn native_operation_guard_reread_acl_failure_spends_and_drops_once() {
    struct Guard<'a>(&'a Cell<usize>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
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
        let stored = BoundStored::store(&composition, &binding, &failure).unwrap();
        let original = stored.pin.read().unwrap();
        let fault = AclFault::prepare(&fixture.attempt.path.join("reason.evidence")).unwrap();
        let drops = Cell::new(0);
        let injected = Cell::new(false);
        let mut sink = Sink {
            on_accept: Some(Box::new(|_| panic!("custody fault must precede sink"))),
            ..Sink::default()
        };
        assert!(stored
            .consume_with_guard(&binding, &mut sink, |_| {
                assert!(stored.spent.get());
                let guard = Guard(&drops);
                fault.protection(false)?;
                assert_eq!(drops.get(), 0);
                injected.set(true);
                Ok(guard)
            })
            .is_err());
        assert!(injected.get());
        assert_eq!(drops.get(), 1);
        assert!(stored.spent.get());
        assert_eq!(sink.accepted, 0);
        // The unchanged operation binding passes; the reread actually reached
        // evidence custody validation and refused before ReadFile, not at the
        // post-hook binding check. This is NOT a native ReadFile error test.
        binding.check(&composition).unwrap();
        let trace = stored.pin.read_trace();
        assert!(!trace.validated);
        assert_eq!(trace.read_calls, 0);
        fault.protection(true).unwrap();
        assert_eq!(stored.pin.read().unwrap(), original);
        assert!(stored
            .consume_with_guard::<Guard<'_>>(&binding, &mut sink, |_| panic!("spent"))
            .is_err());
        assert_eq!(drops.get(), 1);
        assert!(!fixture.attempt.path.join("consume.intent").exists());
        eprintln!("guard_reread_acl_fault operation={} injected=1 validated=0 reads=0 drops=1 spent=1 decision=0 sink=0", if operation == Operation::MoveOld { "MoveOld" } else { "Publish" });
        drop(fault);
        drop(stored);
        cleanup(fixture);
    }
}

#[test]
fn native_operation_retained_head_acl_loss_spends_consumption() {
    for operation in [Operation::MoveOld, Operation::Publish] {
        for after_read in [false, true] {
            let fixture = Fixture::new(operation);
            let ownership = FixtureLock::acquire(&fixture.attempt, true).unwrap();
            let independent = Binding::before_acquisition(&fixture, 1).unwrap();
            let head = ownership
                .publish_head(&expected_head(&independent))
                .unwrap();
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
            let fault = AclFault::prepare(&fixture.attempt.path.join("receipt.json")).unwrap();
            let mut sink = Sink::default();
            let hook_ran = Cell::new(false);
            if after_read {
                assert!(retained
                    .consume_after_read(&stored, &mut sink, || {
                        hook_ran.set(true);
                        fault.protection(false)?;
                        Ok(())
                    })
                    .is_err());
            } else {
                fault.protection(false).unwrap();
                assert!(retained
                    .consume_after_read(&stored, &mut sink, || {
                        hook_ran.set(true);
                        Ok(())
                    })
                    .is_err());
            }
            assert_eq!(sink.accepted, 0);
            assert!(stored.spent.get());
            assert_eq!(hook_ran.get(), after_read);
            assert!(head
                .digest_for(&fixture.attempt, &retained.expected)
                .is_err());
            fault.protection(true).unwrap();
            retained.check(&composition).unwrap();
            // Restoring native custody cannot reset the spent decision.
            assert!(retained.consume(&stored, &mut sink).is_err());
            assert_eq!(sink.accepted, 0);
            assert_eq!(stored.pin.read().unwrap(), evidence);
            eprintln!(
                "head_custody_fixture stage={} spent=1 sink=0 restored=1",
                if after_read {
                    "AfterRead"
                } else {
                    "BeforeRead"
                }
            );
            drop(fault);
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
