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
        AdmittedInputs::prepare(
            PinnedPair::open(installed)?,
            PinnedPair::open(target)?,
            &fixture.documents(),
            &fixture.key.verifying_key(),
            uuid::Uuid::new_v4(),
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
            pinned_old,
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
}
