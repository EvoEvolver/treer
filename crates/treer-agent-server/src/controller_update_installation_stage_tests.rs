//! Only private saved-config fixtures and same-source signed dev images.
use super::*;
use crate::{
    controller_update::windows::preparation::Preparation,
    windows_update_admission::tests::{Fixture, OLD},
};
use ed25519_dalek::pkcs8::EncodePublicKey;
use sha2::Digest;

const WORKER_CANDIDATES: [&str; 4] = [
    ".treer-update-controller.new",
    ".treer-update-cli.new",
    ".treer-update-controller.restore",
    ".treer-update-cli.restore",
];

#[tokio::test]
async fn windows_update_worker_publication_same_holding_publishes_executable_pe_and_phase() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, true);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &["srv_other".into()]).await;
    for (name, expected) in WORKER_CANDIDATES.into_iter().zip([
        input.bytes.as_slice(),
        input.bytes.as_slice(),
        OLD,
        OLD,
    ]) {
        let path = root.join("installed").join(name);
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
    }
    prepared.transfer_custody(&runtime).await.unwrap();
    prepared.release_sender_images(&runtime).unwrap();
    prepared.publish_worker_pair(&runtime).await.unwrap();
    for name in ["treer-agent-server.exe", "treer.exe"] {
        let path = root.join("installed").join(name);
        assert_eq!(std::fs::read(&path).unwrap(), input.bytes);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        // Actual task-owned signed PE mapping/execution under receiver pins.
        // --version is terminal; this starts neither Host nor Controller.
        let mut child = std::process::Command::new(&path)
            .arg("--version")
            .env_clear()
            .current_dir(root.join("installed"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if tokio::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("task PE deadline");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert!(status.success());
    }
    let job = stage(&root, &input).join("job");
    let started: serde_json::Value =
        serde_json::from_slice(&std::fs::read(job.join("worker-pair-started.bin")).unwrap())
            .unwrap();
    let published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(job.join("worker-pair-published.bin")).unwrap())
            .unwrap();
    assert_eq!(published["phase"], "worker_pair_published");
    assert_eq!(started["old_physical_ids"], published["old_physical_ids"]);
    assert_ne!(started["old_physical_ids"], published["new_physical_ids"]);
    assert!(runtime.inner.startup_store.deny_absent("ag_later").is_err());
    prepared.inspect(&runtime).await.unwrap();
    prepared.close(&runtime).await.unwrap();
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_publication_without_explicit_release_refuses_before_phase() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    prepared.transfer_custody(&runtime).await.unwrap();
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    assert!(!stage(&root, &input)
        .join("job/worker-pair-started.bin")
        .exists());
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    held(&root, &input);
    prepared.fixture_stop(&runtime).unwrap();
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_publication_second_locked_image_keeps_mixed_unknown_guards() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    prepared.transfer_custody(&runtime).await.unwrap();
    prepared.release_sender_images(&runtime).unwrap();
    use std::os::windows::fs::OpenOptionsExt;
    let reader = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.join("installed/treer.exe"))
        .unwrap();
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    assert_eq!(
        std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
        input.bytes
    );
    assert_eq!(
        std::fs::read(root.join("installed/treer.exe")).unwrap(),
        OLD
    );
    let job = stage(&root, &input).join("job");
    assert!(job.join("worker-pair-started.bin").exists());
    assert!(!job.join("worker-pair-published.bin").exists());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("installed/treer-agent-server.exe"))
        .is_err());
    assert!(runtime.inner.startup_store.deny_absent("ag_later").is_err());
    drop(reader);
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    assert!(prepared.close(&runtime).await.is_err());
    prepared.fixture_live(&runtime).unwrap();
    prepared.fixture_stop(&runtime).unwrap();
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_publication_existing_phase_refuses_without_replacing_pair() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    prepared.transfer_custody(&runtime).await.unwrap();
    prepared.release_sender_images(&runtime).unwrap();
    let phase = stage(&root, &input).join("job/worker-pair-started.bin");
    std::fs::write(&phase, b"unresolved").unwrap();
    treer_platform::secure_file(&phase).unwrap();
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    assert_eq!(std::fs::read(&phase).unwrap(), b"unresolved");
    held(&root, &input);
    assert!(prepared.publish_worker_pair(&runtime).await.is_err());
    prepared.fixture_stop(&runtime).unwrap();
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

async fn transfer_prepared(
    runtime: &ControllerRuntime,
    root: &Path,
    input: &Inputs,
    shared: &[String],
) -> Preparation {
    let mut prepared = Preparation::local(
        runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        shared,
    )
    .unwrap();
    prepared
        .start(runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.prepare_worker_backup(runtime).await.unwrap();
    let raw = std::fs::read(root.join("configured-state/srv_fixture-agent-startup.json")).unwrap();
    prepared
        .compare_write_startup_fenced(runtime, Some(&raw))
        .await
        .unwrap();
    prepared.prepare_worker_candidates(runtime).await.unwrap();
    prepared
}

#[tokio::test]
async fn windows_update_transfer_same_holding_durable_record_then_explicit_all_sender_images() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, true);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &["srv_other".into()]).await;
    let installed = root.join("installed/treer-agent-server.exe");
    prepared.transfer_custody(&runtime).await.unwrap();
    // Even a real transfer ACK/durable record does not release sender images.
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&installed)
        .is_err());
    let record = stage(&root, &input).join("job/worker-transfer.bin");
    let accepted = std::fs::read(&record).unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&record)
        .is_err());
    prepared.release_sender_images(&runtime).unwrap();
    // Includes every saved inventory image pin, not just ReservedInputs.
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&installed)
            .unwrap(),
    );
    assert_eq!(std::fs::read(&installed).unwrap(), OLD);
    assert!(runtime.inner.startup_store.deny_absent("ag_later").is_err());
    held(&root, &input);
    prepared.inspect(&runtime).await.unwrap();
    prepared.close(&runtime).await.unwrap();
    assert!(prepared.retain_closed(&runtime).is_err());
    assert_eq!(std::fs::read(record).unwrap(), accepted);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_transfer_dead_original_child_never_releases_sender_images() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    prepared.transfer_custody(&runtime).await.unwrap();
    let record = stage(&root, &input).join("job/worker-transfer.bin");
    let raw = std::fs::read(&record).unwrap();
    // A genuine record/ACK plus ORIGINAL Child exit is still not release.
    prepared.fixture_stop(&runtime).unwrap();
    assert!(prepared.release_sender_images(&runtime).is_err());
    assert!(prepared.release_sender_images(&runtime).is_err());
    assert!(prepared.close(&runtime).await.is_err());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("installed/treer-agent-server.exe"))
        .is_err());
    assert_eq!(std::fs::read(record).unwrap(), raw);
    held(&root, &input);
    assert!(runtime.inner.startup_store.deny_absent("ag_later").is_err());
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_transfer_record_conflict_retains_original_pins_and_unknown() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    let record = stage(&root, &input).join("job/worker-transfer.bin");
    std::fs::write(&record, b"unresolved transfer").unwrap();
    assert!(prepared.transfer_custody(&runtime).await.is_err());
    assert!(prepared.transfer_custody(&runtime).await.is_err());
    assert!(prepared.release_sender_images(&runtime).is_err());
    assert!(prepared.close(&runtime).await.is_err());
    assert_eq!(std::fs::read(record).unwrap(), b"unresolved transfer");
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("installed/treer-agent-server.exe"))
        .is_err());
    held(&root, &input);
    assert!(runtime.inner.startup_store.deny_absent("ag_later").is_err());
    // Test-only teardown of the original owned child, not a retry or recovery.
    prepared.fixture_stop(&runtime).unwrap();
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_transfer_disconnected_owner_retains_worker_lifetime_and_lease() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = transfer_prepared(&runtime, &root, &input, &[]).await;
    prepared.transfer_custody(&runtime).await.unwrap();
    prepared.release_sender_images(&runtime).unwrap();
    prepared.fixture_disconnect_and_drop_sender();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    prepared.fixture_live(&runtime).unwrap();
    // Original sender lease/job owners are dropped. Only the original Child's
    // inherited/independent receiver custody keeps these locks and claim held.
    held(&root, &input);
    assert!(treer_platform::windows_update_store::UpdateStore::open(
        &stage(&root, &input).join("job")
    )
    .is_err());
    assert!(prepared.transfer_custody(&runtime).await.is_err());
    prepared.fixture_stop(&runtime).unwrap();
    drop(prepared);
    assert!(
        treer_platform::windows_update_inputs::PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .is_ok()
    );
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_mutation_fence_same_loaded_store_worker_cas_and_candidates() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let path = root.join("configured-state/srv_fixture-agent-startup.json");
    let original = std::fs::read(&path).unwrap();
    let mut next: serde_json::Value = serde_json::from_slice(&original).unwrap();
    next["denied_agent_ids"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("ag_fenced"));
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    prepared
        .compare_write_startup_fenced(&runtime, Some(&serde_json::to_vec(&next).unwrap()))
        .await
        .unwrap();
    assert!(runtime
        .inner
        .startup_store
        .clone()
        .deny_absent("ag_concurrent")
        .is_err());
    assert!(runtime.inner.startup_store.is_denied("ag_deleted").unwrap());
    assert!(runtime.inner.startup_store.is_denied("ag_fenced").unwrap());
    prepared.prepare_worker_candidates(&runtime).await.unwrap();
    prepared.inspect(&runtime).await.unwrap();
    held(&root, &input);
    let after = std::fs::read(&path).unwrap();
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    assert_eq!(
        std::fs::read(stage(&root, &input).join("worker-backup/startup.backup")).unwrap(),
        original
    );
    for (name, bytes) in WORKER_CANDIDATES.into_iter().zip([
        input.bytes.as_slice(),
        input.bytes.as_slice(),
        OLD,
        OLD,
    ]) {
        assert_eq!(
            std::fs::read(root.join("installed").join(name)).unwrap(),
            bytes
        );
    }
    drop(custody);
    drop(prepared);
    assert!(runtime
        .inner
        .startup_store
        .clone()
        .deny_absent("ag_after_drop")
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), after);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_mutation_fence_post_cas_unknown_never_releases_normal_writers() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let path = root.join("configured-state/srv_fixture-agent-startup.json");
    let original = std::fs::read(&path).unwrap();
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    prepared
        .compare_write_startup_fenced(&runtime, Some(&original))
        .await
        .unwrap();
    let blocker = root.join("installed").join(WORKER_CANDIDATES[1]);
    std::fs::write(&blocker, b"keep unresolved").unwrap();
    assert!(prepared.prepare_worker_candidates(&runtime).await.is_err());
    assert!(prepared.prepare_worker_candidates(&runtime).await.is_err());
    assert!(prepared.close(&runtime).await.is_err());
    held(&root, &input);
    assert_eq!(std::fs::read(&blocker).unwrap(), b"keep unresolved");
    drop(prepared);
    assert!(runtime
        .inner
        .startup_store
        .clone()
        .deny_absent("ag_after_unknown")
        .is_err());
    assert!(runtime.inner.startup_store.is_denied("ag_deleted").unwrap());
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_candidates_same_holding_full_new_restore_pair_and_custody() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    let installed = root.join("installed");
    for name in WORKER_CANDIDATES {
        assert!(!installed.join(name).exists());
    }
    let job = stage(&root, &input).join("job");
    let intent = std::fs::read(job.join("receipt.bin")).unwrap();
    prepared.prepare_worker_candidates(&runtime).await.unwrap();
    prepared.inspect(&runtime).await.unwrap();
    for (name, expected) in WORKER_CANDIDATES.into_iter().zip([
        input.bytes.as_slice(),
        input.bytes.as_slice(),
        OLD,
        OLD,
    ]) {
        let path = installed.join(name);
        assert!(path.is_file());
        // Compatible readers are permitted for the later identity bridge;
        // receiver still denies new writers/deleters and verifies all bytes.
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
    }
    held(&root, &input);
    assert_eq!(std::fs::read(job.join("receipt.bin")).unwrap(), intent);
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    for (name, expected) in WORKER_CANDIDATES.into_iter().zip([
        input.bytes.as_slice(),
        input.bytes.as_slice(),
        OLD,
        OLD,
    ]) {
        assert_eq!(std::fs::read(installed.join(name)).unwrap(), expected);
    }
    held(&root, &input);
    assert!(treer_platform::windows_update_store::UpdateStore::open(&job).is_err());
    assert!(prepared.prepare_worker_candidates(&runtime).await.is_err());
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_candidates_partial_destination_or_changed_state_keeps_unknown() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for changed_state in [false, true] {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let input = inputs(&root, &catalog, false);
        let mut prepared = Preparation::local(
            &runtime,
            input.dispatch.clone(),
            &input.settings,
            &input.fixture.documents(),
            (&input.bytes, &input.bytes),
            &[],
        )
        .unwrap();
        prepared
            .start(&runtime, &input.fixture.documents())
            .await
            .unwrap();
        prepared.prepare_worker_backup(&runtime).await.unwrap();
        let installed = root.join("installed");
        if changed_state {
            runtime.inner.startup_store.deny_absent("ag_later").unwrap();
        } else {
            // Real CREATE_NEW collision after one file was already created.
            std::fs::write(installed.join(WORKER_CANDIDATES[1]), b"unresolved").unwrap();
        }
        assert!(prepared.prepare_worker_candidates(&runtime).await.is_err());
        assert!(prepared.prepare_worker_candidates(&runtime).await.is_err());
        assert!(prepared.close(&runtime).await.is_err());
        assert!(prepared.retain_closed(&runtime).is_err());
        if changed_state {
            for name in WORKER_CANDIDATES {
                assert!(!installed.join(name).exists());
            }
        } else {
            assert!(installed.join(WORKER_CANDIDATES[0]).is_file());
            assert_eq!(
                std::fs::read(installed.join(WORKER_CANDIDATES[1])).unwrap(),
                b"unresolved"
            );
            assert!(!installed.join(WORKER_CANDIDATES[2]).exists());
        }
        held(&root, &input);
        drop(prepared);
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

#[tokio::test]
async fn windows_update_worker_job_same_holding_receiver_pins_intent_until_explicit_close() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    let job = stage(&root, &input).join("job");
    let intent = job.join("receipt.bin");
    let original = std::fs::read(&intent).unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&intent)
        .is_ok());
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    for _ in 0..2 {
        prepared.inspect(&runtime).await.unwrap();
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(&intent)
            .is_err());
        assert!(std::fs::remove_file(&intent).is_err());
        assert!(treer_platform::windows_update_store::UpdateStore::open(&job).is_err());
    }
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    prepared.inspect(&runtime).await.unwrap();
    assert_eq!(std::fs::read(&intent).unwrap(), original);
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    // Child exit drops the independent receipt pin; sender still owns its job
    // holder and all original guards. Neither fact grants product release.
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&intent)
        .is_ok());
    assert!(treer_platform::windows_update_store::UpdateStore::open(&job).is_err());
    assert_eq!(std::fs::read(&intent).unwrap(), original);
    held(&root, &input);
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_claim_same_receiver_pins_full_claim_until_explicit_close() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    let path = root.join("installed/.treer-controller-update.claim");
    let original = std::fs::read(&path).unwrap();
    // Sender reservation does not hold a claim file reader. This succeeds
    // before the real independently verified worker acquires its own pin.
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_ok());
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    for _ in 0..2 {
        prepared.inspect(&runtime).await.unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    prepared.inspect(&runtime).await.unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    held(&root, &input);
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_claim_missing_partial_foreign_record_never_receives() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for kind in 0..3 {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let input = inputs(&root, &catalog, false);
        let mut prepared = Preparation::local(
            &runtime,
            input.dispatch.clone(),
            &input.settings,
            &input.fixture.documents(),
            (&input.bytes, &input.bytes),
            &[],
        )
        .unwrap();
        let path = root.join("installed/.treer-controller-update.claim");
        let original = std::fs::read(&path).unwrap();
        if kind == 0 {
            std::fs::remove_file(&path).unwrap();
        } else if kind == 1 {
            std::fs::write(&path, b"partial").unwrap();
        } else {
            let mut value: serde_json::Value = serde_json::from_slice(&original[8..]).unwrap();
            value["binding"]["operation_id"] = serde_json::json!(uuid::Uuid::new_v4());
            let mut wrong = original[..8].to_vec();
            wrong.extend(serde_json::to_vec(&value).unwrap());
            std::fs::write(&path, wrong).unwrap();
        }
        assert!(prepared
            .start(&runtime, &input.fixture.documents())
            .await
            .is_err());
        assert!(prepared
            .start(&runtime, &input.fixture.documents())
            .await
            .is_err());
        assert!(prepared.inspect(&runtime).await.is_err());
        assert!(prepared.retain_closed(&runtime).is_err());
        if kind == 0 {
            assert!(!path.exists());
        } else {
            assert_ne!(std::fs::read(&path).unwrap(), original);
        }
        // No backup/output is repaired or adopted after an unreceived claim.
        assert!(!stage(&root, &input).join("worker-backup").exists());
        drop(prepared);
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

#[tokio::test]
async fn windows_update_startup_cas_native_loaded_store_commit_continues_same_worker() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let path = root.join("configured-state/srv_fixture-agent-startup.json");
    let original = std::fs::read(&path).unwrap();
    let mut next: serde_json::Value = serde_json::from_slice(&original).unwrap();
    next["denied_agent_ids"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("ag_cas"));
    let replacement = serde_json::to_vec(&next).unwrap();
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    prepared
        .compare_write_startup(&runtime, Some(&replacement))
        .await
        .unwrap();
    // BOTH the original loaded Arc and the SAME worker now accept the new state.
    assert!(runtime
        .inner
        .startup_store
        .clone()
        .is_denied("ag_cas")
        .unwrap());
    assert!(runtime.inner.startup_store.is_denied("ag_deleted").unwrap());
    prepared.inspect(&runtime).await.unwrap();
    held(&root, &input);
    let backup = stage(&root, &input).join("worker-backup/startup.backup");
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&backup)
        .is_err());
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    assert!(prepared
        .compare_write_startup(&runtime, Some(&original))
        .await
        .is_err());
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    let disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(disk["version"], 2);
    assert!(disk["denied_agent_ids"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("ag_cas")));
    held(&root, &input);
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_startup_cas_native_fence_regression_concurrent_write_or_missing_grant_refused(
) {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for kind in 0..3 {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let input = inputs(&root, &catalog, false);
        let path = root.join("configured-state/srv_fixture-agent-startup.json");
        let original = std::fs::read(&path).unwrap();
        let mut prepared = Preparation::local(
            &runtime,
            input.dispatch.clone(),
            &input.settings,
            &input.fixture.documents(),
            (&input.bytes, &input.bytes),
            &[],
        )
        .unwrap();
        prepared
            .start(&runtime, &input.fixture.documents())
            .await
            .unwrap();
        if kind != 2 {
            prepared.prepare_worker_backup(&runtime).await.unwrap();
        }
        if kind == 1 {
            runtime
                .inner
                .startup_store
                .clone()
                .deny_absent("ag_concurrent")
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let replacement = if kind == 0 {
            br#"{"version":1,"entries":[]}"#.as_slice()
        } else {
            original.as_slice()
        };
        assert!(prepared
            .compare_write_startup(&runtime, Some(replacement))
            .await
            .is_err());
        assert!(prepared
            .compare_write_startup(&runtime, Some(replacement))
            .await
            .is_err());
        assert!(prepared.close(&runtime).await.is_err());
        assert!(prepared.retain_closed(&runtime).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(runtime.inner.startup_store.is_denied("ag_deleted").unwrap());
        if kind == 1 {
            assert!(runtime
                .inner
                .startup_store
                .is_denied("ag_concurrent")
                .unwrap());
        }
        held(&root, &input);
        drop(prepared);
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

struct Inputs {
    bytes: Vec<u8>,
    fixture: Fixture,
    dispatch: treer_protocol::controller_update_runtime::DispatchBinding,
    settings: crate::UpdateArgs,
}
fn inputs(root: &Path, catalog: &UpdatePlanCatalogFixture, shared: bool) -> Inputs {
    let bytes = std::fs::read(super::worker_entry::binary()).unwrap();
    let fixture = Fixture::with_target(&bytes, |_| {});
    let mut dispatch = update_confirmed_plan_binding();
    dispatch.target.identity.controller_sha256 = format!("{:x}", sha2::Sha256::digest(&bytes));
    dispatch.target.identity.cli_sha256 = dispatch.target.identity.controller_sha256.clone();
    dispatch.plan_digest = dispatch.expected_plan_digest().unwrap();
    catalog.write(&dispatch, false);
    if shared {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(catalog.path()).unwrap()).unwrap();
        value["accept_shared_binaries"] = serde_json::json!(true);
        std::fs::write(catalog.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let installed = root.join("installed");
    treer_platform::secure_directory(&installed).unwrap();
    for name in ["treer-agent-server.exe", "treer.exe"] {
        let file = installed.join(name);
        std::fs::write(&file, OLD).unwrap();
        treer_platform::secure_file(&file).unwrap();
    }
    let config = crate::service::controller_update_config_directory().unwrap();
    let hostname = crate::service::current_hostname().unwrap();
    for (workspace, server) in [("ws_fixture", "srv_fixture"), ("ws_other", "srv_other")]
        .into_iter()
        .take(if shared { 2 } else { 1 })
    {
        let controller = serde_json::json!({"workspace":workspace,"server_id":server,"root":root,"listen":"127.0.0.1:8790","host_socket":"fixture-pipe","install_hostname":hostname,"machine_token":"fixture-secret-not-diagnostic"});
        let host = serde_json::json!({"root":root,"socket_path":"fixture-pipe","controller_path":installed.join("treer-agent-server.exe"),"controller_config_path":config.join(format!("{server}-controller.json"))});
        for (kind, value) in [("controller", controller), ("host", host)] {
            let file = config.join(format!("{server}-{kind}.json"));
            std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
            treer_platform::secure_file(&file).unwrap();
        }
    }
    let key = config.parent().unwrap().join("release-signing-key.pub.pem");
    std::fs::write(
        &key,
        fixture
            .key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap(),
    )
    .unwrap();
    treer_platform::secure_file(&key).unwrap();
    let settings = crate::UpdateArgs {
        release_base_url: url::Url::parse("https://selfhost.example/").unwrap(),
        channel: "stable".into(),
        public_key: None,
    };
    Inputs {
        bytes,
        fixture,
        dispatch,
        settings,
    }
}
fn stage(root: &Path, input: &Inputs) -> PathBuf {
    root.join("configured-state").join(format!(
        "controller-update-{}",
        Uuid::parse_str(&input.dispatch.operation_id)
            .unwrap()
            .simple()
    ))
}
fn held(root: &Path, input: &Inputs) {
    let config = crate::service::controller_update_config_directory().unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(config.join("srv_fixture-controller.json"))
        .is_err());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(stage(root, input).join("treer-agent-server.exe"))
        .is_err());
    assert!(
        treer_platform::windows_update_inputs::PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .is_err()
    );
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    assert_eq!(
        std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
        OLD
    );
}

#[tokio::test]
async fn windows_update_installation_stage_native_saved_shared_install_to_real_receiver_success() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, true);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &["srv_other".into()],
    )
    .unwrap();
    assert_eq!(prepared.shared().unwrap(), &["srv_other".to_string()]);
    held(&root, &input);
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.inspect(&runtime).await.unwrap();
    held(&root, &input);
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    held(&root, &input);
    assert_eq!(custody.reservation.binding().workspace_id, "ws_fixture");
    assert!(prepared.retain_closed(&runtime).is_err());
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_installation_stage_native_scope_consent_trust_binding_before_stage() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for case in 0..8 {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let mut input = inputs(&root, &catalog, true);
        let mut expected = vec!["srv_other".to_string()];
        match case {
            0 => input.dispatch.workspace_id = "ws_foreign".into(),
            1 => input.dispatch.server_id = "srv_foreign".into(),
            2 => expected.clear(),
            3 => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(catalog.path()).unwrap()).unwrap();
                value["accept_shared_binaries"] = serde_json::json!(false);
                std::fs::write(catalog.path(), serde_json::to_vec(&value).unwrap()).unwrap();
            }
            4 => input.settings.public_key = Some(root.join("missing-key.pem")),
            5 => input.bytes[0] ^= 1,
            6 => {
                let file = catalog
                    .path()
                    .parent()
                    .unwrap()
                    .join("srv_fixture-host.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
                value["controller_config_path"] = serde_json::json!(root.join("decoy.json"));
                std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => {
                let file = catalog
                    .path()
                    .parent()
                    .unwrap()
                    .join("srv_fixture-controller.json");
                treer_platform::apply_sddl_dacl(&file, "D:P(A;;GA;;;WD)").unwrap();
            }
        }
        assert!(
            Preparation::local(
                &runtime,
                input.dispatch.clone(),
                &input.settings,
                &input.fixture.documents(),
                (&input.bytes, &input.bytes),
                &expected
            )
            .is_err(),
            "case {case}"
        );
        assert!(!stage(&root, &input).exists(), "case {case}");
        assert!(!root
            .join("installed/.treer-controller-update.claim")
            .exists());
        assert_eq!(
            std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
            OLD
        );
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

#[tokio::test]
async fn windows_update_installation_stage_native_inventory_change_poison_no_stage_replay() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    let new_name = catalog
        .path()
        .parent()
        .unwrap()
        .join("srv_new-controller.json");
    std::fs::write(&new_name, b"{}").unwrap();
    assert!(prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .is_err());
    std::fs::remove_file(new_name).unwrap();
    assert!(prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .is_err());
    assert!(prepared.retain_closed(&runtime).is_err());
    held(&root, &input);
    drop(prepared);
    assert!(Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[]
    )
    .is_err());
    assert!(stage(&root, &input).exists());
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

async fn distributor(
    input: &Inputs,
    corrupt: bool,
) -> (
    url::Url,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{
        body::Body,
        http::{Request, Response},
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let docs = input.fixture.documents();
    let mut bytes = input.bytes.clone();
    if corrupt {
        bytes[0] ^= 1;
    }
    let objects = Arc::new(std::collections::HashMap::from([
        ("channels/stable.json", docs.channel.to_vec()),
        ("channels/stable.json.sig", docs.channel_signature.to_vec()),
        ("releases/v0.1.16/manifest.json", docs.manifest.to_vec()),
        (
            "releases/v0.1.16/manifest.json.sig",
            docs.manifest_signature.to_vec(),
        ),
        (
            "releases/v0.1.16/windows-x86_64/treer-agent-server",
            bytes.clone(),
        ),
        ("releases/v0.1.16/windows-x86_64/treer", bytes),
    ]));
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let app = Router::new().route(
        "/{*path}",
        any(move |request: Request<Body>| {
            let objects = objects.clone();
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                match objects.get(request.uri().path().trim_start_matches('/')) {
                    Some(bytes) => Response::new(Body::from(bytes.clone())),
                    None => Response::builder().status(404).body(Body::empty()).unwrap(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, requests, task)
}

#[tokio::test]
async fn windows_update_download_native_normal_network_to_receiver_retains_custody() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let mut input = inputs(&root, &catalog, true);
    let (base, requests, server) = distributor(&input, false).await;
    input.settings.release_base_url = base;
    let mut prepared = Preparation::download(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &["srv_other".into()],
    )
    .await
    .unwrap();
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 6);
    held(&root, &input);
    // No caller-provided documents: the receiver gets the original HTTP bytes.
    prepared.start_downloaded(&runtime).await.unwrap();
    prepared.inspect(&runtime).await.unwrap();
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    held(&root, &input);
    assert_eq!(custody.reservation.binding().workspace_id, "ws_fixture");
    assert!(prepared.start_downloaded(&runtime).await.is_err());
    drop(custody);
    drop(prepared);
    server.abort();
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_download_native_trust_before_network_and_corrupt_before_stage() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for corrupt in [false, true] {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let mut input = inputs(&root, &catalog, false);
        let (base, requests, server) = distributor(&input, corrupt).await;
        input.settings.release_base_url = base;
        if !corrupt {
            input.settings.public_key = Some(root.join("missing-public-key.pem"));
        }
        assert!(
            Preparation::download(&runtime, input.dispatch.clone(), &input.settings, &[])
                .await
                .is_err()
        );
        let count = requests.load(std::sync::atomic::Ordering::SeqCst);
        if !corrupt {
            assert_eq!(count, 0);
        } else {
            assert!((5..=6).contains(&count));
        }
        assert!(!stage(&root, &input).exists());
        assert!(!root
            .join("installed/.treer-controller-update.claim")
            .exists());
        assert_eq!(
            std::fs::read(root.join("installed/treer-agent-server.exe")).unwrap(),
            OLD
        );
        server.abort();
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

fn consumer_config(catalog: &UpdatePlanCatalogFixture, input: &Inputs, missing_key: bool) {
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(catalog.path()).unwrap()).unwrap();
    value["signed_release"] = serde_json::json!({
        "base_url":input.settings.release_base_url.as_str(), "channel":"stable",
        "public_key": if missing_key { Some(catalog.path().parent().unwrap().join("missing.pem")) } else { None },
        "confirmed_shared_servers": if value["accept_shared_binaries"] == true { vec!["srv_other"] } else { vec![] },
    });
    std::fs::write(catalog.path(), serde_json::to_vec(&value).unwrap()).unwrap();
}

async fn consumer_terminal(
    consumer: &crate::controller_update::windows::consumer::Consumer,
    id: &str,
) -> treer_protocol::controller_update::Operation {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let op = consumer.observe("ws_fixture", "srv_fixture", id).unwrap();
            if op.state == treer_protocol::controller_update::State::Unconfirmed {
                return op;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn windows_update_consumer_native_confirmed_command_detaches_and_keeps_closed_custody() {
    use crate::controller_update::windows::consumer::Consumer;
    use treer_protocol::controller_update::{Health, State};
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let mut input = inputs(&root, &catalog, true);
    let (base, requests, server) = distributor(&input, false).await;
    input.settings.release_base_url = base;
    consumer_config(&catalog, &input, false);
    let consumer = Consumer::new(runtime.clone());
    assert!(consumer
        .dispatch("ws_fixture", "srv_fixture", input.dispatch.clone())
        .is_err());
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "cap remains OFF"
    );
    assert!(consumer
        .accept_confirmed("ws_other", "srv_fixture", input.dispatch.clone())
        .is_err());
    // The fixture exercises the internal already-authorized seam, NOT a real
    // Policy session or a public capability bypass.
    let accepted = consumer
        .accept_confirmed("ws_fixture", "srv_fixture", input.dispatch.clone())
        .unwrap();
    assert_eq!(accepted.state, State::Accepted);
    accepted.validate().unwrap();
    let observer = consumer.clone();
    drop(accepted); // lost browser/command response cannot cancel the owned task
    drop(consumer);
    let op = consumer_terminal(&observer, &input.dispatch.operation_id).await;
    op.validate().unwrap();
    assert!(observer.closed_for_test());
    assert_eq!(op.health, Health::Unknown);
    assert!(op.phase.is_none() && op.receipt_revision.is_none() && op.finished_at.is_none());
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 6);
    held(&root, &input);
    let same = observer
        .accept_confirmed("ws_fixture", "srv_fixture", input.dispatch.clone())
        .unwrap();
    assert_eq!(same, op);
    let mut changed = input.dispatch.clone();
    changed.operation_id = Uuid::new_v4().to_string();
    assert!(observer
        .accept_confirmed("ws_fixture", "srv_fixture", changed)
        .is_err());
    assert!(observer
        .observe("ws_other", "srv_fixture", &input.dispatch.operation_id)
        .is_err());
    assert!(observer
        .observe("ws_fixture", "srv_fixture", &Uuid::new_v4().to_string())
        .is_err());
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 6);
    drop(observer);
    server.abort();
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_consumer_native_unknown_command_is_observation_only_no_replay() {
    use crate::controller_update::windows::consumer::Consumer;
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for missing_key in [false, true] {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let mut input = inputs(&root, &catalog, false);
        let (base, requests, server) = distributor(&input, true).await;
        input.settings.release_base_url = base;
        consumer_config(&catalog, &input, missing_key);
        let consumer = Consumer::new(runtime.clone());
        consumer
            .accept_confirmed("ws_fixture", "srv_fixture", input.dispatch.clone())
            .unwrap();
        let op = consumer_terminal(&consumer, &input.dispatch.operation_id).await;
        op.validate().unwrap();
        assert!(!consumer.closed_for_test());
        let count = requests.load(std::sync::atomic::Ordering::SeqCst);
        if missing_key {
            assert_eq!(count, 0);
        } else {
            assert!((5..=6).contains(&count));
        }
        assert_eq!(
            consumer
                .accept_confirmed("ws_fixture", "srv_fixture", input.dispatch.clone())
                .unwrap(),
            op
        );
        let mut conflicting = input.dispatch.clone();
        conflicting.created_at += chrono::Duration::milliseconds(1);
        assert!(consumer
            .accept_confirmed("ws_fixture", "srv_fixture", conflicting)
            .is_err());
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), count);
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(catalog.path())
                .is_err(),
            "keep normal source pin on Unknown"
        );
        assert!(!stage(&root, &input).exists());
        assert!(!root
            .join("installed/.treer-controller-update.claim")
            .exists());
        drop(consumer);
        server.abort();
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}

#[tokio::test]
async fn windows_update_backup_transition_native_same_holding_worker_writes_and_retains_backup() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let input = inputs(&root, &catalog, false);
    let mut prepared = Preparation::local(
        &runtime,
        input.dispatch.clone(),
        &input.settings,
        &input.fixture.documents(),
        (&input.bytes, &input.bytes),
        &[],
    )
    .unwrap();
    let backup = stage(&root, &input).join("worker-backup");
    assert!(!backup.exists());
    prepared
        .start(&runtime, &input.fixture.documents())
        .await
        .unwrap();
    prepared.inspect(&runtime).await.unwrap();
    assert!(!backup.exists(), "receipt/inspection is not a write grant");
    prepared.prepare_worker_backup(&runtime).await.unwrap();
    held(&root, &input);
    for file in ["controller.backup", "cli.backup", "startup.backup"] {
        assert!(backup.join(file).is_file());
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(backup.join(file))
                .is_err(),
            "receiver independently holds sealed backups"
        );
    }
    let evidence: serde_json::Value =
        serde_json::from_slice(&std::fs::read(backup.join("backup.evidence")).unwrap()).unwrap();
    assert_eq!(evidence["phase"], "worker_backup_prepared");
    assert_eq!(
        evidence["binding"]["operation_id"],
        input.dispatch.operation_id
    );
    prepared.inspect(&runtime).await.unwrap();
    prepared.close(&runtime).await.unwrap();
    let custody = prepared.retain_closed(&runtime).unwrap();
    assert!(prepared.prepare_worker_backup(&runtime).await.is_err());
    held(&root, &input);
    assert_eq!(
        std::fs::read(backup.join("controller.backup")).unwrap(),
        OLD
    );
    assert_eq!(std::fs::read(backup.join("cli.backup")).unwrap(), OLD);
    assert_eq!(
        std::fs::read(backup.join("startup.backup")).unwrap(),
        std::fs::read(root.join("configured-state/srv_fixture-agent-startup.json")).unwrap()
    );
    drop(custody);
    drop(prepared);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_backup_transition_native_existing_destination_or_changed_state_keeps_unknown(
) {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    for changed_state in [false, true] {
        let catalog = UpdatePlanCatalogFixture::new();
        let (runtime, root, task) = update_state_fixture().await;
        let input = inputs(&root, &catalog, false);
        let mut prepared = Preparation::local(
            &runtime,
            input.dispatch.clone(),
            &input.settings,
            &input.fixture.documents(),
            (&input.bytes, &input.bytes),
            &[],
        )
        .unwrap();
        prepared
            .start(&runtime, &input.fixture.documents())
            .await
            .unwrap();
        let backup = stage(&root, &input).join("worker-backup");
        if changed_state {
            runtime.inner.startup_store.deny_absent("ag_later").unwrap();
        } else {
            // A pre-existing name is unresolved evidence, not reusable space.
            std::fs::create_dir(&backup).unwrap();
            treer_platform::secure_directory(&backup).unwrap();
            std::fs::write(backup.join("prior"), b"keep").unwrap();
        }
        assert!(prepared.prepare_worker_backup(&runtime).await.is_err());
        assert!(prepared.prepare_worker_backup(&runtime).await.is_err());
        assert!(prepared.close(&runtime).await.is_err());
        assert!(prepared.retain_closed(&runtime).is_err());
        held(&root, &input);
        if changed_state {
            assert!(!backup.exists());
        } else {
            assert_eq!(std::fs::read(backup.join("prior")).unwrap(), b"keep");
            assert!(!backup.join("backup.evidence").exists());
        }
        drop(prepared);
        drop(runtime);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
        drop(catalog);
    }
}
