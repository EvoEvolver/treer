//! Cooperative close/reopen transition; no path-to-authority constructor.
use super::*;

struct ReadPin<'a>(Stage<'a>);

fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

impl<'a> Stage<'a> {
    fn into_read_pin(
        self,
        after_close: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<ReadPin<'a>> {
        self.validate()?;
        let Self {
            ownership,
            file,
            object,
            bytes,
        } = self;
        drop(file);
        // Only the cooperative lock covers this gap. No invisible in-place
        // rewrite-and-restore detection or noncooperating CAS is claimed.
        after_close()?;
        ownership.validate()?;
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path = evidence_file::directory(ownership.attempt, &owner)?.join("consume.intent");
        let pin = ReadPin(Stage {
            ownership,
            file: open_read(&path)?,
            object,
            bytes,
        });
        pin.validate()?;
        Ok(pin)
    }
}

impl ReadPin<'_> {
    fn validate(&self) -> io::Result<()> {
        self.0.validate()?;
        let owner = crate::windows::current_user_sid().map_err(|_| denied())?;
        let path =
            evidence_file::directory(self.0.ownership.attempt, &owner)?.join("consume.intent");
        let current = open_read(&path)?;
        protected_private_acl(&current, &owner)?;
        if full_identity(&current)? != self.0.object || read_bytes(&current)? != self.0.bytes {
            return Err(denied());
        }
        self.0.validate()
    }
}

#[test]
fn native_consume_pin_excludes_access_until_close() {
    let fixture = super::super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000072",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = consume_record::fixture_bytes();
    let path = attempt.path.join("consume.intent");
    let stage = Stage::create(&ownership, &bytes, false).unwrap();
    let object = stage.object;
    let pin = stage.into_read_pin(|| Ok(())).unwrap();
    assert_eq!(pin.0.object, object);
    assert_eq!(
        read_bytes(&probe(&path, GENERIC_READ).unwrap()).unwrap(),
        bytes
    );
    for (access, label) in [(GENERIC_WRITE, "WriteOpen"), (DELETE, "DeleteOpen")] {
        let error = probe(&path, access).expect_err("pin must exclude access");
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        eprintln!("consume_pin_fixture stage={label} win32=32");
        pin.validate().unwrap();
    }
    drop(pin);
    drop(probe(&path, GENERIC_WRITE).unwrap());
    drop(probe(&path, DELETE).unwrap());
    assert!(Stage::create(&ownership, &bytes, false).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    eprintln!("consume_pin_fixture stage=ReleasedOpen win32=0 retry_refused=1");
    drop(ownership);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn native_consume_pin_refuses_gap_mutation_and_interruption() {
    for mutate in [false, true] {
        let fixture = super::super::super::super::tests::PrivateImage::new();
        let attempt = Attempt::create(
            &fixture.directory,
            "attempt-00000000000000000000000000000073",
        )
        .unwrap();
        let ownership = FixtureLock::acquire(&attempt, true).unwrap();
        let bytes = consume_record::fixture_bytes();
        let path = attempt.path.join("consume.intent");
        let stage = Stage::create(&ownership, &bytes, false).unwrap();
        let object = stage.object;
        let mut expected = bytes.clone();
        if mutate {
            expected[173] ^= 1;
        }
        assert!(stage
            .into_read_pin(|| {
                if !mutate {
                    return Err(denied());
                }
                let mut writer = probe(&path, GENERIC_WRITE)?;
                writer.seek(SeekFrom::Start(0))?;
                writer.write_all(&expected)?;
                writer.sync_all()
            })
            .is_err());
        let reader = probe(&path, GENERIC_READ).unwrap();
        assert_eq!(full_identity(&reader).unwrap(), object);
        assert_eq!(read_bytes(&reader).unwrap(), expected);
        assert!(Stage::create(&ownership, &bytes, false).is_err());
        assert!(!attempt.path.join("reason.evidence").exists());
        eprintln!(
            "consume_pin_fixture stage={} refused=1 preserved=1",
            if mutate {
                "GapMutation"
            } else {
                "GapInterruption"
            }
        );
        drop(reader);
        drop(ownership);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
        let path = attempt.path.clone();
        drop(attempt);
        std::fs::remove_dir(path).unwrap();
    }
}

#[test]
fn native_consume_pin_refuses_same_bytes_gap_replacement() {
    let fixture = super::super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000074",
    )
    .unwrap();
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = consume_record::fixture_bytes();
    let path = attempt.path.join("consume.intent");
    let original_path = attempt.path.join("consume.original");
    let stage = Stage::create(&ownership, &bytes, false).unwrap();
    let original_object = stage.object;
    let replacement_ready = std::cell::Cell::new(false);
    assert!(stage
        .into_read_pin(|| {
            // Keep the original object alive under a different name, so its
            // identity cannot be recycled for the same-bytes replacement.
            std::fs::rename(&path, &original_path)?;
            let replacement = Stage::create(&ownership, &bytes, false)?;
            assert_ne!(replacement.object, original_object);
            replacement.validate()?;
            drop(replacement);
            replacement_ready.set(true);
            Ok(())
        })
        .is_err());
    assert!(replacement_ready.get());
    let original = probe(&original_path, GENERIC_READ).unwrap();
    let replacement = probe(&path, GENERIC_READ).unwrap();
    assert_eq!(full_identity(&original).unwrap(), original_object);
    assert_ne!(full_identity(&replacement).unwrap(), original_object);
    assert_eq!(read_bytes(&original).unwrap(), bytes);
    assert_eq!(read_bytes(&replacement).unwrap(), bytes);
    ownership.validate().unwrap();
    assert!(Stage::create(&ownership, &bytes, false).is_err());
    assert!(!attempt.path.join("reason.evidence").exists());
    eprintln!("consume_pin_fixture stage=GapReplacement refused=1 original_preserved=1 replacement_preserved=1");
    drop(original);
    drop(replacement);
    drop(ownership);
    std::fs::remove_file(original_path).unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}
