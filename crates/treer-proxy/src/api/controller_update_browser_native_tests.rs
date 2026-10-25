//! New browser leaf: normal login and Machine UI, never scripted update POSTs.
//! Uses the existing hosted native parent, but a fresh root/account/operation.
use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

// Fixed stages only: panic text, SQL, URLs and fixture credentials must never
// enter the public collector. This records location, not a claimed root cause.
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    ReadingInput,
    Database,
    FixtureUser,
    Enrollment,
    Catalog,
    Router,
    ControllerConnection,
    Custody,
    BrowserConfirmation,
    TypedDispatch,
    Reconnection,
    NativeResult,
    BrowserResult,
    SameOperationChecks,
    ParentCleanup,
    Complete,
}

struct Progress<'a> {
    root: &'a PrivateDirectory,
    phase: Phase,
    complete: bool,
}

impl<'a> Progress<'a> {
    fn new(root: &'a PrivateDirectory) -> Self {
        let value = Self {
            root,
            phase: Phase::ReadingInput,
            complete: false,
        };
        value
            .save("running")
            .expect("safe fixture diagnostic refused");
        value
    }

    fn set(&mut self, phase: Phase) {
        self.phase = phase;
        self.save("running")
            .expect("safe fixture diagnostic refused");
    }

    fn finish(&mut self) {
        self.phase = Phase::Complete;
        self.save("passed")
            .expect("safe fixture diagnostic refused");
        self.complete = true;
    }

    fn save(&self, status: &str) -> std::io::Result<()> {
        let mut file = self.root.create_file("proxy-browser-diagnostic.pending")?;
        serde_json::to_writer(
            &mut file,
            &json!({"schema_version":1,"phase":self.phase,"status":status}),
        )?;
        file.sync_all()?;
        self.root.replace_file(
            "proxy-browser-diagnostic.pending",
            "proxy-browser-diagnostic.json",
        )?;
        self.root.sync()
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        if !self.complete {
            // Do not hide the original panic or turn an unobserved outcome into
            // success if publishing diagnostics itself fails.
            let _ = self.save("incomplete");
        }
    }
}

#[derive(Clone, Default)]
struct Writes {
    prepare: Arc<AtomicUsize>,
    confirm: Arc<AtomicUsize>,
}

async fn browser_proxy(
    state: AppState,
    auth: AuthStore,
    listener: tokio::net::TcpListener,
    dist: PathBuf,
    writes: Writes,
) -> HttpProxy {
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let url = Url::parse(&format!("{origin}/")).unwrap();
    let identity = IdentityIssuer::load(&auth, &url).await.unwrap();
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let bootstrap = BootstrapConfig::new(
        url.clone(),
        PathBuf::from("dist"),
        Url::parse("https://releases.invalid/").unwrap(),
    )
    .with_app_dist(crate::api::app_dist::AppDistConfig::new(&dist, &url, &url).unwrap());
    let app = router(
        state,
        bootstrap,
        auth,
        PolicyEngine::allow_all(),
        identity,
        BrowserAccess::new(&url, &url).unwrap(),
        IngressConfig::new(None, &url, &url).unwrap(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    )
    .layer(axum::middleware::from_fn(
        move |request: axum::http::Request<Body>, next: axum::middleware::Next| {
            let writes = writes.clone();
            async move {
                // Count actual inbound requests, including refusals. Browser reload
                // cannot hide a second POST behind idempotency or capability OFF.
                if request.method() == Method::POST {
                    let path = request.uri().path();
                    if path.ends_with("/controller-update/prepare") {
                        writes.prepare.fetch_add(1, Ordering::SeqCst);
                    }
                    if path.ends_with("/controller-update/confirm") {
                        writes.confirm.fetch_add(1, Ordering::SeqCst);
                    }
                }
                next.run(request).await
            }
        },
    ));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    HttpProxy { origin, task }
}

async fn operation_in_database(auth: &AuthStore) -> wire::Operation {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let rows: Vec<Value> =
                sqlx::query_scalar("SELECT operation FROM controller_update_operations")
                    .fetch_all(&auth.pool())
                    .await
                    .unwrap();
            assert!(rows.len() <= 1, "browser created more than one operation");
            if let Some(row) = rows.into_iter().next() {
                return serde_json::from_value(row).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("browser did not confirm; never retry a mutation")
}

// Synchronization evidence only, never update/health authority. The actual
// preparation writes this stage only AFTER observing and saving the original
// Controller baseline. Disconnecting on outbound dispatch count races that
// observation's required proxy_connected health claim.
fn baseline_saved_stage(value: &Value) -> bool {
    value.as_object().is_some_and(|object| object.len() == 2)
        && value["schema"] == 1
        && value["stage"] == "startup_backup"
}

async fn wait_for_native_baseline(root: &PrivateDirectory) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            assert!(
                read::<Value>(root, "native-submit-failure.json").is_none(),
                "native preparation refused before disconnect; never retry"
            );
            if let Some(value) = read::<Value>(root, "native-stage-startup_backup.json") {
                assert!(
                    baseline_saved_stage(&value),
                    "invalid baseline stage evidence"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native baseline not observed; do not force disconnect or retry");
}

#[test]
fn disconnect_barrier_requires_completed_baseline_stage() {
    assert!(baseline_saved_stage(
        &json!({"schema":1,"stage":"startup_backup"})
    ));
    for value in [
        json!({"schema":1,"stage":"baseline"}),
        json!({"schema":1,"stage":"host"}),
        json!({"schema":2,"stage":"startup_backup"}),
        json!({"schema":1,"stage":"startup_backup","untrusted":true}),
    ] {
        assert!(!baseline_saved_stage(&value));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "fresh hosted browser, real WS/native parent and disposable PostgreSQL only"]
async fn browser_machine_update_observes_same_native_operation_after_refresh() {
    let root = private_root();
    let mut progress = Progress::new(&root);
    let input: Value = read(&root, "acceptance-input.json").unwrap();
    let target: wire::Target = serde_json::from_value(input["target"].clone()).unwrap();
    let dist = PathBuf::from(
        std::env::var_os("TREER_BROWSER_DIST").expect("explicit built Console required"),
    );
    assert!(dist.is_absolute());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = Url::parse(&format!("http://{address}/")).unwrap();
    progress.set(Phase::Database);
    let auth = AuthStore::for_test("unused-private-browser-admin")
        .await
        .with_public_urls(url.clone(), url);
    auth.seed_test_workspace(NATIVE_WS).await;
    progress.set(Phase::FixtureUser);
    let email = format!("browser-{}@example.test", Uuid::new_v4().simple());
    let password = Uuid::new_v4().to_string();
    let hash = crate::auth::hash_password(&password).unwrap();
    let now = Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at,email_verified) VALUES('browser-owner',$1,'Browser fixture',$2,$3,TRUE)")
        .bind(&email).bind(hash).bind(&now).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,'browser-owner','member',$2)")
        .bind(format!("org_{NATIVE_WS}")).bind(&now).execute(&auth.pool()).await.unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0, "no injected human session");
    progress.set(Phase::Enrollment);
    let enrollment = auth
        .create_machine_enrollment(NATIVE_WS, "browser-owner")
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
    progress.set(Phase::Catalog);
    let current = wire::Target {
        target_id: "acceptance-old".into(),
        identity: serde_json::from_value(input["current"].clone()).unwrap(),
        platform: target.platform,
        version_label: "native old".into(),
    };
    for (sequence, entry) in [(1, current), (2, target.clone())] {
        sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES('linux-x86_64',1,$1,$2)")
            .bind(sequence as i64).bind(serde_json::to_value(entry).unwrap()).execute(&auth.pool()).await.unwrap();
    }
    let state = AppState::new();
    let writes = Writes::default();
    progress.set(Phase::Router);
    let http = browser_proxy(
        state.clone(),
        auth.clone(),
        listener,
        dist.clone(),
        writes.clone(),
    )
    .await;
    publish(
        &root,
        "controller-proxy-ready.json",
        &json!({"proxy":http.origin,"machine_token":format!("{NATIVE_MACHINE}.{secret}")}),
    );
    progress.set(Phase::ControllerConnection);
    connected(&state).await;
    progress.set(Phase::Custody);
    let custody: Value = wait(&root, "controller-dispatch-custody-ready.json", 60).await;
    assert_eq!(custody["old_controller_captured"], true);
    // Private fresh fixture credentials only; never upload this file or browser
    // storage/trace. The browser obtains its own HttpOnly session via login UI.
    publish(
        &root,
        "browser-ready.json",
        &json!({"origin":http.origin,"email":email,"password":password,"workspace_id":NATIVE_WS,"organization_id":format!("org_{NATIVE_WS}"),"machine_id":NATIVE_MACHINE,"target_id":target.target_id}),
    );
    progress.set(Phase::BrowserConfirmation);
    let accepted = operation_in_database(&auth).await;
    progress.set(Phase::TypedDispatch);
    tokio::time::timeout(Duration::from_secs(30), async {
        while state.fixture_update_dispatch_attempts() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("confirmed operation did not reach typed dispatcher");
    wait_for_native_baseline(&root).await;
    let first_state = state.clone();
    progress.set(Phase::Reconnection);
    state
        .close_fixture_server_connection(NATIVE_WS, NATIVE_MACHINE)
        .await;
    drop(http);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let state = AppState::new();
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    let _http = browser_proxy(state.clone(), auth.clone(), listener, dist, writes.clone()).await;
    connected(&state).await;
    publish(
        &root,
        "browser-reconnected.json",
        &json!({"operation_id":accepted.operation_id}),
    );
    progress.set(Phase::NativeResult);
    let result: Value = wait(&root, "native-acceptance-result.json", 420).await;
    assert_eq!(result["native_update_acceptance"], "passed");
    let native: wire::Operation = serde_json::from_value(result["operation"].clone()).unwrap();
    progress.set(Phase::BrowserResult);
    let browser: Value = wait(&root, "browser-result.json", 45).await;
    progress.set(Phase::SameOperationChecks);
    let observed: wire::Operation = serde_json::from_value(browser["operation"].clone()).unwrap();
    assert_eq!(browser["normal_login"], true);
    assert_eq!(browser["refreshed"], true);
    assert_eq!(browser["prepare_requests"], 1);
    assert_eq!(browser["confirm_requests"], 1);
    assert_eq!(browser["duplicate_write_requests"], 0);
    assert_eq!(writes.prepare.load(Ordering::SeqCst), 1);
    assert_eq!(writes.confirm.load(Ordering::SeqCst), 1);
    assert_eq!(first_state.fixture_update_dispatch_attempts(), 1);
    assert_eq!(state.fixture_update_dispatch_attempts(), 0);
    assert_eq!(native.operation_id, accepted.operation_id);
    assert_eq!(observed, native);
    assert_eq!(native.state, wire::State::Succeeded);
    let stored: Value = sqlx::query_scalar(
        "SELECT operation FROM controller_update_operations WHERE operation_id=$1",
    )
    .bind(&native.operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(stored, serde_json::to_value(&native).unwrap());
    let slots: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE holds_slot")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(slots, 0);
    assert!(!state
        .server_capabilities(NATIVE_WS, NATIVE_MACHINE)
        .await
        .unwrap()
        .contains(wire::CAPABILITY));
    assert!(read::<Value>(&root, "controller-dispatch-repeat.json").is_none());
    publish(
        &root,
        "browser-complete.json",
        &json!({"operation_id": native.operation_id}),
    );
    publish(
        &root,
        "proxy-websocket-native-result.json",
        &json!({"schema":1,"operation":native,"browser_interaction":true,"normal_login":true,"prepare_requests":1,"confirm_requests":1,"duplicate_write_requests":0,"dispatch_attempts_old":1,"dispatch_attempts_reconnected":0,"same_pg_operation":true,"ordinary_capability_enabled":false}),
    );
    progress.set(Phase::ParentCleanup);
    let cleanup: Value = wait(&root, "fixture-cleanup.json", 60).await;
    assert_eq!(cleanup["host_reaped_and_socket_absent"], true);
    assert_eq!(cleanup["native_result_available"], true);
    progress.finish();
}

#[test]
fn browser_progress_preserves_only_stage_on_unwind_and_requires_explicit_finish() {
    use std::os::unix::fs::DirBuilderExt;
    let path = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("treer-browser-progress-{}", Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let root = PrivateDirectory::open(&path).unwrap();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut progress = Progress::new(&root);
        progress.set(Phase::FixtureUser);
        panic!("private password / SQL / URL must not enter diagnostic");
    }));
    assert!(failure.is_err());
    assert_eq!(
        read::<Value>(&root, "proxy-browser-diagnostic.json").unwrap(),
        json!({"schema_version":1,"phase":"fixture_user","status":"incomplete"})
    );
    {
        let mut progress = Progress::new(&root);
        progress.finish();
    }
    assert_eq!(
        read::<Value>(&root, "proxy-browser-diagnostic.json").unwrap(),
        json!({"schema_version":1,"phase":"complete","status":"passed"})
    );
    drop(root);
    std::fs::remove_dir_all(path).unwrap();
}
