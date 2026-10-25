//! Real production router, machine websocket authentication, Controller
//! ProxyClient/typed handler and native worker. No actor/completion injection.
use super::*;

#[path = "controller_update_browser_native_tests.rs"]
mod browser_native;

fn publish(root: &PrivateDirectory, name: &str, value: &Value) {
    let mut file = root.create_file(name).unwrap();
    file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    file.sync_all().unwrap();
    root.sync().unwrap();
}
fn observation(root: &PrivateDirectory, value: &Value) {
    publish(root, "proxy-websocket-observation.pending", value);
    root.replace_file(
        "proxy-websocket-observation.pending",
        "proxy-websocket-observation.json",
    )
    .unwrap();
    root.sync().unwrap();
}
async fn connected(state: &AppState) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if state
                .resolve_server(NATIVE_WS, NATIVE_MACHINE)
                .await
                .is_ok_and(|s| s.status == treer_protocol::ServerStatus::Online)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("real Controller websocket did not register");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "fresh hosted real Controller websocket/native worker and disposable PG only"]
async fn authenticated_websocket_dispatch_runs_native_worker_and_reconnects_same_operation() {
    let root = private_root();
    let input: Value = read(&root, "acceptance-input.json").unwrap();
    let target: wire::Target = serde_json::from_value(input["target"].clone()).unwrap();
    let current = wire::Target {
        target_id: "acceptance-old".into(),
        identity: serde_json::from_value(input["current"].clone()).unwrap(),
        platform: target.platform,
        version_label: "native old".into(),
    };
    let auth = AuthStore::for_test("private-real-websocket").await;
    auth.seed_test_workspace(NATIVE_WS).await;
    let now = Utc::now();
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at,email_verified) VALUES('native-owner','native@example.test','Native fixture','fixture',$1,TRUE)").bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,'native-owner','member',$2)").bind(format!("org_{NATIVE_WS}")).bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('native-http-session','native-owner',$1,$2)").bind(now.to_rfc3339()).bind((now+chrono::Duration::hours(1)).to_rfc3339()).execute(&auth.pool()).await.unwrap();
    // Real disposable machine credential hashing/authentication, never injected
    // MachineSession. Only its generated fixture ID is renamed before use.
    let enrollment = auth
        .create_machine_enrollment(NATIVE_WS, "native-owner")
        .await
        .unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    sqlx::query("UPDATE machines SET server_id=$1 WHERE server_id=$2")
        .bind(NATIVE_MACHINE)
        .bind(&machine.server_id)
        .execute(&auth.pool())
        .await
        .unwrap();
    let (_, secret) = machine.machine_token.split_once('.').unwrap();
    let token = format!("{NATIVE_MACHINE}.{secret}");
    for (sequence, entry) in [(1, current), (2, target.clone())] {
        sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES('linux-x86_64',1,$1,$2)").bind(sequence as i64).bind(serde_json::to_value(entry).unwrap()).execute(&auth.pool()).await.unwrap();
    }
    let state = AppState::new();
    let http = HttpProxy::start(state.clone(), auth.clone()).await;
    let address: std::net::SocketAddr = http
        .origin
        .strip_prefix("http://")
        .unwrap()
        .parse()
        .unwrap();
    publish(
        &root,
        "controller-proxy-ready.json",
        &json!({"proxy":http.origin,"machine_token":token}),
    );
    connected(&state).await;
    let custody: Value = wait(&root, "controller-dispatch-custody-ready.json", 60).await;
    assert_eq!(custody["old_controller_captured"], true);
    for (session, origin, status) in [
        (false, ORIGIN, StatusCode::UNAUTHORIZED),
        (true, "https://foreign.invalid", StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            call(
                &http,
                reqwest::Method::POST,
                "/prepare",
                session,
                origin,
                json!({"target_id":target.target_id})
            )
            .await
            .status(),
            status
        );
    }
    assert!(read::<Value>(&root, "controller-dispatch-binding.json").is_none());
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
    let first_state = state.clone();
    // Drop the actual router and its in-memory state; reconnect through the
    // same authenticated websocket route. Neither side resubmits a mutation.
    state
        .close_fixture_server_connection(NATIVE_WS, NATIVE_MACHINE)
        .await;
    drop(http);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let state = AppState::new();
    let http = HttpProxy::start_on(state.clone(), auth.clone(), address).await;
    connected(&state).await;
    let repeat: wire::Operation = json(
        call(
            &http,
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
    let result: Value = wait(&root, "native-acceptance-result.json", 420).await;
    assert_eq!(result["native_update_acceptance"], "passed");
    let native: wire::Operation = serde_json::from_value(result["operation"].clone()).unwrap();
    // A real Controller may still be reconnecting after its local worker has
    // published success. Follow the normal read-only polling contract; never
    // reconfirm or interpret temporary silence as success.
    let done: wire::Operation = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let observed: wire::Operation = json(
                call(
                    &http,
                    reqwest::Method::GET,
                    &format!("/operations/{}", preview.operation_id),
                    true,
                    "",
                    Value::Null,
                )
                .await,
            )
            .await;
            observation(&root,&json!({"operation":observed,"native_operation":native,"dispatch_attempts_old":first_state.fixture_update_dispatch_attempts(),"dispatch_attempts_reconnected":state.fixture_update_dispatch_attempts()}));
            if observed.state != wire::State::Updating && observed.state != wire::State::Unconfirmed
            {
                break observed;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("receipt did not reconcile through ordinary replacement");
    // Retain the safe observation before comparisons; private panic logs are
    // deliberately not release artifacts and cannot be the only failure clue.
    observation(
        &root,
        &json!({"operation":done,"native_operation":native,"dispatch_attempts_old":first_state.fixture_update_dispatch_attempts(),"dispatch_attempts_reconnected":state.fixture_update_dispatch_attempts()}),
    );
    assert_eq!(done, native);
    assert_eq!(done.state, wire::State::Succeeded);
    assert_eq!(first_state.fixture_update_dispatch_attempts(), 1);
    assert_eq!(
        state.fixture_update_dispatch_attempts(),
        0,
        "reconnect must not even attempt a dispatch refused by ordinary target capability"
    );
    assert!(
        !state
            .server_capabilities(NATIVE_WS, NATIVE_MACHINE)
            .await
            .unwrap()
            .contains(wire::CAPABILITY),
        "replacement is ordinary capOFF image; operation reads do not enable fresh updates"
    );
    let stored: Value = sqlx::query_scalar(
        "SELECT operation FROM controller_update_operations WHERE operation_id=$1",
    )
    .bind(&preview.operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(stored, serde_json::to_value(&native).unwrap());
    let active: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE holds_slot")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(active, 0);
    let submitted: DispatchBinding = read(&root, "controller-dispatch-binding.json").unwrap();
    assert_eq!(submitted.operation_id, preview.operation_id);
    assert!(
        read::<Value>(&root, "proxy-native-dispatch.json").is_none(),
        "old synthetic bridge must not run"
    );
    assert!(
        read::<Value>(&root, "controller-dispatch-repeat.json").is_none(),
        "no second Controller dispatch, including refused repeats"
    );
    publish(
        &root,
        "proxy-websocket-native-result.json",
        &json!({"schema":1,"operation":done,"real_authenticated_machine_websocket":true,"controller_typed_dispatch":true,"same_pg_operation":true,"reconnect_observation_only":true,"dispatch_attempts_old":first_state.fixture_update_dispatch_attempts(),"dispatch_attempts_reconnected":state.fixture_update_dispatch_attempts(),"synthetic_actor":false,"ordinary_capability_enabled":false,"artifact_transport":"local_pinned_not_https_retest"}),
    );
    // Keep the real router alive until the parent independently verifies the
    // new Controller image/Host and finishes its own child cleanup.
    let cleanup: Value = wait(&root, "fixture-cleanup.json", 60).await;
    assert_eq!(cleanup["host_reaped_and_socket_absent"], true);
    assert_eq!(cleanup["native_result_available"], true);
}
