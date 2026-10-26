//! Real task-owned child reception, never an installed/admitted image or Host.
use super::*;
use crate::windows_update_admission::{tests::Fixture, ReservedInputs};

#[tokio::test]
#[ignore = "task-owned worker receiver helper only"]
async fn worker_input_fixture_child() {
    let selector = std::env::var("TREER_TEST_WORKER_RECEIVE_SELECTOR").unwrap();
    let operation =
        Uuid::parse_str(&std::env::var("TREER_TEST_WORKER_RECEIVE_OPERATION").unwrap()).unwrap();
    let mode = std::env::var("TREER_TEST_WORKER_RECEIVE_MODE").unwrap();
    let root = PathBuf::from(std::env::var_os("TREER_TEST_WORKER_RECEIVE_ROOT").unwrap());
    // Synthetic authority is compiled into this ignored test helper only.
    // No key is accepted from the offer or environment.
    let key =
        ed25519_dalek::SigningKey::from_bytes(&[if mode == "wrong-key" { 99 } else { 35 }; 32])
            .verifying_key();
    let result = crate::windows_update_admission::worker_inputs::native::receive_creator_inputs(
        &selector,
        *operation.as_bytes(),
        &key,
    )
    .await;
    if mode != "valid" && mode != "state-current" {
        if result.is_ok() {
            std::process::exit(81);
        }
        return;
    }
    let mut custody = result.unwrap_or_else(|_| std::process::exit(82));
    if mode == "state-current" {
        custody.inspect_snapshot().unwrap();
    }
    std::fs::write(root.join("receiver-held"), []).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while !root.join("receiver-release").exists() {
        if tokio::time::Instant::now() >= deadline {
            std::process::exit(83);
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    if mode == "state-current" {
        assert!(custody.inspect_snapshot().is_err());
        std::fs::write(root.join("receiver-poisoned"), []).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while !root.join("receiver-restored").exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(custody.inspect_snapshot().is_err());
    }
    drop(custody);
}

fn command(
    reserved: &ReservedInputs,
    selector: &str,
    root: &Path,
    mode: &str,
) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("TREER_TEST_WORKER_RECEIVE_SELECTOR", selector)
        .env(
            "TREER_TEST_WORKER_RECEIVE_OPERATION",
            reserved.binding().operation_id.to_string(),
        )
        .env("TREER_TEST_WORKER_RECEIVE_MODE", mode)
        .env("TREER_TEST_WORKER_RECEIVE_ROOT", root)
        .args([
            "--exact",
            "controller::tests::worker_receive::worker_input_fixture_child",
            "--ignored",
            "--nocapture",
        ])
        .stdin(treer_platform::windows_update_pipe::creator_process_stdio().unwrap())
        .stderr(reserved.fixture_lock_stdio())
        .stdout(std::process::Stdio::null());
    command
}

fn reserve(
    runtime: &ControllerRuntime,
    root: &Path,
    catalog: &UpdatePlanCatalogFixture,
) -> ReservedInputs {
    let dispatch = update_confirmed_plan_binding();
    catalog.write(&dispatch, false);
    let admitted = update_confirmed_plan_inputs(runtime, root, dispatch, false).unwrap();
    let job = root.join("job");
    treer_platform::secure_directory(&job).unwrap();
    admitted
        .reserve(treer_platform::windows_update_store::UpdateStore::open(&job).unwrap())
        .unwrap()
}

async fn exited(
    child: &mut ControllerUpdateChild,
    runtime: &ControllerRuntime,
    binding: &crate::windows_update_intent::Binding,
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = child.poll(runtime, binding).unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fixture child did not exit"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn windows_update_worker_receive_native_signed_inputs_state_and_retained_sender() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let reserved = reserve(&runtime, &root, &catalog);
    let binding = reserved.binding().clone();
    let selector = Uuid::new_v4().simple().to_string();
    let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(&selector).unwrap();
    let mut command = command(&reserved, &selector, &root, "valid");
    let mut child = reserved.spawn_owned_child(&runtime, &mut command).unwrap();
    drop(command);
    child
        .accept_worker_pipe(&runtime, &binding, pipe)
        .await
        .unwrap();
    let fixture = Fixture::new(|_| {});
    let mut foreign = binding.clone();
    foreign.server_id = "srv_other".into();
    assert!(child
        .exchange_worker_inputs(&runtime, &foreign, &fixture.documents())
        .await
        .is_err());
    child
        .exchange_worker_inputs(&runtime, &binding, &fixture.documents())
        .await
        .unwrap();
    assert!(child
        .exchange_worker_inputs(&runtime, &binding, &fixture.documents())
        .await
        .is_err());
    assert!(child
        .exchange_worker_binding(&runtime, &binding)
        .await
        .is_err());
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer.exe"))
        .is_err());
    assert!(
        treer_platform::windows_update_inputs::PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .is_err()
    );
    assert!(std::fs::rename(root.join("configured-state"), root.join("moved-state")).is_err());
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .exists());
    std::fs::write(root.join("receiver-release"), []).unwrap();
    exited(&mut child, &runtime, &binding).await;
    assert!(root.join("receiver-held").is_file());
    // Natural child exit still does not release the sender reservation/pins.
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer.exe"))
        .is_err());
    drop(child);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_receive_native_tampered_signature_identity_state_and_key_refuse() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    for case in 0..6 {
        let directory = root.join(format!("receive-{case}"));
        treer_platform::secure_directory(&directory).unwrap();
        let reserved = reserve(&runtime, &directory, &catalog);
        let binding = reserved.binding().clone();
        let selector = Uuid::new_v4().simple().to_string();
        let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(&selector).unwrap();
        let mut command = command(
            &reserved,
            &selector,
            &directory,
            if case == 5 { "wrong-key" } else { "refuse" },
        );
        let mut child = reserved.spawn_owned_child(&runtime, &mut command).unwrap();
        drop(command);
        child
            .accept_worker_pipe(&runtime, &binding, pipe)
            .await
            .unwrap();
        let fixture = Fixture::new(|_| {});
        assert!(
            child
                .exchange_fixture_worker_inputs(&runtime, &binding, &fixture.documents(), |offer| {
                    match case {
                        0 => offer.channel_signature[0] ^= 1,
                        1 => offer.target.identities[1].1 ^= 1,
                        2 => offer.state.snapshot.as_mut().unwrap().sha256 = "e".repeat(64),
                        3 => offer.state.identity.1 ^= 1,
                        4 => {
                            offer.dispatch.target.identity.cli_sha256 = "e".repeat(64);
                            offer.dispatch.plan_digest =
                                offer.dispatch.expected_plan_digest().unwrap();
                        }
                        _ => {}
                    }
                })
                .await
                .is_err(),
            "case {case}"
        );
        assert!(child
            .exchange_worker_inputs(&runtime, &binding, &fixture.documents())
            .await
            .is_err());
        exited(&mut child, &runtime, &binding).await;
        assert!(directory
            .join("installed/.treer-controller-update.claim")
            .exists());
        assert!(!directory.join("receiver-held").exists());
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .open(directory.join("target/treer.exe"))
            .is_err());
        drop(child);
    }
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn windows_update_worker_state_current_native_real_fence_change_poison_keeps_custody() {
    let _serial = UPDATE_PLAN_CONFIG_LOCK.lock().await;
    let catalog = UpdatePlanCatalogFixture::new();
    let (runtime, root, task) = update_state_fixture().await;
    let reserved = reserve(&runtime, &root, &catalog);
    let binding = reserved.binding().clone();
    let selector = Uuid::new_v4().simple().to_string();
    let pipe = treer_platform::windows_update_pipe::CreatedChildPipe::bind(&selector).unwrap();
    let mut command = command(&reserved, &selector, &root, "state-current");
    let mut child = reserved.spawn_owned_child(&runtime, &mut command).unwrap();
    drop(command);
    child
        .accept_worker_pipe(&runtime, &binding, pipe)
        .await
        .unwrap();
    let fixture = Fixture::new(|_| {});
    child
        .exchange_worker_inputs(&runtime, &binding, &fixture.documents())
        .await
        .unwrap();
    let wait_marker = |name: &'static str| {
        let path = root.join(name);
        async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
            while !path.exists() {
                assert!(tokio::time::Instant::now() < deadline, "missing {name}");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    };
    wait_marker("receiver-held").await;
    let startup = root.join("configured-state/srv_fixture-agent-startup.json");
    let original = std::fs::read(&startup).unwrap();
    let saved = root.join("startup-original");
    std::fs::rename(&startup, &saved).unwrap();
    runtime.inner.startup_store.deny_absent("ag_later").unwrap();
    std::fs::write(root.join("receiver-release"), []).unwrap();
    wait_marker("receiver-poisoned").await;
    // Only this disposable test restores the ORIGINAL file identity AND bytes
    // to prove poison is irreversible, not merely another inode mismatch.
    // Product code has no startup restoration or resume API.
    std::fs::rename(&startup, root.join("startup-changed")).unwrap();
    std::fs::rename(saved, &startup).unwrap();
    assert_eq!(std::fs::read(&startup).unwrap(), original);
    assert!(child.poll(&runtime, &binding).unwrap().is_none());
    assert!(std::fs::rename(root.join("configured-state"), root.join("moved-state")).is_err());
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer.exe"))
        .is_err());
    assert!(
        treer_platform::windows_update_inputs::PinnedPair::open(&root.join("installed"))
            .unwrap()
            .lock_installation()
            .is_err()
    );
    assert!(root
        .join("installed/.treer-controller-update.claim")
        .is_file());
    std::fs::write(root.join("receiver-restored"), []).unwrap();
    exited(&mut child, &runtime, &binding).await;
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("target/treer.exe"))
        .is_err());
    drop(child);
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(root).unwrap();
}
