//! Private decision storage custody only, never continuation authority.
use super::*;

mod read_pin;

struct Stage<'a> {
    ownership: &'a FixtureLock<'a>,
    file: File,
    object: (u64, [u8; 16]),
    bytes: Vec<u8>,
}

fn read_bytes(mut file: &File) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() != 177 {
        return Err(denied());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(178).read_to_end(&mut bytes)?;
    if !consume_record::valid_bytes(&bytes) {
        return Err(denied());
    }
    Ok(bytes)
}

impl<'a> Stage<'a> {
    fn create(ownership: &'a FixtureLock<'a>, bytes: &[u8], interrupt: bool) -> io::Result<Self> {
        ownership.validate()?;
        if !consume_record::valid_bytes(bytes) {
            return Err(denied());
        }
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = evidence_file::directory(ownership.attempt, &owner)?.join("consume.intent");
        let descriptor = descriptor(&owner)?;
        let security = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ,
                &security,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_handle(raw) };
        if interrupt {
            file.write_all(&bytes[..17])?;
            file.sync_all()?;
            // Injected interruption, not process death or a real sync error.
            // Closing the handle must preserve the partial exclusive file.
            return Err(denied());
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        let stage = Self {
            ownership,
            object: full_identity(&file)?,
            file,
            bytes: bytes.to_vec(),
        };
        stage.validate()?;
        Ok(stage)
    }

    fn validate(&self) -> io::Result<()> {
        self.ownership.validate()?;
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        protected_private_acl(&self.file, &owner)?;
        let (_, _, links, flags) = identity(&self.file)?;
        let mut handle_flags = 0;
        check(unsafe { GetHandleInformation(self.file.as_raw_handle(), &mut handle_flags) })?;
        if links != 1
            || !matches!(flags, FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_ARCHIVE)
            || handle_flags & HANDLE_FLAG_INHERIT != 0
            || full_identity(&self.file)? != self.object
            || final_path(&self.file)?
                != evidence_file::directory(self.ownership.attempt, &owner)?.join("consume.intent")
            || read_bytes(&self.file)? != self.bytes
        {
            return Err(denied());
        }
        self.ownership.validate()
    }
}

fn probe(path: &Path, access: u32) -> io::Result<File> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(raw) })
}

#[test]
fn native_consume_stage_exclusive_writer_sharing_and_existing_refusal() {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000070",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = consume_record::fixture_bytes();
    let path = attempt.path.join("consume.intent");
    assert!(Stage::create(&ownership, b"invalid", false).is_err());
    assert!(!path.exists());
    let stage = Stage::create(&ownership, &bytes, false).unwrap();
    let object = stage.object;
    assert_eq!(
        read_bytes(&probe(&path, GENERIC_READ).unwrap()).unwrap(),
        bytes
    );
    for (access, label) in [(GENERIC_WRITE, "WriteOpen"), (DELETE, "DeleteOpen")] {
        let error = probe(&path, access).expect_err("competing access must fail");
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        eprintln!("consume_stage_fixture stage={label} win32=32");
    }
    assert!(Stage::create(&ownership, &bytes, false).is_err());
    stage.validate().unwrap();
    drop(stage);
    // Closing the writer does not make a complete existing decision fresh.
    assert!(Stage::create(&ownership, &bytes, false).is_err());
    let reader = probe(&path, GENERIC_READ).unwrap();
    assert_eq!(full_identity(&reader).unwrap(), object);
    assert_eq!(read_bytes(&reader).unwrap(), bytes);
    drop(reader);
    drop(probe(&path, GENERIC_WRITE).unwrap());
    drop(probe(&path, DELETE).unwrap());
    eprintln!("consume_stage_fixture stage=ReleasedOpen win32=0");
    drop(ownership);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn native_consume_stage_partial_file_is_preserved_and_burned() {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000071",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = consume_record::fixture_bytes();
    let path = attempt.path.join("consume.intent");
    assert!(Stage::create(&ownership, &bytes, true).is_err());
    let reader = probe(&path, GENERIC_READ).unwrap();
    let object = full_identity(&reader).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes[..17]);
    assert!(read_bytes(&reader).is_err());
    assert!(Stage::create(&ownership, &bytes, false).is_err());
    drop(reader);
    drop(ownership);
    // Even reacquiring fixture ownership cannot overwrite partial evidence.
    let reopened = FixtureLock::acquire(&attempt, false).unwrap();
    assert!(Stage::create(&reopened, &bytes, false).is_err());
    let reader = probe(&path, GENERIC_READ).unwrap();
    assert_eq!(full_identity(&reader).unwrap(), object);
    assert_eq!(std::fs::read(&path).unwrap(), bytes[..17]);
    assert!(!attempt.path.join("reason.evidence").exists());
    eprintln!("consume_stage_fixture stage=PartialPreserved bytes=17 retry_refused=1");
    drop(reader);
    drop(reopened);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}
