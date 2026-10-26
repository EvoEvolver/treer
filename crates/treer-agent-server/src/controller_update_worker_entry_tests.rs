//! Same-source private dev image only. Never an installed image or Host.
use super::*;
use crate::windows_update_admission::{tests::Fixture, AdmittedInputs, ReservedInputs};
use crate::windows_update_worker_entry::{Trust, SWITCH};
use ed25519_dalek::pkcs8::EncodePublicKey;
use sha2::Digest;
use treer_platform::{windows_update_inputs::PinnedPair, windows_update_store::UpdateStore};

pub(super) fn binary() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_ENTRY_BINARY").unwrap());
    assert!(path.is_absolute() && path.is_file());
    path
}

fn reserve(
    runtime: &ControllerRuntime,
    root: &Path,
    catalog: &UpdatePlanCatalogFixture,
) -> (ReservedInputs, Fixture, Trust) {
    let bytes = std::fs::read(binary()).unwrap();
    assert!(!bytes.is_empty() && bytes.len() <= 128 * 1024 * 1024);
    let fixture = Fixture::with_target(&bytes, |_| {});
    let mut dispatch = update_confirmed_plan_binding();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    dispatch.target.identity.controller_sha256 = digest.clone();
    dispatch.target.identity.cli_sha256 = digest;
    dispatch.plan_digest = dispatch.expected_plan_digest().unwrap();
    catalog.write(&dispatch, false);
    for (name, data) in [
        ("installed", crate::windows_update_admission::tests::OLD),
        ("target", bytes.as_slice()),
    ] {
        let directory = root.join(name);
        treer_platform::secure_directory(&directory).unwrap();
        for name in ["treer-agent-server.exe", "treer.exe"] {
            let path = directory.join(name);
            std::fs::write(&path, data).unwrap();
            treer_platform::secure_file(&path).unwrap();
        }
    }
    let key = root.join("selfhost-public.pem");
    std::fs::write(
        &key,
        fixture
            .key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap(),
    )
    .unwrap();
    let trust = Trust::configured(
        &url::Url::parse("https://selfhost.example/").unwrap(),
        Some(&key),
        &root.join("absent.pem"),
    )
    .unwrap();
    let admitted = AdmittedInputs::prepare_confirmed_for_controller(
        PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .unwrap(),
        PinnedPair::open(&root.join("target")).unwrap(),
        &fixture.documents(),
        &fixture.key.verifying_key(),
        runtime,
        dispatch,
    )
    .unwrap();
    let job = root.join("job");
    treer_platform::secure_directory(&job).unwrap();
    (
        admitted.reserve(UpdateStore::open(&job).unwrap()).unwrap(),
        fixture,
        trust,
    )
}

pub(super) fn held(root: &Path) {
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer-agent-server.exe"))
        .is_err());
    assert!(PinnedPair::open(&root.join("installed"))
        .unwrap()
        .lock_installation()
        .is_err());
    assert!(std::fs::rename(root.join("configured-state"), root.join("moved-state")).is_err());
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    assert_eq!(
        std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
        crate::windows_update_admission::tests::OLD
    );
}

async fn exit(
    child: &mut ControllerUpdateChild,
    runtime: &ControllerRuntime,
    binding: &crate::windows_update_intent::Binding,
) -> std::process::ExitStatus {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.poll(runtime, binding).unwrap() {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "private entry exit deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn windows_update_worker_entry_native_configured_key_real_receive_retains_sender() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let (reserved, fixture, trust) = reserve(&runtime, &root, &catalog);
    let binding = reserved.binding().clone();
    let mut child = reserved
        .spawn_owned_input_receiver(&runtime, &trust)
        .unwrap_or_else(|_| panic!("configured receiver creation refused"));
    child
        .exchange_created_inputs(&runtime, &binding, &fixture.documents())
        .await
        .unwrap();
    assert!(child
        .exchange_created_inputs(&runtime, &binding, &fixture.documents())
        .await
        .is_err());
    assert!(exit(&mut child, &runtime, &binding).await.success());
    // A receipt and natural exit do not mark ready or release sender custody.
    assert!(child
        .wait_for_bound_child_exit(&runtime, &binding)
        .await
        .is_err());
    held(&root);
    child.stop(&runtime, &binding).unwrap();
    drop(child);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_entry_native_failed_trust_returns_custody_and_terminal_refusal() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let (reserved, _fixture, trust) = reserve(&runtime, &root, &catalog);
    // Configured file changes after selection. Never substitute offer/local/bundle.
    std::fs::remove_file(root.join("selfhost-public.pem")).unwrap();
    let (_, reserved) = match reserved.spawn_owned_input_receiver(&runtime, &trust) {
        Err(error) => error,
        Ok(_) => panic!("missing configured key accepted"),
    };
    held(&root);
    let (_, reserved) = match (*reserved).spawn_owned_input_receiver(&runtime, &trust) {
        Err(error) => error,
        Ok(_) => panic!("creation attempt replayed"),
    };
    held(&root);
    // Actual terminal binary refuses malformed argv or a missing key before
    // contacting any creator; it cannot fall through to normal Controller main.
    for args in [
        vec![],
        vec![
            Uuid::new_v4().to_string(),
            Uuid::new_v4().simple().to_string(),
            "--public-key".into(),
            root.join("missing.pem").to_str().unwrap().into(),
        ],
    ] {
        let state = runtime
            .windows_update_state_for_operation(Uuid::new_v4(), "ws_fixture", "srv_fixture")
            .unwrap();
        let binding = update_state_binding(state.scope().0);
        let mut command = std::process::Command::new(binary());
        command
            .arg(SWITCH)
            .args(args)
            .env_clear()
            .current_dir(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = update_state::ControllerUpdateChild::spawn_fixture(
            &state,
            &runtime,
            &binding,
            &mut command,
        )
        .unwrap();
        assert!(!exit(&mut child, &runtime, &binding).await.success());
        child.stop(&runtime, &binding).unwrap();
        drop(child);
        drop(state);
    }
    held(&root);
    drop(reserved);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}
