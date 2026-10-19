//! Disposable live-image fixture only. Run the ignored parent in a hosted
//! Windows process outside the runner Job, never on an enrolled machine.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
};

const CHILD_TEST: &str = "windows_update_bootstrap::running_image::running_image_child";

// ReOpenFile binds to the retained object rather than resolving a caller path.
// A read-only pin remains alive throughout execution and both renames, denying
// write sharing but permitting DELETE access on a separate operation handle.
// The original write-open sharing record may outlive DuplicateHandle's access
// reduction; CreateProcess refused that pin on hosted Server 2022/2025. This is
// confined to the live-image fixture;
// checked_files::copy/copy_for_rename retain their original sharing contract.
fn reopen(image: &File, access: u32, sharing: u32, stage: &'static str) -> io::Result<File> {
    let raw = unsafe {
        ReOpenFile(
            image.as_raw_handle(),
            access,
            sharing,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        let code = unsafe { GetLastError() };
        eprintln!("running_image_fixture stage={stage} access={access:#x} sharing={sharing:#x} win32={code}");
        return Err(denied());
    }
    let reopened = unsafe { File::from_raw_handle(raw) };
    check(unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, 0) })?;
    if identity(&reopened)? != identity(image)? {
        return Err(denied());
    }
    Ok(reopened)
}

// Tighten ONLY this newly created private fixture image before closing the last
// write-open file object. ReOpenFile cannot request a no-write-sharing read pin
// while that write-open object exists. A verified protected RX/DELETE DACL
// denies new write opens across that transition, including the current user.
// Owner WRITE_DAC override / hostile same-user code is outside this fixture's
// trust claim. This is not a policy for an existing installed executable.
fn execution_pin(image: File, expected: [u8; 32]) -> io::Result<File> {
    let metadata = reopen(
        &image,
        READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        "acl_pin",
    )?;
    let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
    private_owner_dacl(&metadata, &owner)?;
    let access = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | DELETE;
    let text = wide(OsStr::new(&format!(
        "D:P(A;;{access:#x};;;SY)(A;;{access:#x};;;{owner})"
    )));
    let mut descriptor = ptr::null_mut();
    check(unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    })?;
    let _allocation = SecurityAllocation(descriptor);
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = ptr::null_mut();
    check(unsafe {
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
    })?;
    if present == 0 || acl.is_null() {
        return Err(denied());
    }
    if unsafe {
        SetSecurityInfo(
            metadata.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        )
    } != ERROR_SUCCESS
    {
        return Err(denied());
    }
    // Verify the exact installed ACL, not merely the owner/foreign-writer rule.
    let mut actual_acl = ptr::null_mut();
    let mut actual_descriptor = ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            metadata.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut actual_acl,
            ptr::null_mut(),
            &mut actual_descriptor,
        )
    };
    let _actual_allocation = SecurityAllocation(actual_descriptor);
    if result != ERROR_SUCCESS || actual_acl.is_null() || unsafe { IsValidAcl(actual_acl) } == 0 {
        return Err(denied());
    }
    let mut control = 0;
    let mut revision = 0;
    check(unsafe { GetSecurityDescriptorControl(actual_descriptor, &mut control, &mut revision) })?;
    if control & SE_DACL_PROTECTED == 0 || unsafe { (*actual_acl).AceCount } != 2 {
        return Err(denied());
    }
    for index in 0..2 {
        let mut desired = ptr::null_mut();
        let mut actual = ptr::null_mut();
        check(unsafe { GetAce(acl, index, &mut desired) })?;
        check(unsafe { GetAce(actual_acl, index, &mut actual) })?;
        let desired_size = unsafe { (*desired.cast::<ACE_HEADER>()).AceSize } as usize;
        let actual_size = unsafe { (*actual.cast::<ACE_HEADER>()).AceSize } as usize;
        if desired_size != actual_size
            || unsafe {
                std::slice::from_raw_parts(desired.cast::<u8>(), desired_size)
                    != std::slice::from_raw_parts(actual.cast::<u8>(), actual_size)
            }
        {
            return Err(denied());
        }
    }
    let path = final_path(&image)?;
    drop(image);
    // At this exact point only the metadata handle remains: it shares writes,
    // so this assertion specifically proves the tightened ACL denies writing.
    if OpenOptions::new().write(true).open(&path).is_ok() {
        return Err(denied());
    }
    let pin = reopen(
        &metadata,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        "read_pin",
    )?;
    private_owner_dacl(&pin, &owner)?;
    if digest(&pin)? != expected {
        return Err(denied());
    }
    Ok(pin)
}

fn rename_running(
    attempt: &checked_files::Attempt,
    image: &File,
    from: checked_files::Slot,
    to: checked_files::Slot,
    expected: [u8; 32],
) -> io::Result<()> {
    let operation = reopen(
        image,
        GENERIC_READ | DELETE,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        "rename_pin",
    )?;
    // The existing primitive still checks ACL, source slot, links, reparse
    // status and digest and verifies identity plus destination after the move.
    // The DELETE-access operation handle drops before the next liveness check.
    attempt.rename_checked(&operation, from, to, expected)
}

// Only the actual CreateProcess handle is used for observation and cleanup.
// The child runs one fixed test, creates no descendants, and also self-times out.
struct Child {
    process: OwnedHandle,
    created: u64,
    pid: u32,
    cleanup_attempted: bool,
}

impl Child {
    fn spawn(image: &File, expected: [u8; 32], endpoint: &Endpoint) -> io::Result<Self> {
        let path = final_path(image)?;
        let application = wide(path.as_os_str());
        let mut command = OsString::from("\"");
        command.push(&path);
        command.push(format!("\" --exact {CHILD_TEST} --ignored --skip "));
        command.push(&endpoint.selector);
        let mut command = wide(&command);
        let environment = [0u16; 2];
        let cwd = wide(path.parent().ok_or_else(denied)?.as_os_str());
        let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
        startup.cb = mem::size_of_val(&startup) as u32;
        let mut info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        let spawned = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                cwd.as_ptr(),
                &startup,
                &mut info,
            )
        };
        if spawned == 0 {
            let code = unsafe { GetLastError() };
            eprintln!("running_image_fixture stage=create_process win32={code}");
            return Err(denied());
        }
        let mut child = Self {
            process: unsafe { OwnedHandle::from_raw_handle(info.hProcess) },
            created: 0,
            pid: info.dwProcessId,
            cleanup_attempted: false,
        };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        // Any failure from this point drops and cleans the actual child, even
        // while suspended. No breakaway or access to installed processes.
        child.created = creation(child.process.as_raw_handle())?;
        child.alive()?;
        let mut in_job = 0;
        check(unsafe {
            IsProcessInJob(child.process.as_raw_handle(), ptr::null_mut(), &mut in_job)
        })?;
        if in_job != 0 {
            return Err(denied());
        }
        let mut buffer = vec![0; 32_768];
        let mut length = buffer.len() as u32;
        check(unsafe {
            QueryFullProcessImageNameW(
                child.process.as_raw_handle(),
                0,
                buffer.as_mut_ptr(),
                &mut length,
            )
        })?;
        let observed_path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
        // Same read-only/write-denying sharing contract as the execution pin.
        let observed = open_checked(&observed_path, false, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
        if identity(&observed)? != identity(image)? || digest(&observed)? != expected {
            return Err(denied());
        }
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(denied());
        }
        Ok(child)
    }

    fn alive(&self) -> io::Result<()> {
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } != WAIT_TIMEOUT
            || creation(self.process.as_raw_handle())? != self.created
        {
            return Err(denied());
        }
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleanup_attempted {
            return Err(denied());
        }
        self.cleanup_attempted = true;
        let handle = self.process.as_raw_handle();
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_TIMEOUT => check(unsafe { TerminateProcess(handle, 1) })?,
            WAIT_OBJECT_0 => return Ok(()),
            _ => return Err(denied()),
        }
        if unsafe { WaitForSingleObject(handle, CLEANUP_MS) } != WAIT_OBJECT_0 {
            return Err(denied());
        }
        Ok(())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "fixed child of native_running_image_rename only"]
async fn running_image_child() {
    let args: Vec<_> = std::env::args().collect();
    let at = args.iter().position(|arg| arg == "--skip").unwrap();
    let selector = &args[at + 1];
    assert_eq!(selector.len(), 32);
    assert!(selector.bytes().all(|byte| byte.is_ascii_hexdigit()));
    // One deadline covers all messages; a missing owner cannot leave this
    // fixture alive indefinitely. The child never launches another process.
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut pipe = ClientOptions::new()
            .open(format!(r"\\.\pipe\Treer-update-{selector}"))
            .unwrap();
        pipe.write_all(&[1]).await.unwrap();
        for (request, response) in [(2, 3), (4, 5)] {
            assert_eq!(pipe.read_u8().await.unwrap(), request);
            pipe.write_all(&[response]).await.unwrap();
        }
        // Keep executing until the owner explicitly terminates this handle.
        // Do not exit merely because the final liveness reply was buffered.
        let _ = pipe.read_u8().await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "disposable hosted Windows worker outside ambient Jobs; never win_native"]
async fn native_running_image_rename() {
    use checked_files::{Attempt, Slot};
    let mut in_job = 0;
    check(unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) }).unwrap();
    assert_eq!(in_job, 0, "native fixture must start outside ambient Jobs");
    let fixture = tests::PrivateImage::new();
    let name = "attempt-00000000000000000000000000000006";
    let path = fixture.directory.join(name);
    let attempt = Attempt::create(&fixture.directory, name).unwrap();
    let source = open_pinned(&fixture.path, false).unwrap();
    let expected = digest(&source).unwrap();
    let image = attempt.copy_for_rename(&source, expected).unwrap();
    attempt
        .rename_checked(&image, Slot::Candidate, Slot::Active, expected)
        .unwrap();
    let before = identity(&image).unwrap();
    let image = execution_pin(image, expected).unwrap();
    let mut endpoint = Endpoint::bind_random().unwrap();
    let mut child = Child::spawn(&image, expected, &endpoint).unwrap();
    tokio::time::timeout(CONNECT_BUDGET, async {
        endpoint.pipe.connect().await.unwrap();
        let mut peer = 0;
        check(unsafe { GetNamedPipeClientProcessId(endpoint.pipe.as_raw_handle(), &mut peer) })
            .unwrap();
        assert_eq!(peer, child.pid);
        assert_eq!(endpoint.pipe.read_u8().await.unwrap(), 1);
        child.alive().unwrap();
        rename_running(&attempt, &image, Slot::Active, Slot::Backup, expected).unwrap();
        assert_eq!(identity(&image).unwrap(), before);
        assert!(final_path(&image).unwrap().ends_with("controller.prev.exe"));
        endpoint.pipe.write_all(&[2]).await.unwrap();
        assert_eq!(endpoint.pipe.read_u8().await.unwrap(), 3);
        child.alive().unwrap();
        rename_running(&attempt, &image, Slot::Backup, Slot::Active, expected).unwrap();
        assert_eq!(identity(&image).unwrap(), before);
        assert!(final_path(&image).unwrap().ends_with("controller.exe"));
        assert!(OpenOptions::new()
            .write(true)
            .open(path.join("controller.exe"))
            .is_err());
        endpoint.pipe.write_all(&[4]).await.unwrap();
        assert_eq!(endpoint.pipe.read_u8().await.unwrap(), 5);
        child.alive().unwrap();
    })
    .await
    .unwrap();
    child.cleanup().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(child.process.as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    drop((child, endpoint, image, source, attempt));
    std::fs::remove_file(path.join("controller.exe")).unwrap();
    std::fs::remove_dir(path).unwrap();
}
