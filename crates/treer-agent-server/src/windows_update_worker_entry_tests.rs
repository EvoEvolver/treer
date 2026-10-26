use super::*;
use ed25519_dalek::{pkcs8::EncodePublicKey, SigningKey};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("treer-worker-trust-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
    fn key(&self, name: &str, seed: u8) -> PathBuf {
        let path = self.0.join(name);
        let key = SigningKey::from_bytes(&[seed; 32]).verifying_key();
        std::fs::write(&path, key.to_public_key_pem(Default::default()).unwrap()).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn windows_update_worker_entry_configured_official_explicit_local_and_fail_closed() {
    let fixture = Fixture::new();
    let official = url::Url::parse("https://releases.treer.ai/").unwrap();
    let custom = url::Url::parse("https://publisher.example/").unwrap();
    let absent = fixture.0.join("absent.pem");
    let bundled = Trust::configured(&official, None, &absent).unwrap();
    assert!(matches!(bundled.source, Source::Official));
    assert!(Trust::configured(&custom, None, &absent).is_err());
    assert!(Trust::configured(&official, Some(&absent), &absent).is_err());
    let explicit = fixture.key("explicit.pem", 35);
    let local = fixture.key("local.pem", 36);
    let selected = Trust::configured(&custom, Some(&explicit), &local).unwrap();
    assert_eq!(
        selected.key,
        SigningKey::from_bytes(&[35; 32]).verifying_key()
    );
    let selected = Trust::configured(&official, None, &local).unwrap();
    assert_eq!(
        selected.key,
        SigningKey::from_bytes(&[36; 32]).verifying_key()
    );
    let selected = Trust::configured(&custom, None, &local).unwrap();
    assert_eq!(
        selected.key,
        SigningKey::from_bytes(&[36; 32]).verifying_key()
    );
    std::fs::write(&local, "invalid").unwrap();
    assert!(Trust::configured(&official, None, &local).is_err());
    std::fs::write(&explicit, vec![b'a'; 16 * 1024 + 1]).unwrap();
    assert!(Trust::configured(&custom, Some(&explicit), &local).is_err());
}

#[test]
fn windows_update_worker_entry_strict_argv_and_changed_key_never_fallback() {
    let fixture = Fixture::new();
    let base = url::Url::parse("https://releases.treer.ai/").unwrap();
    let absent = fixture.0.join("absent.pem");
    let op = Uuid::new_v4();
    let selector = Uuid::new_v4();
    let key = fixture.key("configured.pem", 35);
    let trust = Trust::configured(&base, Some(&key), &absent).unwrap();
    let args = trust
        .arguments(op, selector, &trust.key.to_bytes())
        .unwrap();
    assert_eq!(args[0], SWITCH);
    let (parsed, endpoint, source) = arguments(args.into_iter().skip(1)).unwrap();
    assert_eq!(parsed, op);
    assert_eq!(endpoint, selector.simple().to_string());
    assert_eq!(source.load().unwrap(), trust.key);
    let official = Trust::configured(&base, None, &absent).unwrap();
    assert!(official
        .arguments(op, selector, &trust.key.to_bytes())
        .is_err());
    assert!(trust
        .arguments(Uuid::nil(), selector, &trust.key.to_bytes())
        .is_err());
    for suffix in [
        vec![],
        vec!["--public-key"],
        vec!["--public-key", "relative.pem"],
        vec!["--official", "--public-key", "file.pem"],
        vec!["--official", "extra"],
        vec!["--pem", "untrusted-offer-key"],
    ] {
        let args = [op.to_string(), selector.simple().to_string()]
            .into_iter()
            .chain(suffix.into_iter().map(String::from))
            .map(OsString::from);
        assert!(arguments(args).is_err());
    }
    fixture.key("configured.pem", 99);
    assert!(trust
        .arguments(op, selector, &trust.key.to_bytes())
        .is_err());
    std::fs::remove_file(&key).unwrap();
    assert!(trust
        .arguments(op, selector, &trust.key.to_bytes())
        .is_err());
}
