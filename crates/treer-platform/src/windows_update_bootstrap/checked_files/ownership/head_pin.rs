//! Cooperative publication and read custody only, not acquisition authority.
use super::*;

mod diagnosis;

type HeadSnapshot = ((u64, [u8; 16]), [u8; 32]);

pub(in crate::windows_update_bootstrap::checked_files) struct HeadPin<'a> {
    ownership: &'a FixtureLock<'a>,
    file: File,
    object: (u64, [u8; 16]),
    bytes: Vec<u8>,
}

fn open_head(path: &Path) -> io::Result<File> {
    // No WRITE or DELETE sharing for the entire retained-head interval.
    // OpenOptions creates a non-inheritable handle; validate it explicitly too.
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn read_head(mut file: &File) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() != 181 {
        return Err(denied());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(182).read_to_end(&mut bytes)?;
    if !head_record::valid_bytes(&bytes) {
        return Err(denied());
    }
    Ok(bytes)
}

impl<'a> FixtureLock<'a> {
    // The fixture publishes only an initial head, while holding live ownership.
    // It cannot reopen a path to reconstruct a HeadPin or recovered authority.
    pub(in crate::windows_update_bootstrap::checked_files) fn publish_head(
        &'a self,
        bytes: &[u8],
    ) -> io::Result<HeadPin<'a>> {
        self.validate()?;
        if !head_record::valid_bytes(bytes) {
            return Err(denied());
        }
        let published = self
            .attempt
            .replace_record(bytes, None, RecordFault::None)?;
        let object = full_identity(&published)?;
        self.validate()?;
        // The helper has already closed its writer and returns a read handle
        // allowing delete sharing. Replace that with the restrictive pin and
        // compare full identity/bytes across the cooperative-lock transition.
        drop(published);
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = evidence_file::directory(self.attempt, &owner)?.join("receipt.json");
        let pin = HeadPin {
            ownership: self,
            file: open_head(&path)?,
            object,
            bytes: bytes.to_vec(),
        };
        pin.validate()?;
        Ok(pin)
    }
}

impl HeadPin<'_> {
    // Plain identity/digest data, with no borrowed handle or capability. Only
    // the live pin plus independent expected bytes can obtain this snapshot.
    pub(in crate::windows_update_bootstrap::checked_files) fn snapshot_for(
        &self,
        attempt: &Attempt,
        expected: &[u8],
    ) -> io::Result<HeadSnapshot> {
        self.validate()?;
        if !ptr::eq(self.ownership.attempt, attempt) || self.bytes != expected {
            return Err(denied());
        }
        Ok((self.object, Sha256::digest(&self.bytes).into()))
    }

    // Independent expectations are fixed before native acquisition. This
    // returns data only, and cannot reconstruct a pin from a file or digest.
    pub(in crate::windows_update_bootstrap::checked_files) fn digest_for(
        &self,
        attempt: &Attempt,
        expected: &[u8],
    ) -> io::Result<[u8; 32]> {
        self.snapshot_for(attempt, expected)
            .map(|(_, digest)| digest)
    }

    fn validate(&self) -> io::Result<()> {
        self.ownership.validate()?;
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = evidence_file::directory(self.ownership.attempt, &owner)?.join("receipt.json");
        let current = open_head(&path)?;
        for file in [&self.file, &current] {
            protected_private_acl(file, &owner)?;
            let (_, _, links, flags) = identity(file)?;
            let mut handle_flags = 0;
            check(unsafe { GetHandleInformation(file.as_raw_handle(), &mut handle_flags) })?;
            if links != 1
                || !matches!(flags, FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_ARCHIVE)
                || handle_flags & HANDLE_FLAG_INHERIT != 0
                || full_identity(file)? != self.object
                || final_path(file)? != path
                || read_head(file)? != self.bytes
            {
                return Err(denied());
            }
        }
        self.ownership.validate()
    }
}

#[test]
fn native_head_pin_snapshot_requires_live_expected_custody() {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000075",
    )
    .unwrap();
    let foreign = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000076",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = head_record::fixture_bytes();
    let pin = ownership.publish_head(&bytes).unwrap();
    let (object, digest) = pin.snapshot_for(&attempt, &bytes).unwrap();
    let reader = open_head(&attempt.path.join("receipt.json")).unwrap();
    assert_eq!(object, full_identity(&reader).unwrap());
    assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert!(pin.snapshot_for(&foreign, &bytes).is_err());
    let mut changed = bytes.clone();
    changed[42] = 2; // Valid component, different independent expectation.
    assert!(head_record::valid_bytes(&changed));
    assert!(pin.snapshot_for(&attempt, &changed).is_err());
    assert_eq!(
        pin.snapshot_for(&attempt, &bytes).unwrap(),
        (object, digest)
    );
    assert_eq!(read_head(&reader).unwrap(), bytes);
    assert!(!attempt.path.join("consume.intent").exists());
    assert!(!attempt.path.join("reason.evidence").exists());
    eprintln!("head_snapshot_fixture identity_match=1 digest_match=1 foreign_refused=1 expected_refused=1 unchanged=1");
    drop(reader);
    drop(pin);
    drop(ownership);
    std::fs::remove_file(attempt.path.join("receipt.json")).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let paths = [attempt.path.clone(), foreign.path.clone()];
    drop(attempt);
    drop(foreign);
    for path in paths {
        std::fs::remove_dir(path).unwrap();
    }
}

// A real second opener, with permissive sharing, isolates the access denied by
// the retained pin's sharing mode rather than its own incompatible share mask.
fn probe_access(path: &Path, access: u32) -> io::Result<()> {
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
    drop(unsafe { File::from_raw_handle(raw) });
    Ok(())
}

#[test]
fn native_head_pin_excludes_writers_and_deleters_until_close() {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000066",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = head_record::fixture_bytes();
    let pin = ownership.publish_head(&bytes).unwrap();
    let path = attempt.path.join("receipt.json");
    probe_access(&path, GENERIC_READ).unwrap();
    for (access, stage) in [(GENERIC_WRITE, "WriteOpen"), (DELETE, "DeleteOpen")] {
        let error = probe_access(&path, access).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        eprintln!("head_pin_fixture stage={stage} win32=32");
        pin.validate().unwrap();
    }
    drop(pin);
    probe_access(&path, GENERIC_WRITE).unwrap();
    probe_access(&path, DELETE).unwrap();
    eprintln!("head_pin_fixture stage=ReleasedWriteOpen win32=0");
    ownership.validate().unwrap();
    drop(ownership);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn native_head_pin_refuses_invalid_or_existing_initial_head() {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000067",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = head_record::fixture_bytes();
    assert!(ownership.publish_head(b"invalid").is_err());
    assert!(!attempt.path.join("receipt.json").exists());
    let pin = ownership.publish_head(&bytes).unwrap();
    let object = pin.object;
    drop(pin);
    assert!(ownership.publish_head(&bytes).is_err());
    let current = open_head(&attempt.path.join("receipt.json")).unwrap();
    assert_eq!(full_identity(&current).unwrap(), object);
    assert_eq!(read_head(&current).unwrap(), bytes);
    assert!(!attempt.path.join("receipt.next").exists());
    drop(current);
    drop(ownership);
    std::fs::remove_file(attempt.path.join("receipt.json")).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn native_head_pin_refuses_same_bytes_replacement_snapshot() {
    replacement_snapshot(false);
}

#[test]
fn native_head_pin_refuses_advanced_head_snapshot() {
    replacement_snapshot(true);
}

// Deliberately release the real pin before replacing the file. This is a
// validator fault fixture, NOT a way to recover custody or advance a live pin.
fn replacement_snapshot(advance: bool) {
    let fixture = super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000068",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = head_record::fixture_bytes();
    let pin = ownership.publish_head(&bytes).unwrap();
    let original = pin.object;
    drop(pin);
    let path = attempt.path.join("receipt.json");
    // Keep the old object alive under a delete-sharing reader so Windows
    // cannot recycle its file ID while creating the replacement.
    let old = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)
        .unwrap();
    assert_eq!(full_identity(&old).unwrap(), original);
    let mut next = bytes.clone();
    if advance {
        // The existing head codec fixes revision at offset 45, u64 LE.
        next[45..53].copy_from_slice(&8u64.to_le_bytes());
    }
    assert!(head_record::valid_bytes(&next));
    let published = attempt
        .replace_record(&next, Some(&old), RecordFault::None)
        .unwrap();
    let replacement = full_identity(&published).unwrap();
    assert_ne!(replacement, original);
    assert_eq!(read_head(&published).unwrap(), next);
    drop(published);
    drop(old);
    // Inject only the reopened handle into a stale in-memory snapshot. This
    // aggregate is constructed solely inside this negative test, never by an
    // API that a fresh process could call to mint a HeadPin.
    let mut stale = HeadPin {
        ownership: &ownership,
        file: open_head(&path).unwrap(),
        object: original,
        bytes: bytes.clone(),
    };
    assert!(stale.validate().is_err());
    assert!(stale.digest_for(&attempt, &bytes).is_err());
    if advance {
        // Even an injected matching new object ID cannot bless old bytes:
        // the independently retained predecessor must not follow the head.
        stale.object = replacement;
        assert!(stale.validate().is_err());
        assert!(stale.digest_for(&attempt, &bytes).is_err());
    }
    assert_eq!(full_identity(&stale.file).unwrap(), replacement);
    assert_eq!(read_head(&stale.file).unwrap(), next);
    assert!(!attempt.path.join("reason.evidence").exists());
    assert!(!attempt.path.join("receipt.next").exists());
    eprintln!(
        "head_snapshot_fixture stage={} refused=1",
        if advance {
            "AdvancedBytes"
        } else {
            "SameBytesNewObject"
        }
    );
    drop(stale);
    ownership.validate().unwrap();
    drop(ownership);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}
