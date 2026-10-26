use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{pkcs8::EncodePublicKey, Signer, SigningKey};
use serde_json::json;
use std::io::Cursor;

const NEW: &[u8] = b"signed target fixture, never executed";
const OLD: &[u8] = b"installed fixture, never executed";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Fixture {
    key: SigningKey,
    channel: Vec<u8>,
    channel_signature: Vec<u8>,
    manifest: Vec<u8>,
    manifest_signature: Vec<u8>,
}

impl Fixture {
    fn new(mutate: impl FnOnce(&mut serde_json::Value)) -> Self {
        let key = SigningKey::from_bytes(&[35; 32]); // synthetic test authority only
        let key_id =
            hash(key.verifying_key().to_public_key_der().unwrap().as_bytes())[..16].to_owned();
        let mut manifest = json!({
            "schema_version":2, "version":"v0.1.16", "signing_key_id":key_id,
            "artifacts":[
                {"binary":"treer-agent-server","platform":"windows-x86_64",
                 "path":"releases/v0.1.16/windows-x86_64/treer-agent-server",
                 "size":NEW.len(),"sha256":hash(NEW)},
                {"binary":"treer","platform":"windows-x86_64",
                 "path":"releases/v0.1.16/windows-x86_64/treer",
                 "size":NEW.len(),"sha256":hash(NEW)}
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

    fn documents(&self) -> Documents<'_> {
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
