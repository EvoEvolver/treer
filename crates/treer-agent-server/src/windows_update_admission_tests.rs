use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{pkcs8::EncodePublicKey, Signer, SigningKey};
use serde_json::json;
use std::io::Cursor;

pub(crate) const NEW: &[u8] = b"signed target fixture, never executed";
pub(crate) const OLD: &[u8] = b"installed fixture, never executed";

#[test]
fn windows_update_launch_hashes_entire_held_image_not_current_offset() {
    let bytes = b"whole verified worker image";
    let mut file = Cursor::new(bytes.to_vec());
    file.set_position(bytes.len() as u64);
    require_worker_image(&mut file, &hash(bytes)).unwrap();
    file.get_mut()[0] ^= 1;
    assert!(require_worker_image(&mut file, &hash(bytes)).is_err());
    assert!(require_worker_image(&mut Cursor::new(Vec::<u8>::new()), &hash(b"")).is_err());
}

#[test]
fn windows_update_launch_refuses_seek_or_read_error_before_digest_acceptance() {
    struct RefusedSeek;
    impl std::io::Read for RefusedSeek {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("must refuse seek before reading an unbound suffix");
        }
    }
    impl std::io::Seek for RefusedSeek {
        fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
            Err(std::io::Error::other("fixture seek refused"))
        }
    }
    struct RefusedRead;
    impl std::io::Read for RefusedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("fixture read refused"))
        }
    }
    impl std::io::Seek for RefusedRead {
        fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
            Ok(0)
        }
    }
    assert!(require_worker_image(&mut RefusedSeek, &hash(NEW)).is_err());
    assert!(require_worker_image(&mut RefusedRead, &hash(NEW)).is_err());
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn receive_fixture(fixture: &Fixture, expected: &Binding, bytes: [&[u8]; 4]) -> Result<()> {
    let [mut a, mut b, mut c, mut d] = bytes.map(Cursor::new);
    for reader in [&mut a, &mut b, &mut c, &mut d] {
        reader.set_position(reader.get_ref().len() as u64);
    }
    verify_received_inputs(
        &fixture.documents(),
        &fixture.key.verifying_key(),
        expected,
        [&mut a, &mut b, &mut c, &mut d],
    )
}

#[test]
fn windows_update_receiver_whole_inputs_and_exact_digests() {
    let fixture = Fixture::new(|_| {});
    let expected = fixture.verify(NEW, OLD).unwrap();
    receive_fixture(&fixture, &expected, [OLD, OLD, NEW, NEW]).unwrap();
    for index in 0..4 {
        let mut bytes = [OLD, OLD, NEW, NEW];
        bytes[index] = b"wrong whole file";
        assert!(receive_fixture(&fixture, &expected, bytes).is_err());
    }
    for field in [
        "old_controller_sha256",
        "old_cli_sha256",
        "controller_sha256",
        "cli_sha256",
        "signed_manifest_sha256",
    ] {
        let mut value = serde_json::to_value(&expected).unwrap();
        value[field] = json!("0".repeat(64));
        let changed = serde_json::from_value(value).unwrap();
        assert!(
            receive_fixture(&fixture, &changed, [OLD, OLD, NEW, NEW]).is_err(),
            "{field}"
        );
    }
    for index in 0..4 {
        let mut bytes = [OLD, OLD, NEW, NEW];
        bytes[index] = b"";
        assert!(receive_fixture(&fixture, &expected, bytes).is_err());
    }
    assert!(receive_fixture(
        &fixture,
        &expected,
        [OLD, OLD, &[NEW, b"extra"].concat(), NEW]
    )
    .is_err());
}

#[test]
fn windows_update_receiver_signature_trust_and_io_refusals() {
    let source = Fixture::new(|_| {});
    let expected = source.verify(NEW, OLD).unwrap();
    for mode in 0..4 {
        let mut fixture = Fixture::new(|_| {});
        match mode {
            0 => fixture.key = SigningKey::from_bytes(&[99; 32]),
            1 => fixture.channel_signature[0] ^= 1,
            2 => fixture.manifest_signature[0] ^= 1,
            _ => fixture.manifest.push(b' '),
        }
        assert!(receive_fixture(&fixture, &expected, [OLD, OLD, NEW, NEW]).is_err());
    }
    struct Broken(bool);
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            assert!(!self.0, "failed seek must refuse before any read");
            Err(std::io::Error::other("fixture read refused"))
        }
    }
    impl std::io::Seek for Broken {
        fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
            if self.0 {
                Err(std::io::Error::other("fixture seek refused"))
            } else {
                Ok(0)
            }
        }
    }
    for seek_error in [false, true] {
        let mut broken = Broken(seek_error);
        let [mut b, mut c, mut d] = [OLD, NEW, NEW].map(Cursor::new);
        assert!(verify_received_inputs(
            &source.documents(),
            &source.key.verifying_key(),
            &expected,
            [&mut broken, &mut b, &mut c, &mut d]
        )
        .is_err());
    }
}

pub(crate) struct Fixture {
    pub(crate) key: SigningKey,
    channel: Vec<u8>,
    channel_signature: Vec<u8>,
    manifest: Vec<u8>,
    manifest_signature: Vec<u8>,
}

impl Fixture {
    pub(crate) fn new(mutate: impl FnOnce(&mut serde_json::Value)) -> Self {
        Self::with_target(NEW, mutate)
    }

    pub(crate) fn with_target(target: &[u8], mutate: impl FnOnce(&mut serde_json::Value)) -> Self {
        let key = SigningKey::from_bytes(&[35; 32]); // synthetic test authority only
        let key_id =
            hash(key.verifying_key().to_public_key_der().unwrap().as_bytes())[..16].to_owned();
        let mut manifest = json!({
            "schema_version":2, "version":"v0.1.16", "signing_key_id":key_id,
            "artifacts":[
                {"binary":"treer-agent-server","platform":"windows-x86_64",
                 "path":"releases/v0.1.16/windows-x86_64/treer-agent-server",
                 "size":target.len(),"sha256":hash(target)},
                {"binary":"treer","platform":"windows-x86_64",
                 "path":"releases/v0.1.16/windows-x86_64/treer",
                 "size":target.len(),"sha256":hash(target)}
            ]
        });
        mutate(&mut manifest);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let channel = serde_json::to_vec(&json!({
            "schema_version":1,"channel":"stable","version":"v0.1.16",
            "manifest_path":"releases/v0.1.16/manifest.json","manifest_sha256":hash(&manifest)
        }))
        .unwrap();
        let sign = |bytes: &[u8]| {
            serde_json::to_vec(&json!({
                "schema_version":1,"algorithm":"ed25519","key_id":key_id,
                "signature":STANDARD.encode(key.sign(bytes).to_bytes())
            }))
            .unwrap()
        };
        let channel_signature = sign(&channel);
        let manifest_signature = sign(&manifest);
        Self {
            key,
            channel,
            channel_signature,
            manifest,
            manifest_signature,
        }
    }

    pub(crate) fn documents(&self) -> Documents<'_> {
        Documents {
            channel: &self.channel,
            channel_signature: &self.channel_signature,
            manifest: &self.manifest,
            manifest_signature: &self.manifest_signature,
            channel_name: "stable",
        }
    }

    fn verify(&self, target: &[u8], old: &[u8]) -> Result<Binding> {
        let mut binding = Binding {
            operation_id: uuid::Uuid::new_v4(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            installation_volume: 10,
            installation_file_id: 20,
            old_controller_sha256: String::new(),
            old_cli_sha256: String::new(),
            controller_sha256: String::new(),
            cli_sha256: String::new(),
            signed_manifest_sha256: String::new(),
        };
        verify_inputs(
            &self.documents(),
            &self.key.verifying_key(),
            &mut binding,
            PairReaders {
                controller: &mut Cursor::new(old),
                cli: &mut Cursor::new(old),
            },
            PairReaders {
                controller: &mut Cursor::new(target),
                cli: &mut Cursor::new(target),
            },
        )?;
        Ok(binding)
    }
}

#[test]
fn windows_update_admission_signed_inputs_and_metadata_refusals() {
    let fixture = Fixture::new(|_| {});
    let binding = fixture.verify(NEW, OLD).unwrap();
    assert_eq!(binding.controller_sha256, hash(NEW));
    assert_eq!(binding.old_controller_sha256, hash(OLD));
    assert_eq!(binding.signed_manifest_sha256, hash(&fixture.manifest));
    for mutate in [
        |v: &mut serde_json::Value| {
            v["artifacts"][0]["platform"] = json!("linux-x86_64");
        },
        |v: &mut serde_json::Value| {
            v["artifacts"][0]["path"] = json!("../other");
        },
        |v: &mut serde_json::Value| {
            let duplicate = v["artifacts"][0].clone();
            v["artifacts"].as_array_mut().unwrap().push(duplicate);
        },
    ] {
        assert!(Fixture::new(mutate).verify(NEW, OLD).is_err());
    }
    let mut fixture = Fixture::new(|_| {});
    fixture.channel_signature[0] ^= 1;
    assert!(fixture.verify(NEW, OLD).is_err());
    let mut fixture = Fixture::new(|_| {});
    fixture.manifest_signature[0] ^= 1;
    assert!(fixture.verify(NEW, OLD).is_err());
    let mut fixture = Fixture::new(|_| {});
    fixture.manifest.push(b' '); // still valid JSON, invalid signed bytes
    assert!(fixture.verify(NEW, OLD).is_err());
    let mut fixture = Fixture::new(|_| {});
    fixture.key = SigningKey::from_bytes(&[36; 32]);
    assert!(fixture.verify(NEW, OLD).is_err());
}

#[test]
fn windows_update_admission_file_digest_bounds_and_read_failure() {
    let fixture = Fixture::new(|_| {});
    for target in [
        b"wrong".as_slice(),
        &NEW[..NEW.len() - 1],
        &[NEW, b"x"].concat(),
    ] {
        assert!(fixture.verify(target, OLD).is_err());
    }
    assert!(fixture.verify(NEW, b"").is_err());
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::PermissionDenied.into())
        }
    }
    assert!(digest(&mut Broken, None).is_err());
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::path::{Path, PathBuf};
    use treer_platform::{windows_update_inputs::PinnedPair, windows_update_store::UpdateStore};

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("treer-update-admission-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            treer_platform::secure_directory(&root).unwrap();
            Self(root)
        }
        fn directory(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir(&path).unwrap();
            treer_platform::secure_directory(&path).unwrap();
            path
        }
        fn pair(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.directory(name);
            for file in ["treer-agent-server.exe", "treer.exe"] {
                let file = path.join(file);
                std::fs::write(&file, bytes).unwrap();
                treer_platform::secure_file(&file).unwrap();
            }
            path
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn prepare(installed: &Path, target: &Path, fixture: &Fixture) -> Result<AdmittedInputs> {
        prepare_operation(installed, target, fixture, uuid::Uuid::new_v4())
    }

    fn pair_ready(
        root: &Root,
        installed: &Path,
        target: &Path,
        fixture: &Fixture,
    ) -> (StartupBackedUpInputs, PathBuf) {
        use treer_platform::windows_update_backup::BackupDirectory;
        let job = root.directory("pair-job");
        let backup = root.directory("pair-backup");
        let state = root.directory("pair-state");
        crate::startup_store::StartupStore::load(state.join("srv_fixture-agent-startup.json"))
            .unwrap()
            .deny_absent("ag_deleted")
            .unwrap();
        let ready = prepare(installed, target, fixture)
            .unwrap()
            .reserve(UpdateStore::open(&job).unwrap())
            .unwrap()
            .backup(BackupDirectory::open(&backup).unwrap())
            .unwrap()
            .backup_startup(BackupDirectory::open(&state).unwrap())
            .unwrap();
        (ready, backup)
    }

    #[test]
    fn windows_update_pair_native_publishes_and_explicitly_restores_complete_files() {
        for restore in [false, true] {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let fixture = Fixture::new(|_| {});
            let (ready, backup) = pair_ready(&root, &installed, &target, &fixture);
            let binding = ready.binding().clone();
            let published = ready.publish_pair().unwrap();
            assert_eq!(published.phase(), FilePairPhase::FilesPublished);
            assert_eq!(published.binding(), &binding);
            published.inspect_pair().unwrap();
            assert!(prepare(&installed, &target, &fixture).is_err());
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(installed.join("treer.exe"))
                .is_err());
            let history = std::fs::read(backup.join("publication.evidence")).unwrap();
            if restore {
                let restored = published.restore_files().unwrap();
                assert_eq!(restored.phase(), FilePairPhase::FilesRestored);
                restored.inspect_pair().unwrap();
                assert_eq!(
                    std::fs::read(backup.join("publication.evidence")).unwrap(),
                    history
                );
                assert!(backup.join("restoration.evidence").exists());
                assert!(restored.restore_files().is_err());
            } else {
                drop(published);
            }
            let expected = if restore { OLD } else { NEW };
            for name in ["treer-agent-server.exe", "treer.exe"] {
                assert_eq!(std::fs::read(installed.join(name)).unwrap(), expected);
            }
            assert!(installed.join(".treer-controller-update.claim").exists());
        }
    }

    #[test]
    fn windows_update_pair_native_locked_destinations_refuse_or_compensate_without_mixed_result() {
        use std::os::windows::fs::OpenOptionsExt;
        for locked in ["treer-agent-server.exe", "treer.exe"] {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let fixture = Fixture::new(|_| {});
            let (ready, _) = pair_ready(&root, &installed, &target, &fixture);
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1) // FILE_SHARE_READ, deliberately no DELETE sharing
                .open(installed.join(locked))
                .unwrap();
            let refused = ready.publish_pair().unwrap();
            let expected = if locked == "treer.exe" {
                FilePairPhase::FilesRestored
            } else {
                FilePairPhase::PublicationRefused
            };
            assert_eq!(refused.phase(), expected);
            refused.inspect_pair().unwrap();
            assert!(refused.restore_files().is_err());
            drop(lock);
            for name in ["treer-agent-server.exe", "treer.exe"] {
                assert_eq!(std::fs::read(installed.join(name)).unwrap(), OLD);
            }
            assert!(installed.join(".treer-controller-update.claim").exists());
        }
    }

    #[test]
    fn windows_update_pair_native_stale_candidates_or_evidence_never_authorize_publication() {
        for case in 0..3 {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let fixture = Fixture::new(|_| {});
            let (ready, backup) = pair_ready(&root, &installed, &target, &fixture);
            let stale = match case {
                0 => installed.join(".treer-update-cli.new"),
                1 => backup.join("publication.evidence"),
                _ => backup.join("restoration.evidence"),
            };
            std::fs::write(&stale, b"prior unknown evidence").unwrap();
            treer_platform::secure_file(&stale).unwrap();
            assert!(ready.publish_pair().is_err());
            assert_eq!(std::fs::read(&stale).unwrap(), b"prior unknown evidence");
            assert_eq!(
                installed.join(".treer-update-controller.new").exists(),
                case == 0
            );
            for name in ["treer-agent-server.exe", "treer.exe"] {
                assert_eq!(std::fs::read(installed.join(name)).unwrap(), OLD);
            }
            let job = root.directory("new-job");
            assert!(prepare(&installed, &target, &fixture)
                .unwrap()
                .reserve(UpdateStore::open(&job).unwrap())
                .is_err());
        }
    }

    #[test]
    fn windows_update_pair_native_partial_restore_is_unknown_and_never_replayed() {
        use std::io::Write;
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        // Exercise the platform kernel's real second-restore refusal. This is
        // not an authenticated Core operation or runtime health fixture.
        let locked = PinnedPair::open(&installed)
            .unwrap()
            .lock_installation()
            .unwrap();
        locked.claim(b"synthetic platform-only claim").unwrap();
        let mut staging = locked.stage_pair().unwrap();
        for (file, bytes) in staging.files().into_iter().zip([NEW, NEW, OLD, OLD]) {
            file.write_all(bytes).unwrap();
        }
        let published = staging.seal().unwrap().publish().unwrap();
        let held_cli = published.files().1.try_clone().unwrap();
        assert!(published.restore_files().is_err());
        drop(held_cli);
        assert_eq!(
            std::fs::read(installed.join("treer-agent-server.exe")).unwrap(),
            OLD
        );
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), NEW);
        assert_eq!(
            std::fs::read(installed.join(".treer-controller-update.claim")).unwrap(),
            b"synthetic platform-only claim"
        );
        assert!(installed.join(".treer-update-cli.restore").exists());
    }

    #[test]
    fn windows_update_startup_native_real_store_snapshots_preserve_fences_and_hold_backups() {
        use treer_platform::windows_update_backup::BackupDirectory;
        for mode in 0..4 {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let job = root.directory("job");
            let destination = root.directory("backup");
            let state = root.directory("state");
            let source = state.join("srv_fixture-agent-startup.json");
            let store = crate::startup_store::StartupStore::load(source.clone()).unwrap();
            if mode == 1 || mode == 3 {
                let entry = serde_json::from_value(json!({
                    "agent_id":"ag_kept","server_id":"srv_fixture","workspace_id":"ws_fixture",
                    "generation":1,"kind":"shell","name":"fixture","cwd":"private-cwd",
                    "command":"fixture","args":["private-argument"],"enabled":false,
                    "workload_credential":"private-credential","last_host_epoch":"fixture"
                }))
                .unwrap();
                store.upsert(entry).unwrap();
            }
            if mode >= 2 {
                store.deny_absent("ag_deleted").unwrap();
            }
            let original = std::fs::read(&source).ok();
            let fixture = Fixture::new(|_| {});
            let images = prepare(&installed, &target, &fixture)
                .unwrap()
                .reserve(UpdateStore::open(&job).unwrap())
                .unwrap()
                .backup(BackupDirectory::open(&destination).unwrap())
                .unwrap();
            let expected_binding = images.binding().clone();
            let backed = images
                .backup_startup(BackupDirectory::open(&state).unwrap())
                .unwrap();
            backed.inspect_startup().unwrap();
            assert_eq!(backed.binding(), &expected_binding);
            assert!(prepare(&installed, &target, &fixture).is_err());
            if original.is_some() {
                // Exclusive same-object pin also refuses path-based readers.
                // Core inspection above already read the held handle fully.
                assert!(std::fs::read(destination.join("startup.backup")).is_err());
                assert!(std::fs::OpenOptions::new()
                    .write(true)
                    .open(destination.join("startup.backup"))
                    .is_err());
                assert!(std::fs::rename(
                    destination.join("startup.backup"),
                    destination.join("moved")
                )
                .is_err());
            } else {
                assert!(!destination.join("startup.backup").exists());
            }
            // Capture releases the source file pin. A legitimate later write
            // succeeds and is never restored over or mistaken for corruption.
            store.deny_absent("ag_later").unwrap();
            backed.inspect_startup().unwrap();
            assert!(store.is_denied("ag_later").unwrap());
            let evidence_path = destination.join("startup.evidence");
            let safe = std::fs::read_to_string(&evidence_path).unwrap();
            assert!(!safe.contains("private-credential"));
            assert!(!safe.contains("ag_deleted"));
            let mut evidence: serde_json::Value = serde_json::from_str(&safe).unwrap();
            assert_eq!(evidence["phase"], "startup_backed_up");
            assert_eq!(
                evidence["binding"],
                serde_json::to_value(expected_binding).unwrap()
            );
            evidence["state_directory"] = json!([0, 0]);
            std::fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap()).unwrap();
            assert!(backed.inspect_startup().is_err());
            assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
            drop(backed);
            assert_eq!(
                std::fs::read(destination.join("startup.backup")).ok(),
                original
            );
            if original.is_some() {
                let captured =
                    crate::startup_store::StartupStore::load(destination.join("startup.backup"))
                        .unwrap();
                assert_eq!(captured.is_denied("ag_deleted").unwrap(), mode >= 2);
                assert!(!captured.is_denied("ag_later").unwrap());
            }
        }
    }

    #[test]
    fn windows_update_startup_native_refuses_unsafe_partial_or_cross_scope_without_evidence() {
        use treer_platform::windows_update_backup::BackupDirectory;
        for case in 0..8 {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let job = root.directory("job");
            let destination = root.directory("backup");
            let state = root.directory("state");
            let source = state.join("srv_fixture-agent-startup.json");
            let store = crate::startup_store::StartupStore::load(source.clone()).unwrap();
            store.deny_absent("ag_deleted").unwrap();
            let writer = (case == 7).then(|| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&source)
                    .unwrap()
            });
            match case {
                0 => std::fs::write(&source, b"private-invalid-json").unwrap(),
                1 => {
                    std::fs::hard_link(&source, state.join("alias")).unwrap();
                }
                2 => treer_platform::apply_sddl_dacl(&source, "D:P(A;;GA;;;WD)").unwrap(),
                3 => {
                    std::fs::write(destination.join("startup.backup"), b"prior").unwrap();
                }
                4 => {
                    std::fs::write(destination.join("startup.evidence"), b"prior").unwrap();
                }
                5 => {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(&source)
                        .unwrap()
                        .set_len(16 * 1024 * 1024 + 1)
                        .unwrap();
                }
                _ => {}
            }
            let fixture = Fixture::new(|_| {});
            let images = prepare(&installed, &target, &fixture)
                .unwrap()
                .reserve(UpdateStore::open(&job).unwrap())
                .unwrap()
                .backup(BackupDirectory::open(&destination).unwrap())
                .unwrap();
            let source_dir = if case == 6 { &destination } else { &state };
            assert!(images
                .backup_startup(BackupDirectory::open(source_dir).unwrap())
                .is_err());
            if case == 4 {
                assert_eq!(
                    std::fs::read(destination.join("startup.evidence")).unwrap(),
                    b"prior"
                );
            } else {
                assert!(!destination.join("startup.evidence").exists());
            }
            if case == 3 {
                assert_eq!(
                    std::fs::read(destination.join("startup.backup")).unwrap(),
                    b"prior"
                );
            }
            let again = prepare(&installed, &target, &fixture).unwrap();
            assert!(again.reserve(UpdateStore::open(&job).unwrap()).is_err());
            assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
            drop(writer);
        }
    }

    fn prepare_operation(
        installed: &Path,
        target: &Path,
        fixture: &Fixture,
        operation: uuid::Uuid,
    ) -> Result<AdmittedInputs> {
        AdmittedInputs::prepare(
            PinnedPair::open(installed)?.lock_installation()?,
            PinnedPair::open(target)?,
            &fixture.documents(),
            &fixture.key.verifying_key(),
            operation,
            "ws_fixture".into(),
            "srv_fixture".into(),
        )
    }

    #[test]
    fn windows_update_admission_native_pins_inputs_and_reserves_without_replacement() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job = root.directory("job");
        let fixture = Fixture::new(|_| {});
        let pinned_old = PinnedPair::open(&installed).unwrap();
        let pinned_target = PinnedPair::open(&target).unwrap();
        // Prior inspection must not make admission hash only a file suffix.
        use std::io::{Seek, SeekFrom};
        for pair in [&pinned_old, &pinned_target] {
            let (mut controller, mut cli) = pair.files();
            controller.seek(SeekFrom::End(0)).unwrap();
            cli.seek(SeekFrom::End(0)).unwrap();
        }
        let inputs = AdmittedInputs::prepare(
            pinned_old.lock_installation().unwrap(),
            pinned_target,
            &fixture.documents(),
            &fixture.key.verifying_key(),
            uuid::Uuid::new_v4(),
            "ws_fixture".into(),
            "srv_fixture".into(),
        )
        .unwrap();
        assert_eq!(
            (
                inputs.binding().installation_volume,
                inputs.binding().installation_file_id
            ),
            PinnedPair::open(&installed).unwrap().identity().unwrap()
        );
        for directory in [&installed, &target] {
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(directory.join("treer.exe"))
                .is_err());
            assert!(
                std::fs::rename(directory.join("treer.exe"), directory.join("moved.exe")).is_err()
            );
        }
        let reserved = inputs.reserve(UpdateStore::open(&job).unwrap()).unwrap();
        crate::windows_update_intent::inspect(
            &UpdateStore::open(&job).unwrap().read().unwrap(),
            reserved.binding(),
        )
        .unwrap();
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
        assert_eq!(std::fs::read(target.join("treer.exe")).unwrap(), NEW);
        drop(reserved);
    }

    #[test]
    fn windows_update_admission_native_refuses_unsafe_or_corrupt_inputs_without_intent() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", b"tampered");
        let job = root.directory("job");
        let fixture = Fixture::new(|_| {});
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert!(prepare(&installed, &installed, &fixture).is_err());
        assert_eq!(std::fs::read_dir(&job).unwrap().count(), 0);
        std::fs::hard_link(target.join("treer.exe"), target.join("alias.exe")).unwrap();
        assert!(PinnedPair::open(&target).is_err());
        treer_platform::apply_sddl_dacl(&installed, "D:P(A;;GA;;;WD)").unwrap();
        assert!(PinnedPair::open(&installed).is_err());
        assert_eq!(std::fs::read_dir(&job).unwrap().count(), 0);
    }

    #[test]
    fn windows_update_lock_native_excludes_distinct_jobs_through_reservation() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job_a = root.directory("job-a");
        let job_b = root.directory("job-b");
        let fixture = Fixture::new(|_| {});
        let inputs = prepare(&installed, &target, &fixture).unwrap();
        let lock = installed.join(".treer-controller-update.lock");
        assert!(treer_platform::private_path_is_secure(&lock).unwrap());
        // Different operation roots do not bypass the physical installation
        // lease. Reopening the pair itself remains read-only and permissible.
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert!(std::fs::remove_file(&lock).is_err());
        assert!(std::fs::rename(&lock, installed.join("moved-lock")).is_err());
        assert!(std::fs::OpenOptions::new().write(true).open(&lock).is_err());
        let reserved = inputs.reserve(UpdateStore::open(&job_a).unwrap()).unwrap();
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert_eq!(std::fs::read_dir(&job_b).unwrap().count(), 0);
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
        assert_eq!(std::fs::read(target.join("treer.exe")).unwrap(), NEW);
        drop(reserved);
        assert!(lock.exists());
        assert_eq!(std::fs::read(&lock).unwrap(), b"");
        // This demonstrates OS lease release, NOT permission to resume/retry
        // either operation. Existing job-a intent must still refuse resubmit.
        let inputs = prepare(&installed, &target, &fixture).unwrap();
        assert!(inputs.reserve(UpdateStore::open(&job_a).unwrap()).is_err());
        assert_eq!(std::fs::read_dir(&job_b).unwrap().count(), 0);
        assert!(lock.exists());
    }

    #[test]
    fn windows_update_lock_native_refuses_unsafe_lock_without_repair_or_intent() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job = root.directory("job");
        let fixture = Fixture::new(|_| {});
        let lock = installed.join(".treer-controller-update.lock");
        std::fs::write(&lock, b"retained evidence").unwrap();
        treer_platform::secure_file(&lock).unwrap();
        let alias = installed.join("lock-alias");
        std::fs::hard_link(&lock, &alias).unwrap();
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), b"retained evidence");
        std::fs::remove_file(&alias).unwrap();
        treer_platform::apply_sddl_dacl(&lock, "D:P(A;;GA;;;WD)").unwrap();
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert!(!treer_platform::private_path_is_secure(&lock).unwrap());
        assert_eq!(std::fs::read(&lock).unwrap(), b"retained evidence");
        assert_eq!(std::fs::read_dir(&job).unwrap().count(), 0);
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
    }

    #[test]
    fn windows_update_claim_survives_owner_exit_and_refuses_new_job_even_exact_binding() {
        const CHILD_ROOT: &str = "TREER_TEST_UPDATE_CLAIM_CHILD_ROOT";
        const CHILD_OPERATION: &str = "TREER_TEST_UPDATE_CLAIM_OPERATION";
        let fixture = Fixture::new(|_| {});
        if let Some(path) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(path);
            let operation = std::env::var(CHILD_OPERATION).unwrap().parse().unwrap();
            let inputs = prepare_operation(
                &root.join("installed"),
                &root.join("target"),
                &fixture,
                operation,
            )
            .unwrap();
            let _reserved = inputs
                .reserve(UpdateStore::open(&root.join("job-a")).unwrap())
                .unwrap();
            // Exit without Rust destructors: the kernel releases leases, but
            // both persisted records must still fence a fresh process owner.
            std::process::exit(0);
        }
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job_a = root.directory("job-a");
        let job_b = root.directory("job-b");
        let operation = uuid::Uuid::new_v4();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows_update_admission::tests::native::windows_update_claim_survives_owner_exit_and_refuses_new_job_even_exact_binding", "--nocapture"])
            .env(CHILD_ROOT, &root.0).env(CHILD_OPERATION, operation.to_string())
            .output().unwrap();
        assert!(child.status.success());
        let claim = installed.join(".treer-controller-update.claim");
        let bytes = std::fs::read(&claim).unwrap();
        let same = prepare_operation(&installed, &target, &fixture, operation).unwrap();
        same.inspect_installation_claim().unwrap();
        crate::windows_update_intent::inspect(
            &UpdateStore::open(&job_a).unwrap().read().unwrap(),
            same.binding(),
        )
        .unwrap();
        assert!(same.reserve(UpdateStore::open(&job_b).unwrap()).is_err());
        let different = prepare(&installed, &target, &fixture).unwrap();
        assert!(different.inspect_installation_claim().is_err());
        assert!(different
            .reserve(UpdateStore::open(&job_b).unwrap())
            .is_err());
        assert_eq!(std::fs::read(&claim).unwrap(), bytes);
        assert!(!job_b.join("receipt.bin").exists());
        assert!(!job_b.join("receipt.next").exists());
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
    }

    #[test]
    fn windows_update_claim_job_refusal_retains_claim_and_blocks_other_job() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job_a = root.directory("job-a");
        let job_b = root.directory("job-b");
        let fixture = Fixture::new(|_| {});
        UpdateStore::open(&job_a)
            .unwrap()
            .publish(b"prior evidence")
            .unwrap();
        let inputs = prepare(&installed, &target, &fixture).unwrap();
        let binding = inputs.binding().clone();
        assert!(inputs.reserve(UpdateStore::open(&job_a).unwrap()).is_err());
        let bytes = std::fs::read(installed.join(".treer-controller-update.claim")).unwrap();
        crate::windows_update_intent::inspect(&bytes, &binding).unwrap();
        assert_eq!(
            UpdateStore::open(&job_a).unwrap().read().unwrap(),
            b"prior evidence"
        );
        assert!(prepare(&installed, &target, &fixture)
            .unwrap()
            .reserve(UpdateStore::open(&job_b).unwrap())
            .is_err());
        assert!(!job_b.join("receipt.bin").exists());
        assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
    }

    #[test]
    fn windows_update_claim_partial_or_unsafe_record_is_never_repaired_or_adopted() {
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job = root.directory("job");
        let fixture = Fixture::new(|_| {});
        let claim = installed.join(".treer-controller-update.claim");
        let locked = PinnedPair::open(&installed)
            .unwrap()
            .lock_installation()
            .unwrap();
        assert!(locked.claim(b"").is_err());
        assert!(locked.claim(&vec![0; 4097]).is_err());
        assert!(!claim.exists());
        drop(locked);
        std::fs::write(&claim, b"partial claim").unwrap();
        treer_platform::secure_file(&claim).unwrap();
        let input = prepare(&installed, &target, &fixture).unwrap();
        assert!(input.inspect_installation_claim().is_err());
        assert!(input.reserve(UpdateStore::open(&job).unwrap()).is_err());
        std::fs::hard_link(&claim, installed.join("claim-alias")).unwrap();
        let input = prepare(&installed, &target, &fixture).unwrap();
        assert!(input.inspect_installation_claim().is_err());
        assert!(input.reserve(UpdateStore::open(&job).unwrap()).is_err());
        std::fs::remove_file(installed.join("claim-alias")).unwrap();
        treer_platform::apply_sddl_dacl(&claim, "D:P(A;;GA;;;WD)").unwrap();
        let input = prepare(&installed, &target, &fixture).unwrap();
        assert!(input.inspect_installation_claim().is_err());
        assert!(input.reserve(UpdateStore::open(&job).unwrap()).is_err());
        assert!(!treer_platform::private_path_is_secure(&claim).unwrap());
        assert_eq!(std::fs::read(&claim).unwrap(), b"partial claim");
        assert!(!job.join("receipt.bin").exists());
        assert!(!job.join("receipt.next").exists());
        assert_eq!(std::fs::read(target.join("treer.exe")).unwrap(), NEW);
    }

    #[test]
    fn windows_update_backup_native_copies_old_pair_and_binds_evidence_without_replacement() {
        use std::io::Write;
        use treer_platform::windows_update_backup::BackupDirectory;
        let root = Root::new();
        let installed = root.pair("installed", OLD);
        let target = root.pair("target", NEW);
        let job = root.directory("job");
        let destination = root.directory("backup");
        let fixture = Fixture::new(|_| {});
        let inputs = prepare(&installed, &target, &fixture).unwrap();
        let expected = inputs.binding().clone();
        let reserved = inputs.reserve(UpdateStore::open(&job).unwrap()).unwrap();
        let backed = reserved
            .backup(BackupDirectory::open(&destination).unwrap())
            .unwrap();
        backed.inspect_backups().unwrap();
        assert_eq!(backed.binding(), &expected);
        let receipt_path = destination.join("backup.evidence");
        let receipt = std::fs::read(&receipt_path).unwrap();
        let json = std::str::from_utf8(&receipt).unwrap();
        let mut wrong_identity: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        wrong_identity["directory_file_id"] = serde_json::json!(0);
        // Own disposable evidence only: the strict observer must refuse a
        // forged phase, duplicate/unknown key or a different physical identity.
        for invalid in [
            json.replace("images_backed_up", "images_published"),
            json.replacen('{', "{\"schema_version\":1,", 1),
            json.replacen('{', "{\"untrusted\":true,", 1),
            wrong_identity.to_string(),
        ] {
            std::fs::write(&receipt_path, invalid).unwrap();
            assert!(backed.inspect_backups().is_err());
        }
        std::fs::write(&receipt_path, &receipt).unwrap();
        backed.inspect_backups().unwrap();
        assert!(prepare(&installed, &target, &fixture).is_err());
        assert!(BackupDirectory::open(&destination)
            .unwrap()
            .create()
            .is_err());
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(destination.join("controller.backup"))
            .is_err());
        assert_eq!(
            std::fs::read(installed.join("treer-agent-server.exe")).unwrap(),
            OLD
        );
        assert_eq!(std::fs::read(target.join("treer.exe")).unwrap(), NEW);
        drop(backed);
        for file in ["controller.backup", "cli.backup"] {
            assert_eq!(std::fs::read(destination.join(file)).unwrap(), OLD);
            assert!(treer_platform::private_path_is_secure(&destination.join(file)).unwrap());
        }
        let evidence: serde_json::Value =
            serde_json::from_slice(&std::fs::read(destination.join("backup.evidence")).unwrap())
                .unwrap();
        assert_eq!(evidence["phase"], "images_backed_up");
        assert_eq!(evidence["binding"], serde_json::to_value(expected).unwrap());
        assert_eq!(evidence["controller_size"], OLD.len());
        assert_eq!(evidence["cli_size"], OLD.len());
        let (volume, file_id) = BackupDirectory::open(&destination)
            .unwrap()
            .identity()
            .unwrap();
        assert_eq!(evidence["directory_volume"], volume);
        assert_eq!(evidence["directory_file_id"], file_id);
        // Exercise reduced access on the same file object independently of
        // the Controller evidence: even the borrowed sealed File cannot write.
        let direct = root.directory("read-only-check");
        let mut writing = BackupDirectory::open(&direct).unwrap().create().unwrap();
        let (controller, cli) = writing.files();
        controller.write_all(OLD).unwrap();
        cli.write_all(OLD).unwrap();
        let sealed = writing.seal().unwrap();
        let (mut controller, mut cli) = sealed.files();
        assert!(controller.write_all(b"tamper").is_err());
        assert!(cli.write_all(b"tamper").is_err());
        drop(sealed);
    }

    #[test]
    fn windows_update_backup_native_refuses_alias_unsafe_or_partial_destination_without_success() {
        use treer_platform::windows_update_backup::BackupDirectory;
        for case in 0..5 {
            let root = Root::new();
            let installed = root.pair("installed", OLD);
            let target = root.pair("target", NEW);
            let job = root.directory("job");
            let destination = match case {
                0 => installed.clone(),
                1 => target.clone(),
                _ => root.directory("backup"),
            };
            let fixture = Fixture::new(|_| {});
            let reserved = prepare(&installed, &target, &fixture)
                .unwrap()
                .reserve(UpdateStore::open(&job).unwrap())
                .unwrap();
            if case == 2 {
                treer_platform::apply_sddl_dacl(&destination, "D:P(A;;GA;;;WD)").unwrap();
                assert!(BackupDirectory::open(&destination).is_err());
                assert!(!destination.join("controller.backup").exists());
                drop(reserved);
            } else {
                if case == 3 {
                    std::fs::write(destination.join("cli.backup"), b"prior evidence").unwrap();
                    treer_platform::secure_file(&destination.join("cli.backup")).unwrap();
                }
                if case == 4 {
                    std::fs::write(destination.join("backup.evidence"), b"prior evidence").unwrap();
                    treer_platform::secure_file(&destination.join("backup.evidence")).unwrap();
                }
                assert!(reserved
                    .backup(BackupDirectory::open(&destination).unwrap())
                    .is_err());
                if case == 3 {
                    assert_eq!(
                        std::fs::read(destination.join("cli.backup")).unwrap(),
                        b"prior evidence"
                    );
                    assert!(destination.join("controller.backup").exists());
                }
            }
            if case == 4 {
                assert_eq!(
                    std::fs::read(destination.join("backup.evidence")).unwrap(),
                    b"prior evidence"
                );
                assert!(!destination.join("controller.backup").exists());
            } else {
                assert!(!destination.join("backup.evidence").exists());
            }
            assert_eq!(std::fs::read(installed.join("treer.exe")).unwrap(), OLD);
            assert_eq!(std::fs::read(target.join("treer.exe")).unwrap(), NEW);
            let again = prepare(&installed, &target, &fixture).unwrap();
            assert!(again.reserve(UpdateStore::open(&job).unwrap()).is_err());
        }
    }
}
