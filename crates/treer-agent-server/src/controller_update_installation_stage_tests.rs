//! Only private saved-config fixtures and same-source signed dev images.
use super::*;
use crate::{
    controller_update::windows::preparation::Preparation,
    windows_update_admission::tests::{Fixture, OLD},
};
use ed25519_dalek::pkcs8::EncodePublicKey;
use sha2::Digest;

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
