//! Fresh-process fixture diagnostic, deliberately incapable of acquisition.
use super::*;

#[derive(Debug, PartialEq, Eq)]
enum Diagnosis {
    ParsedUntrusted,
    Refused,
}

// A path can supply bounded, syntactically valid DATA, never live ownership,
// independently committed expectations, a HeadPin, or a consume capability.
fn diagnose(path: &Path) -> Diagnosis {
    match open_head(path).and_then(|file| read_head(&file)) {
        Ok(_) => Diagnosis::ParsedUntrusted,
        Err(_) => Diagnosis::Refused,
    }
}

struct Probe(std::process::Child);

impl Drop for Probe {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn child_diagnosis(path: &Path) {
    let mut probe = Probe(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows_update_bootstrap::checked_files::ownership::head_pin::diagnosis::native_head_pin_fresh_process_diagnoses_without_custody",
                "--nocapture",
            ])
            .env("TREER_TEST_HEAD_DIAGNOSE", path)
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = probe.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "head diagnostic timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_head_pin_fresh_process_diagnoses_without_custody() {
    if let Some(path) = std::env::var_os("TREER_TEST_HEAD_DIAGNOSE") {
        assert_eq!(diagnose(Path::new(&path)), Diagnosis::ParsedUntrusted);
        // No ownership acquisition, HeadPin aggregate, context, native rename,
        // evidence publication or sink is reachable on this branch.
        eprintln!("head_diagnosis_fixture stage=ChildParsedUntrusted diagnose_only=1");
        return;
    }
    let fixture = super::super::super::super::tests::PrivateImage::new();
    let attempt = Attempt::create(
        &fixture.directory,
        "attempt-00000000000000000000000000000069",
    )
    .unwrap();
    let path = attempt.path.join("receipt.json");
    assert_eq!(diagnose(&path), Diagnosis::Refused);
    let ownership = FixtureLock::acquire(&attempt, true).unwrap();
    let bytes = head_record::fixture_bytes();
    let pin = ownership.publish_head(&bytes).unwrap();
    let object = pin.object;
    child_diagnosis(&path);
    pin.validate().unwrap();
    eprintln!("head_diagnosis_fixture stage=LiveParentChecked unchanged=1");
    drop(pin);
    drop(ownership);
    // A fresh process after all parent custody closes still gets only data.
    // It is never given an Attempt, old object ID or a recovered head token.
    child_diagnosis(&path);
    let reader = open_head(&path).unwrap();
    assert_eq!(full_identity(&reader).unwrap(), object);
    assert_eq!(read_head(&reader).unwrap(), bytes);
    assert!(!attempt.path.join("reason.evidence").exists());
    assert!(!attempt.path.join("receipt.next").exists());
    eprintln!("head_diagnosis_fixture stage=ClosedParentChecked unchanged=1");
    drop(reader);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(attempt.path.join("fixture.lock")).unwrap();
    let path = attempt.path.clone();
    drop(attempt);
    std::fs::remove_dir(path).unwrap();
}
