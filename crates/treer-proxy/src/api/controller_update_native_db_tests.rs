//! Paired hosted test only: authenticated HTTP/PG owns the binding; the other
//! test process runs the actual native worker and publishes its real projection.
//! The command transport/eligibility are fixture adapters, not product support.
use super::*;
use std::io::{Read, Write};
use treer_platform::private_directory::PrivateDirectory;

#[path = "controller_update_websocket_native_tests.rs"]
mod websocket_native;

const NATIVE_WS: &str = "ws_native_acceptance";
const NATIVE_MACHINE: &str = "srv_native_acceptance";

fn private_root() -> PrivateDirectory {
    assert_eq!(std::env::var("TREER_DISPOSABLE_RUNNER").as_deref(), Ok("1"));
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    assert_eq!(
        std::env::var("RUNNER_ENVIRONMENT").as_deref(),
        Ok("github-hosted")
    );
    for key in [
        "TREER_AGENT_ID",
        "TREER_WORKLOAD_CREDENTIAL",
        "TREER_OPERATOR_CREDENTIAL",
    ] {
        assert!(
            std::env::var_os(key).is_none(),
            "managed/operator identity is forbidden"
        );
    }
    let path = PathBuf::from(std::env::var_os("TREER_ACCEPTANCE_ROOT").unwrap());
    assert_eq!(path.canonicalize().unwrap(), path);
    let home = PathBuf::from(std::env::var_os("HOME").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(path.parent(), Some(home.as_path()));
    let token = path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("tn-")
        .unwrap();
    assert!(
        token.len() == 16
            && token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    PrivateDirectory::open(&path).unwrap()
}

fn read<T: serde::de::DeserializeOwned>(root: &PrivateDirectory, name: &str) -> Option<T> {
    let file = match root.read_file(name) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("private bridge read refused: {e}"),
    };
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() <= 65536);
    Some(serde_json::from_slice(&bytes).unwrap())
}
async fn wait<T: serde::de::DeserializeOwned>(
    root: &PrivateDirectory,
    name: &str,
    seconds: u64,
) -> T {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            if let Some(value) = read(root, name) {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("native bridge timed out; no mutation retry")
}
fn dispatch_once(root: &PrivateDirectory, binding: &DispatchBinding) {
    assert!(binding.validate());
    // EXCL and sync: losing any response never permits another submission.
    let mut intent = root
        .create_file("proxy-native-dispatch-intent.json")
        .unwrap();
    intent
        .write_all(&serde_json::to_vec(binding).unwrap())
        .unwrap();
    intent.sync_all().unwrap();
    root.sync().unwrap();
    let mut file = root
        .create_file("proxy-native-dispatch-stage.json")
        .unwrap();
    file.write_all(&serde_json::to_vec(binding).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    root.replace_file(
        "proxy-native-dispatch-stage.json",
        "proxy-native-dispatch.json",
    )
    .unwrap();
}

async fn state_and_channel() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<SocketFrame>) {
    let template = super::super::super::tests::state_with_managed_agent().await;
    let mut server = template
        .resolve_server("default", "machine-a")
        .await
        .unwrap();
    server.workspace_id = NATIVE_WS.into();
    server.server_id = NATIVE_MACHINE.into();
    let state = AppState::new();
    let id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // Test transport eligibility only. Ordinary native capability remains OFF.
    state
        .register_server_instance_with_capabilities(
            server.clone(),
            id,
            "paired-native-fixture".into(),
            vec![wire::CAPABILITY.into()],
            tx,
        )
        .await
        .unwrap();
    state
        .apply_snapshot(
            id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![],
            },
        )
        .await
        .unwrap();
    (state, rx)
}

async fn call(
    http: &HttpProxy,
    method: reqwest::Method,
    suffix: &str,
    token: bool,
    origin: &str,
    body: Value,
) -> reqwest::Response {
    let mut request = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .unwrap()
        .request(
            method,
            format!(
                "{}/api/workspaces/{NATIVE_WS}/machines/{NATIVE_MACHINE}/controller-update{suffix}",
                http.origin
            ),
        )
        .header("host", "proxy.treer.example");
    if token {
        request = request.header("cookie", "treer_session=native-http-session");
    }
    if !origin.is_empty() {
        request = request.header("origin", origin);
    }
    request.json(&body).send().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "paired hosted private-account native worker fixture plus disposable PostgreSQL only"]
async fn authenticated_http_pg_uses_same_native_worker_receipt_without_redispatch() {
    let root = private_root();
    let inspection: Inspection = wait(&root, "proxy-native-inspection.json", 90).await;
    assert!(
        !inspection.native_ready,
        "production capability must remain OFF"
    );
    let input: Value = read(&root, "acceptance-input.json").unwrap();
    let target: wire::Target = serde_json::from_value(input["target"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&inspection.current).unwrap(),
        input["current"]
    );
    assert_eq!(inspection.platform, wire::Platform::LinuxX86_64);
    let auth = AuthStore::for_test("private-native-fixture").await;
    auth.seed_test_workspace(NATIVE_WS).await;
    let now = Utc::now();
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at,email_verified) VALUES('native-owner','native@example.test','Native fixture','fixture',$1,TRUE)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,'native-owner','member',$2)")
        .bind(format!("org_{NATIVE_WS}")).bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('native-http-session','native-owner',$1,$2)")
        .bind(now.to_rfc3339()).bind((now+chrono::Duration::hours(1)).to_rfc3339()).execute(&auth.pool()).await.unwrap();
    let current = wire::Target {
        target_id: "acceptance-old".into(),
        identity: inspection.current.clone(),
        platform: inspection.platform,
        version_label: "native fixture old".into(),
    };
    for (sequence, entry) in [(1, current), (2, target.clone())] {
        sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES('linux-x86_64',1,$1,$2)")
            .bind(sequence as i64).bind(serde_json::to_value(entry).unwrap()).execute(&auth.pool()).await.unwrap();
    }
    let (state, mut rx) = state_and_channel().await;
    let producer = state.clone();
    let db = auth.clone();
    let actor = tokio::spawn(async move {
        let root = private_root();
        let mut dispatches = 0;
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Text(frame) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } = serde_json::from_str(&frame).unwrap() else {
                continue;
            };
            let result = match envelope.command {
                AgentCommand::ControllerUpdateInspect {} => {
                    let mut native: Inspection =
                        read(&root, "proxy-native-inspection.json").unwrap();
                    assert!(!native.native_ready);
                    // The fixture grants test transport eligibility, not a
                    // release/product capability or permission bypass.
                    native.native_ready = true;
                    serde_json::to_value(native).unwrap()
                }
                AgentCommand::ControllerUpdateDispatch { binding } => {
                    dispatches += 1;
                    assert_eq!(dispatches, 1);
                    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE operation_id=$1 AND holds_slot")
                        .bind(&binding.operation_id).fetch_one(&db.pool()).await.unwrap();
                    assert_eq!(count, 1, "PG claim must precede native submission");
                    dispatch_once(&root, &binding);
                    // Deliberately lose the acknowledgement after the one
                    // native submission. The production HTTP path retains its
                    // durable Accepted claim regardless of this transport error.
                    producer.complete_command(CommandResult::failure(envelope.command_id,
                        treer_protocol::ProtocolError::new("controller_update_result_unconfirmed",
                            "Fixture lost native submission acknowledgement; observe without retry"))).await;
                    return dispatches;
                }
                _ => panic!("initial actor may only inspect and dispatch once"),
            };
            producer
                .complete_command(CommandResult::success(envelope.command_id, result))
                .await;
        }
        panic!("native dispatch channel closed before submission");
    });
    let http = HttpProxy::start(state, auth.clone()).await;
    assert_eq!(
        call(
            &http,
            reqwest::Method::POST,
            "/prepare",
            false,
            ORIGIN,
            json!({"target_id":target.target_id})
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &http,
            reqwest::Method::POST,
            "/prepare",
            true,
            "https://foreign.invalid",
            json!({"target_id":target.target_id})
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert!(read::<Value>(&root, "proxy-native-dispatch.json").is_none());
    let preview: wire::Preview = json(
        call(
            &http,
            reqwest::Method::POST,
            "/prepare",
            true,
            ORIGIN,
            json!({"target_id":target.target_id}),
        )
        .await,
    )
    .await;
    let confirmation = json!({"confirmation_token":preview.confirmation_token});
    let accepted: wire::Operation = json(
        call(
            &http,
            reqwest::Method::POST,
            "/confirm",
            true,
            ORIGIN,
            confirmation.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(accepted.operation_id, preview.operation_id);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), actor)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    drop(http);
    // Complete native execution continues independently of all Proxy memory.
    let result: Value = wait(&root, "native-acceptance-result.json", 420).await;
    assert_eq!(result["native_update_acceptance"], "passed");
    let native: wire::Operation = serde_json::from_value(result["operation"].clone()).unwrap();
    assert_eq!(native.operation_id, preview.operation_id);
    assert_eq!(native.state, wire::State::Succeeded);
    let (state, mut rx) = state_and_channel().await;
    let producer = state.clone();
    let observed = native.clone();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let observer = tokio::spawn(async move {
        tokio::pin!(finished);
        let mut queries = 0;
        loop {
            let frame = tokio::select! {biased;
                frame=rx.recv()=>frame.expect("reconnect channel closed"),
                result=&mut finished=>{result.unwrap();break;}
            };
            let SocketFrame::Text(frame) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } = serde_json::from_str(&frame).unwrap() else {
                continue;
            };
            let AgentCommand::ControllerUpdateOperation { operation_id } = envelope.command else {
                panic!("reconnect must not dispatch")
            };
            queries += 1;
            assert_eq!(operation_id, observed.operation_id);
            producer
                .complete_command(CommandResult::success(
                    envelope.command_id,
                    serde_json::to_value(&observed).unwrap(),
                ))
                .await;
        }
        queries
    });
    let reconnected = HttpProxy::start(state, auth.clone()).await;
    let repeat: wire::Operation = json(
        call(
            &reconnected,
            reqwest::Method::POST,
            "/confirm",
            true,
            ORIGIN,
            confirmation,
        )
        .await,
    )
    .await;
    assert_eq!(repeat.operation_id, preview.operation_id);
    let done: wire::Operation = json(
        call(
            &reconnected,
            reqwest::Method::GET,
            &format!("/operations/{}", preview.operation_id),
            true,
            "",
            Value::Null,
        )
        .await,
    )
    .await;
    assert_eq!(done, native);
    finish.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), observer)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    let active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE holds_slot")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(active, 0);
    let stored: Value = sqlx::query_scalar(
        "SELECT operation FROM controller_update_operations WHERE operation_id=$1",
    )
    .bind(&preview.operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(stored, serde_json::to_value(&native).unwrap());
    let submitted: DispatchBinding = read(&root, "proxy-native-dispatch.json").unwrap();
    assert_eq!(submitted.operation_id, preview.operation_id);
    let mut receipt = root.create_file("proxy-native-db-result.json").unwrap();
    receipt.write_all(&serde_json::to_vec_pretty(&json!({"schema":1,"same_operation_id":preview.operation_id,"http_auth_and_origin":true,
        "durable_pg_claim_before_single_native_submit":true,"native_worker_receipt":done,"proxy_memory_rebuilt":true,
        "native_submit_count":1,"capability_enabled":false,"transport":"private_test_bridge_not_production_controller_dispatch"})).unwrap()).unwrap();
    receipt.sync_all().unwrap();
    root.sync().unwrap();
}

#[test]
fn native_bridge_dispatch_is_exclusive_and_publishes_complete_binding() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("treer-native-bridge-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = PrivateDirectory::open(&path).unwrap();
    let mut binding = DispatchBinding {
        operation_id: Uuid::new_v4().to_string(),
        workspace_id: NATIVE_WS.into(),
        server_id: NATIVE_MACHINE.into(),
        current: identity('a'),
        target: target(),
        catalog_revision: 1,
        plan_digest: "0".repeat(64),
        created_at: Utc::now(),
    };
    binding.plan_digest = binding.expected_plan_digest().unwrap();
    dispatch_once(&root, &binding);
    let published: DispatchBinding = read(&root, "proxy-native-dispatch.json").unwrap();
    assert_eq!(
        serde_json::to_value(&published).unwrap(),
        serde_json::to_value(&binding).unwrap()
    );
    assert!(root
        .read_file("proxy-native-dispatch-stage.json")
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch_once(
            &root, &binding
        )))
        .is_err()
    );
    let retained: DispatchBinding = read(&root, "proxy-native-dispatch.json").unwrap();
    assert_eq!(retained.operation_id, binding.operation_id);
    root.remove_file("proxy-native-dispatch.json").unwrap();
    root.remove_file("proxy-native-dispatch-intent.json")
        .unwrap();
    drop(root);
    std::fs::remove_dir(path).unwrap();
}
