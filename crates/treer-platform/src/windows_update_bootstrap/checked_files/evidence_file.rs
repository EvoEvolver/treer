//! Disposable opaque evidence storage only. No reason schema or authority.
use super::*;

const LIMIT: usize = 8192;
const PREPARE: u32 =
    FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const PIN: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL;

fn at<T>(stage: &'static str, result: io::Result<T>) -> io::Result<T> {
    result.map_err(|error| {
        eprintln!(
            "evidence_fixture stage={stage} win32={}",
            error.raw_os_error().unwrap_or(0)
        );
        denied()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Created,
    Synced,
    Closed,
}

// Private, non-Clone, non-serializable custody of one live disposable Attempt.
struct EvidencePin<'a> {
    attempt: &'a Attempt,
    file: File,
    identity: (u32, u64, u32, u32),
    hash: [u8; 32],
}

fn directory(attempt: &Attempt, owner: &str) -> io::Result<PathBuf> {
    protected_private_acl(&attempt.directory, owner)?;
    let current = open_pinned(&attempt.path, true)?;
    if identity(&current)? != identity(&attempt.directory)? {
        return Err(denied());
    }
    final_path(&attempt.directory)
}

fn read(
    file: &File,
    owner: &str,
    expected: (u32, u64, u32, u32),
    hash: [u8; 32],
) -> io::Result<Vec<u8>> {
    let validate = || -> io::Result<u64> {
        protected_private_acl(file, owner)?;
        if identity(file)? != expected
            || expected.2 != 1
            || !matches!(
                expected.3,
                FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_READONLY
            )
        {
            return Err(denied());
        }
        let length = file.metadata()?.len();
        if length == 0 || length > LIMIT as u64 {
            return Err(denied());
        }
        Ok(length)
    };
    let length = validate()? as usize;
    let mut reader = file;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = vec![0; length + 1];
    let mut used = 0;
    loop {
        let count = reader.read(&mut bytes[used..]).map_err(|_| denied())?;
        if count == 0 {
            break;
        }
        used += count;
        if used > length {
            return Err(denied());
        }
    }
    if used != length || validate()? != length as u64 {
        return Err(denied());
    }
    bytes.truncate(length);
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != hash {
        return Err(denied());
    }
    Ok(bytes)
}

impl Attempt {
    // Hook is a test-only fault/inspection seam, never a production callback.
    fn store_evidence(
        &self,
        bytes: &[u8],
        expected: [u8; 32],
        mut hook: impl FnMut(Stage) -> io::Result<()>,
    ) -> io::Result<EvidencePin<'_>> {
        if bytes.is_empty()
            || bytes.len() > LIMIT
            || <[u8; 32]>::from(Sha256::digest(bytes)) != expected
        {
            return Err(denied());
        }
        // Even a pre-creation OS failure cannot restart this live attempt.
        if self.evidence_used.replace(true) {
            return Err(denied());
        }
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = directory(self, &owner)?.join("reason.evidence");
        let security = descriptor(&owner)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                PREPARE,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return at("create", Err(io::Error::last_os_error()));
        }
        let mut writer = unsafe { File::from_raw_handle(raw) };
        hook(Stage::Created)?;
        at("write", writer.write_all(bytes))?;
        at("sync", writer.sync_all())?;
        hook(Stage::Synced)?;
        let object = identity(&writer)?;
        if at("prepare_readback", read(&writer, &owner, object, expected))? != bytes {
            return Err(denied());
        }
        drop(writer);
        hook(Stage::Closed)?;
        if directory(self, &owner)?.join("reason.evidence") != path {
            return Err(denied());
        }
        let raw = unsafe {
            CreateFileW(
                wide(path.as_os_str()).as_ptr(),
                PIN,
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return at("pin_open", Err(io::Error::last_os_error()));
        }
        let file = unsafe { File::from_raw_handle(raw) };
        if final_path(&file)? != path
            || at("pin_readback", read(&file, &owner, object, expected))? != bytes
        {
            return Err(denied());
        }
        directory(self, &owner)?;
        Ok(EvidencePin {
            attempt: self,
            file,
            identity: object,
            hash: expected,
        })
    }
}

impl EvidencePin<'_> {
    // Only pins can read the slot; no reopen-by-path recovery/source API.
    fn read(&self) -> io::Result<Vec<u8>> {
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        if final_path(&self.file)? != directory(self.attempt, &owner)?.join("reason.evidence") {
            return Err(denied());
        }
        read(&self.file, &owner, self.identity, self.hash)
    }
}

#[test]
fn native_evidence_bounds_and_pins_deny_writers_and_deleters() {
    let fixture = super::super::tests::PrivateImage::new();
    for (index, size) in [1, LIMIT].into_iter().enumerate() {
        let name = format!("attempt-{:032x}", 40 + index);
        let path = fixture.directory.join(&name);
        let attempt = Attempt::create(&fixture.directory, &name).unwrap();
        let bytes = vec![7; size];
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        assert!(attempt.store_evidence(&[], hash, |_| Ok(())).is_err());
        assert!(attempt
            .store_evidence(&vec![0; LIMIT + 1], hash, |_| Ok(()))
            .is_err());
        assert!(attempt.store_evidence(&bytes, [0; 32], |_| Ok(())).is_err());
        assert!(!path.join("reason.evidence").exists());
        let blocked = || {
            assert!(OpenOptions::new()
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(path.join("reason.evidence"))
                .is_err());
            let raw = unsafe {
                CreateFileW(
                    wide(path.join("reason.evidence").as_os_str()).as_ptr(),
                    DELETE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if raw != INVALID_HANDLE_VALUE {
                drop(unsafe { File::from_raw_handle(raw) });
                panic!("delete access unexpectedly granted");
            }
        };
        let pin = attempt
            .store_evidence(&bytes, hash, |stage| {
                if stage != Stage::Closed {
                    blocked();
                }
                Ok(())
            })
            .unwrap();
        blocked();
        let mut flags = 0;
        check(unsafe { GetHandleInformation(pin.file.as_raw_handle(), &mut flags) }).unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        assert_eq!(pin.read().unwrap(), bytes);
        assert!(attempt.store_evidence(&bytes, hash, |_| Ok(())).is_err());
        assert!(std::fs::remove_file(path.join("reason.evidence")).is_err());
        drop(pin);
        drop(attempt);
        std::fs::remove_file(path.join("reason.evidence")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}

#[test]
fn native_evidence_failed_store_has_no_pin_and_cannot_retry_or_accept_replacement() {
    let fixture = super::super::tests::PrivateImage::new();
    for (index, stop) in [Stage::Created, Stage::Synced, Stage::Closed]
        .into_iter()
        .enumerate()
    {
        let name = format!("attempt-{:032x}", 50 + index);
        let path = fixture.directory.join(&name);
        let attempt = Attempt::create(&fixture.directory, &name).unwrap();
        let hash: [u8; 32] = Sha256::digest(b"evidence").into();
        let result = attempt.store_evidence(b"evidence", hash, |stage| {
            if stage == stop {
                Err(denied())
            } else {
                Ok(())
            }
        });
        // No successful pin means there is no source to pass to a verifier.
        assert!(result.map(|pin| pin.read()).is_err());
        assert!(attempt
            .store_evidence(b"evidence", hash, |_| Ok(()))
            .is_err());
        assert_eq!(
            std::fs::metadata(path.join("reason.evidence"))
                .unwrap()
                .len(),
            if stop == Stage::Created { 0 } else { 8 }
        );
        drop(attempt);
        std::fs::remove_file(path.join("reason.evidence")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
    let name = "attempt-00000000000000000000000000000060";
    let path = fixture.directory.join(name);
    let attempt = Attempt::create(&fixture.directory, name).unwrap();
    let hash: [u8; 32] = Sha256::digest(b"evidence").into();
    let result = attempt.store_evidence(b"evidence", hash, |stage| {
        if stage == Stage::Closed {
            std::fs::rename(path.join("reason.evidence"), path.join("retained.evidence"))?;
            std::fs::copy(path.join("retained.evidence"), path.join("reason.evidence"))?;
        }
        Ok(())
    });
    assert!(result.is_err());
    drop(result);
    assert!(attempt
        .store_evidence(b"evidence", hash, |_| Ok(()))
        .is_err());
    drop(attempt);
    for filename in ["reason.evidence", "retained.evidence"] {
        std::fs::remove_file(path.join(filename)).unwrap();
    }
    std::fs::remove_dir(path).unwrap();
}
