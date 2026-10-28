//! Real loopback HTTP, production router/session middleware and private
//! PostgreSQL. Only the remote Controller command producer is synthetic;
//! this does not claim native worker/platform acceptance or enable capability.
use super::*;

#[cfg(unix)]
#[path = "controller_update_native_db_tests.rs"]
mod native_db;

const ORIGIN: &str = "https://app.treer.example";
const BASE: &str = "/api/workspaces/default/machines/machine-a/controller-update";

struct HttpProxy {
    origin: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for HttpProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl HttpProxy {
    async fn start(state: AppState, auth: AuthStore) -> Self {
        Self::start_on(state, auth, "127.0.0.1:0".parse().unwrap()).await
    }
    async fn start_on(state: AppState, auth: AuthStore, address: std::net::SocketAddr) -> Self {
        let proxy = Url::parse("https://proxy.treer.example/").unwrap();
        let app = Url::parse(&format!("{ORIGIN}/")).unwrap();
        let identity = IdentityIssuer::load(&auth, &proxy).await.unwrap();
        let messages = MessageStore::open(auth.pool()).await.unwrap();
        let router = router(
            state,
            BootstrapConfig::new(
                proxy.clone(),
                PathBuf::from("dist"),
                Url::parse("https://releases.invalid/").unwrap(),
            ),
            auth,
            PolicyEngine::allow_all(),
            identity,
            BrowserAccess::new(&app, &proxy).unwrap(),
            IngressConfig::new(None, &proxy, &app).unwrap(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { origin, task }
    }
    async fn call(
        &self,
        method: reqwest::Method,
        suffix: &str,
        token: Option<&str>,
        origin: Option<&str>,
        body: Value,
    ) -> reqwest::Response {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let mut request = client
            .request(method, format!("{}{BASE}{suffix}", self.origin))
            .header("host", "proxy.treer.example");
        if let Some(token) = token {
            request = request.header("cookie", format!("treer_session={token}"));
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.json(&body).send().await.unwrap()
    }
}
async fn json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> T {
    let status = response.status();
    let cache = response.headers().get("cache-control").cloned();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(cache.unwrap(), "no-store");
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn authenticated_http_db_update_survives_proxy_restart_without_redispatch() {
    let auth = seeded().await;
    let now = Utc::now();
    sqlx::query("UPDATE users SET email_verified=TRUE WHERE id='creator'")
        .execute(&auth.pool())
        .await
        .unwrap();
    for token in ["http-session-one", "http-session-two"] {
        sqlx::query(
            "INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,'creator',$2,$3)",
        )
        .bind(token)
        .bind(now.to_rfc3339())
        .bind((now + chrono::Duration::hours(1)).to_rfc3339())
        .execute(&auth.pool())
        .await
        .unwrap();
    }
    let state = super::super::tests::state_with_managed_agent().await;
    let mut rx = attach(&state).await;
    let counter = Arc::new(AtomicUsize::new(0));
    let observed = counter.clone();
    let producer_state = state.clone();
    let producer_auth = auth.clone();
    let actor = tokio::spawn(async move {
        let mut binding = None;
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Text(text) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            let result = match envelope.command {
                AgentCommand::ControllerUpdateInspect {} => serde_json::to_value(Inspection {
                    platform: wire::Platform::LinuxX86_64,
                    current: identity('a'),
                    observed_at: Utc::now(),
                    native_ready: true,
                })
                .unwrap(),
                AgentCommand::ControllerUpdateDispatch { binding: b } => {
                    observed.fetch_add(1, Ordering::SeqCst);
                    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE operation_id=$1 AND holds_slot").bind(&b.operation_id).fetch_one(&producer_auth.pool()).await.unwrap();
                    assert_eq!(count, 1, "durable claim must precede transport dispatch");
                    let op = accepted(&b);
                    binding = Some(b);
                    serde_json::to_value(op).unwrap()
                }
                AgentCommand::ControllerUpdateOperation { operation_id } => {
                    let b = binding.as_ref().unwrap();
                    assert_eq!(operation_id, b.operation_id);
                    let mut op = accepted(b);
                    op.state = wire::State::Succeeded;
                    op.phase = Some(wire::Phase::Complete);
                    op.health = wire::Health::Healthy;
                    op.observation = wire::Observation::Available;
                    op.receipt_revision = Some(9);
                    op.observed_identity = Some(b.target.identity.clone());
                    op.updated_at = Utc::now();
                    op.observed_at = Some(op.updated_at);
                    op.finished_at = Some(op.updated_at);
                    serde_json::to_value(op).unwrap()
                }
                _ => panic!("HTTP update path must not dispatch exec, recipe or lifecycle"),
            };
            producer_state
                .complete_command(CommandResult::success(envelope.command_id, result))
                .await;
        }
    });
    let http = HttpProxy::start(state, auth.clone()).await;
    // Real middleware, not injected CurrentSession extensions.
    let denied = http
        .call(
            reqwest::Method::POST,
            "/prepare",
            None,
            Some(ORIGIN),
            json!({"target_id":"release-b"}),
        )
        .await;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let denied = http
        .call(
            reqwest::Method::POST,
            "/prepare",
            Some("http-session-one"),
            Some("https://foreign.invalid"),
            json!({"target_id":"release-b"}),
        )
        .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let preview: wire::Preview = json(
        http.call(
            reqwest::Method::POST,
            "/prepare",
            Some("http-session-one"),
            Some(ORIGIN),
            json!({"target_id":"release-b"}),
        )
        .await,
    )
    .await;
    preview.validate().unwrap();
    let confirmation = json!({"confirmation_token":preview.confirmation_token});
    // Another valid login session cannot consume this session-bound authority.
    let foreign = http
        .call(
            reqwest::Method::POST,
            "/confirm",
            Some("http-session-two"),
            Some(ORIGIN),
            confirmation.clone(),
        )
        .await;
    assert!(!foreign.status().is_success());
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    let accepted_op: wire::Operation = json(
        http.call(
            reqwest::Method::POST,
            "/confirm",
            Some("http-session-one"),
            Some(ORIGIN),
            confirmation.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(accepted_op.operation_id, preview.operation_id);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let repeat: wire::Operation = json(
        http.call(
            reqwest::Method::POST,
            "/confirm",
            Some("http-session-one"),
            Some(ORIGIN),
            confirmation.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(repeat.operation_id, accepted_op.operation_id);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    // Lose every in-memory Proxy/Controller transport object, keep the real DB.
    drop(http);
    actor.abort();
    let stopped = actor.await.unwrap_err();
    assert!(
        stopped.is_cancelled(),
        "initial actor must not have panicked"
    );
    let offline = HttpProxy::start(
        super::super::tests::state_with_managed_agent().await,
        auth.clone(),
    )
    .await;
    let discovery: wire::Discovery = json(
        offline
            .call(
                reqwest::Method::GET,
                "/operations",
                Some("http-session-one"),
                None,
                Value::Null,
            )
            .await,
    )
    .await;
    assert_eq!(discovery.operations.len(), 1);
    assert_eq!(
        discovery.operations[0].operation_id,
        accepted_op.operation_id
    );
    let offline_repeat: wire::Operation = json(
        offline
            .call(
                reqwest::Method::POST,
                "/confirm",
                Some("http-session-one"),
                Some(ORIGIN),
                confirmation,
            )
            .await,
    )
    .await;
    assert_eq!(offline_repeat.operation_id, accepted_op.operation_id);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE holds_slot")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "disconnect must not release uncertain mutation claim"
    );
    drop(offline);
    // Reconnect a protocol Controller reporting the same persisted operation;
    // only a GET observation may advance it. No second dispatch is permitted.
    let state = super::super::tests::state_with_managed_agent().await;
    let mut rx = attach(&state).await;
    let producer_state = state.clone();
    let b = DispatchBinding {
        operation_id: accepted_op.operation_id.clone(),
        workspace_id: WS.into(),
        server_id: MACHINE.into(),
        current: accepted_op.current.clone(),
        target: accepted_op.target.clone(),
        catalog_revision: 2,
        plan_digest: "a".repeat(64),
        created_at: accepted_op.created_at,
    };
    let observed = counter.clone();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let observer = tokio::spawn(async move {
        tokio::pin!(finished);
        let mut queries = 0;
        loop {
            // Drain commands already queued before accepting the end witness.
            let frame = tokio::select! {
                biased;
                frame = rx.recv() => frame.expect("reconnect command channel closed"),
                result = &mut finished => {
                    result.unwrap();
                    break;
                }
            };
            let SocketFrame::Text(text) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            if matches!(
                envelope.command,
                AgentCommand::ControllerUpdateDispatch { .. }
            ) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
            assert!(
                matches!(
                    envelope.command,
                    AgentCommand::ControllerUpdateOperation { .. }
                ),
                "reconnect must only query, never resend update"
            );
            queries += 1;
            let mut op = accepted(&b);
            op.state = wire::State::Succeeded;
            op.phase = Some(wire::Phase::Complete);
            op.health = wire::Health::Healthy;
            op.observation = wire::Observation::Available;
            op.receipt_revision = Some(9);
            op.observed_identity = Some(b.target.identity.clone());
            op.updated_at = Utc::now();
            op.observed_at = Some(op.updated_at);
            op.finished_at = Some(op.updated_at);
            producer_state
                .complete_command(CommandResult::success(
                    envelope.command_id,
                    serde_json::to_value(op).unwrap(),
                ))
                .await;
        }
        queries
    });
    let reconnected = HttpProxy::start(state, auth.clone()).await;
    let done: wire::Operation = json(
        reconnected
            .call(
                reqwest::Method::GET,
                &format!("/operations/{}", accepted_op.operation_id),
                Some("http-session-one"),
                None,
                Value::Null,
            )
            .await,
    )
    .await;
    assert_eq!(done.state, wire::State::Succeeded);
    done.validate().unwrap();
    finish.send(()).unwrap();
    let queries = tokio::time::timeout(Duration::from_secs(5), observer)
        .await
        .expect("reconnect actor must finish within the bound")
        .expect("reconnect actor assertions must reach the main test");
    assert_eq!(
        queries, 1,
        "reconnect must observe exactly one operation query"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    drop(reconnected);
    let final_http = HttpProxy::start(
        super::super::tests::state_with_managed_agent().await,
        auth.clone(),
    )
    .await;
    let final_discovery: wire::Discovery = json(
        final_http
            .call(
                reqwest::Method::GET,
                "/operations",
                Some("http-session-one"),
                None,
                Value::Null,
            )
            .await,
    )
    .await;
    assert_eq!(final_discovery.operations, vec![done]);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE holds_slot")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(count, 0, "verified terminal projection releases the slot");
}
