//! Disposable live-image fixture only. Run the ignored parent in a hosted
//! Windows process outside the runner Job, never on an enrolled machine.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
};

const CHILD_TEST: &str = "windows_update_bootstrap::running_image::running_image_child";

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
        // This temporary read-only observer must share DELETE with the retained
        // rename handle. That original handle still denies delete sharing.
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
        attempt
            .rename_checked(&image, Slot::Active, Slot::Backup, expected)
            .unwrap();
        assert_eq!(identity(&image).unwrap(), before);
        assert!(final_path(&image).unwrap().ends_with("controller.prev.exe"));
        endpoint.pipe.write_all(&[2]).await.unwrap();
        assert_eq!(endpoint.pipe.read_u8().await.unwrap(), 3);
        child.alive().unwrap();
        attempt
            .rename_checked(&image, Slot::Backup, Slot::Active, expected)
            .unwrap();
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
