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
        // Reapply the original non-null owner/SYSTEM ACL. Never install a
        // null/world ACL; the negative condition is the protection flag only.
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
