//! Real same-source receiver in a private installation; no installed process.
use super::*;
use crate::controller_update::windows::coordinator::Coordinator;
use crate::windows_update_admission::tests::Fixture;
use crate::windows_update_worker_entry::Trust;
use ed25519_dalek::pkcs8::EncodePublicKey;
use sha2::Digest;
use treer_platform::{windows_update_inputs::PinnedPair, windows_update_store::UpdateStore};

fn prepare(
    runtime: &ControllerRuntime,
    root: &Path,
    catalog: &UpdatePlanCatalogFixture,
) -> (Coordinator, Fixture) {
    let bytes = std::fs::read(super::worker_entry::binary()).unwrap();
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
    let job = root.join("job");
    treer_platform::secure_directory(&job).unwrap();
    let coordinator = Coordinator::prepare(
        runtime,
        dispatch,
        PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .unwrap(),
        PinnedPair::open(&root.join("target")).unwrap(),
        &fixture.documents(),
        UpdateStore::open(&job).unwrap(),
        trust,
    )
    .unwrap();
    (coordinator, fixture)
}

#[tokio::test]
async fn windows_update_coordinator_native_successful_hold_inspect_close_moves_original_custody() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let (mut coordinator, fixture) = prepare(&runtime, &root, &catalog);
    assert!(coordinator.retain_closed(&runtime).is_err());
    coordinator
        .start(&runtime, &fixture.documents())
        .await
        .unwrap();
    for _ in 0..2 {
        coordinator.inspect(&runtime).await.unwrap();
        super::worker_entry::held(&root);
    }
    coordinator.close(&runtime).await.unwrap();
    super::worker_entry::held(&root);
    let reserved = coordinator.retain_closed(&runtime).unwrap();
    super::worker_entry::held(&root);
    assert!(coordinator.retain_closed(&runtime).is_err());
    assert!(coordinator.inspect(&runtime).await.is_err());
    drop(reserved);
    drop(coordinator);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_coordinator_native_changed_runtime_state_poison_keeps_sender_claim() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let (mut coordinator, fixture) = prepare(&runtime, &root, &catalog);
    coordinator
        .start(&runtime, &fixture.documents())
        .await
        .unwrap();
    coordinator.inspect(&runtime).await.unwrap();
    runtime
        .inner
        .startup_store
        .deny_absent("ag_changed")
        .unwrap();
    assert!(coordinator.inspect(&runtime).await.is_err());
    assert!(coordinator.inspect(&runtime).await.is_err());
    assert!(coordinator.close(&runtime).await.is_err());
    assert!(coordinator
        .start(&runtime, &fixture.documents())
        .await
        .is_err());
    assert!(coordinator.retain_closed(&runtime).is_err());
    super::worker_entry::held(&root);
    drop(coordinator);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}
