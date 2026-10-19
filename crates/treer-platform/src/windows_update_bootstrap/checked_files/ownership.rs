//! Disposable cooperative fixture ownership, never installation authority.
use super::*;

struct FixtureLock<'a> {
    attempt: &'a Attempt,
    file: File,
    object: (u64, [u8; 16]),
}

fn full_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info: FILE_ID_INFO = unsafe { mem::zeroed() };
    check(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            mem::size_of::<FILE_ID_INFO>() as u32,
        )
    })?;
    let id = info.FileId.Identifier;
    if matches!(info.VolumeSerialNumber, 0 | u64::MAX) || id == [0; 16] || id == [u8::MAX; 16] {
        return Err(denied());
    }
    Ok((info.VolumeSerialNumber, id))
}

impl<'a> FixtureLock<'a> {
    // Creation mode is selected only by the private fixture, never record data.
    // OPEN_EXISTING after a close establishes custody only, not recovered head
    // or failure authority. No public caller or head writer consumes this yet.
    fn acquire(attempt: &'a Attempt, create: bool) -> io::Result<Self> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = evidence_file::directory(attempt, &owner)?.join("fixture.lock");
        let descriptor = descriptor(&owner)?;
        let security = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                GENERIC_READ,
                0, // FILE_SHARE_NONE: excludes read, write and delete opens.
                &security,
                if create { CREATE_NEW } else { OPEN_EXISTING },
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_handle(raw) };
        let lock = Self {
            attempt,
            object: full_identity(&file)?,
            file,
        };
        lock.validate()?;
        Ok(lock)
    }

    fn validate(&self) -> io::Result<()> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        protected_private_acl(&self.file, &owner)?;
        let (_, _, links, attributes) = identity(&self.file)?;
        if links != 1
            || !matches!(attributes, FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_ARCHIVE)
            || full_identity(&self.file)? != self.object
            || final_path(&self.file)?
                != evidence_file::directory(self.attempt, &owner)?.join("fixture.lock")
        {
            return Err(denied());
        }
        let mut flags = 0;
        check(unsafe { GetHandleInformation(self.file.as_raw_handle(), &mut flags) })?;
        if flags & HANDLE_FLAG_INHERIT != 0 {
            return Err(denied());
        }
        Ok(())
    }
}

// A real second process performs only an OPEN_EXISTING probe. It cannot create
// ownership/head/failure capability from its inherited environment or pathname.
fn competing_open(path: &OsStr) -> io::Result<()> {
    let raw = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            GENERIC_READ,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    drop(unsafe { File::from_raw_handle(raw) });
    Ok(())
}

#[test]
fn native_fixture_lock_excludes_openers_and_releases_on_close() {
    const PROBE: &str = "TREER_TEST_FIXTURE_LOCK_PROBE";
    if let Some(path) = std::env::var_os(PROBE) {
        let status = competing_open(&path).unwrap_err().raw_os_error();
        assert_eq!(status, Some(ERROR_SHARING_VIOLATION as i32));
        eprintln!("ownership_fixture stage=ChildOpen win32=32");
        return;
    }
    let fixture = super::super::tests::PrivateImage::new();
    let name = "attempt-00000000000000000000000000000065";
    let attempt = Attempt::create(&fixture.directory, name).unwrap();
    let path = attempt.path.join("fixture.lock");
    let lock = FixtureLock::acquire(&attempt, true).unwrap();
    lock.validate().unwrap();
    let error = match FixtureLock::acquire(&attempt, false) {
        Err(error) => error,
        Ok(_) => panic!("second owner unexpectedly acquired"),
    };
    assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
    eprintln!("ownership_fixture stage=SameProcessOpen win32=32");
    assert!(std::fs::remove_file(&path).is_err());
    // Spawn the exact test again as a bounded, no-worker native probe. The lock
    // is non-inheritable; exclusion must come from the parent's live handle.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "windows_update_bootstrap::checked_files::ownership::native_fixture_lock_excludes_openers_and_releases_on_close",
            "--nocapture",
        ])
        .env(PROBE, &path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let result = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("lock probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(result.success());
    lock.validate().unwrap();
    let object = lock.object;
    drop(lock);
    let reopened = FixtureLock::acquire(&attempt, false).unwrap();
    assert_eq!(reopened.object, object);
    reopened.validate().unwrap();
    eprintln!("ownership_fixture stage=ReleasedOpen win32=0");
    drop(reopened);
    std::fs::remove_file(path).unwrap();
    let directory = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(directory).unwrap();
}
