
use super::*;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::process::{Command, Stdio};
use tower::ServiceExt;
use treer_protocol::{
    CommandResult, MachineServiceProtocol, PolicyEffect, PolicyMode, PolicyPrincipalKind,
    PolicyPrincipalRef, ProxyMessage, WorkspacePolicyDocument, POLICY_SCHEMA_VERSION,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

async fn state_with_managed_agent() -> AppState {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let agent = treer_protocol::AgentInfo {
        agent_id: "agent-a".to_string(),
        workspace_id: "default".to_string(),
        server_id: "machine-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let mut recipient = agent.clone();
    recipient.agent_id = "agent-b".to_string();
    recipient.name = "reviewer".to_string();
    let connection_id = Uuid::new_v4();
    let (outgoing, _incoming) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, outgoing)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![agent, recipient],
            },
        )
        .await
        .expect("apply agent snapshot");
    state
}

#[tokio::test]
async fn public_workspace_updates_hide_app_runtime_agents() {
    let state = state_with_managed_agent().await;
    let mut app_agent = state
        .resolve_agent("default", "agent-a")
        .await
        .expect("resolve fixture agent");
    app_agent.agent_id = "appw-test".to_string();
    app_agent.name = "app:Docs".to_string();
    app_agent.kind = "app".to_string();
    state.test_insert_agent(app_agent.clone()).await;

    let snapshot = state.snapshot("default").await.expect("workspace snapshot");
    assert!(snapshot.agents.iter().any(|agent| agent.kind == "app"));

    let visible = visible_workspace_snapshot(snapshot);
    assert!(visible.agents.iter().all(|agent| agent.kind != "app"));
    assert_eq!(visible.agents.len(), 2);

    let event = WorkspaceEvent {
        revision: 1,
        workspace_id: "default".to_string(),
        event: "agent.updated".to_string(),
        data: serde_json::to_value(app_agent).expect("encode app agent"),
    };
    assert!(is_internal_app_agent_event(&event));
}

#[tokio::test]
async fn app_reconcile_stops_a_runtime_left_running_while_the_machine_was_offline() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".to_string(),
            CreateAppDeploymentRequest {
                server_id: Some("machine-a".to_string()),
                name: "Docs".to_string(),
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "http.server".to_string()],
                cwd: ".".to_string(),
                port: 8080,
                hostname: "docs.internal".to_string(),
                public: false,
            },
        )
        .await
        .expect("create App");
    let runtime_id = "appw_offline_stop";
    auth.claim_app_runtime("default", &app.app_id, None, runtime_id, "reconciler")
        .await
        .expect("claim runtime")
        .expect("runtime claim");
    auth.set_app_desired_state("default", &app.app_id, AppDesiredState::Stopped, "owner")
        .await
        .expect("persist stopped state");

    let state = AppState::new();
    let now = Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let runtime = AgentInfo {
        agent_id: runtime_id.to_string(),
        workspace_id: "default".to_string(),
        server_id: "machine-a".to_string(),
        kind: "app".to_string(),
        name: "app:Docs".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: Some(42),
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, server_tx)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![runtime],
            },
        )
        .await
        .expect("apply snapshot");

    let reconcile = tokio::spawn(reconcile_app_deployments_for_server(
        state.clone(),
        auth,
        "default".to_string(),
        "machine-a".to_string(),
    ));
    let command: ProxyMessage = match server_rx.recv().await.expect("stop command") {
        SocketFrame::Text(value) => serde_json::from_str(&value).expect("decode command"),
        _ => panic!("expected text command"),
    };
    let ProxyMessage::Command { envelope } = command else {
        panic!("expected command envelope");
    };
    assert!(matches!(
        envelope.command,
        AgentCommand::Stop { ref agent_id } if agent_id == runtime_id
    ));
    state
        .complete_command(CommandResult::success(envelope.command_id, json!({})))
        .await;
    reconcile.await.expect("join reconcile");
    assert!(state.resolve_agent("default", runtime_id).await.is_err());
}

fn test_config() -> BootstrapConfig {
    BootstrapConfig::new(
        Url::parse("https://treer.example/").expect("valid URL"),
        PathBuf::from("dist"),
        Url::parse("https://github.example/releases/latest/download").expect("valid release URL"),
    )
}

fn test_browser_access() -> BrowserAccess {
    BrowserAccess::new(
        &Url::parse("https://app.treer.ai/").expect("app URL"),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .expect("browser access")
}

#[test]
fn browser_tunnels_accept_app_and_proxy_origins_only() {
    let access = test_browser_access();
    for origin in ["https://app.treer.ai", "https://proxy.treer.ai"] {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        access
            .validate_tunnel_if_present(&headers)
            .expect("trusted tunnel origin");
    }
    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://attacker.example"),
    );
    assert!(access.validate_tunnel_if_present(&denied).is_err());
}

fn test_ingress_config() -> IngressConfig {
    IngressConfig::new(
        Some(Url::parse("https://apps.treer.ai/").expect("ingress URL")),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        &Url::parse("https://app.treer.ai/").expect("app URL"),
    )
    .expect("ingress config")
}

#[test]
fn app_public_url_reports_the_managed_ingress_access() {
    let config = test_ingress_config();
    let now = Utc::now();
    let mut app = AppDeployment {
        app_id: "app_1234".to_string(),
        workspace_id: "default".to_string(),
        name: "Public Docs".to_string(),
        server_id: "machine-a".to_string(),
        command: "python3".to_string(),
        args: vec![],
        cwd: ".".to_string(),
        port: 8080,
        hostname: "docs.internal".to_string(),
        service_id: "svc_docs".to_string(),
        public_url: None,
        access: None,
        desired_state: AppDesiredState::Running,
        runtime_agent_id: None,
        restart_count: 0,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: None,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
    };
    let hostname = managed_app_ingress_hostname(
        &app.name,
        &app.app_id,
        config.base_domain().expect("base domain"),
    )
    .expect("managed hostname");
    let ingress = ServiceIngress {
        ingress_id: "ing_docs".to_string(),
        workspace_id: "default".to_string(),
        service_id: app.service_id.clone(),
        hostname,
        access: ServiceIngressAccess::Public,
        enabled: true,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
    };

    attach_app_public_url(&config, &[ingress], &mut app);

    assert_eq!(app.access, Some(ServiceIngressAccess::Public));
    assert!(app
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://public-docs-")));
}

async fn admin_router(updater: crate::updater::UpdaterClient) -> Router {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        updater,
        crate::voice::VoiceServices::disabled(),
    )
}

async fn admin_cookie(app: Router) -> (Router, HeaderValue) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"password":"admin-password"}"#))
                .expect("login request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("admin cookie")
        .to_str()
        .expect("cookie text");
    let token = set_cookie.split(';').next().expect("cookie pair");
    (app, HeaderValue::from_str(token).expect("cookie header"))
}

async fn spawn_updater_sidecar() -> Url {
    let app = Router::new()
        .route(
            "/v1/status",
            get(|| async {
                Json(serde_json::json!({"channel":"stable","services":[],"job":null}))
            }),
        )
        .route(
            "/v1/apply",
            post(|| async {
                (
                    StatusCode::ACCEPTED,
                    Json(serde_json::json!({
                        "channel": "stable",
                        "job": {"id": "job1", "state": "running", "error": null}
                    })),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sidecar");
    let addr = listener.local_addr().expect("sidecar address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("sidecar server");
    });
    tokio::task::yield_now().await;
    Url::parse(&format!("http://{addr}/")).expect("sidecar URL")
}

#[test]
fn ingress_config_matches_one_wildcard_label_and_builds_urls() {
    let config = test_ingress_config();
    assert!(config.matches_hostname("demo.apps.treer.ai"));
    assert!(!config.matches_hostname("apps.treer.ai"));
    assert!(!config.matches_hostname("nested.demo.apps.treer.ai"));
    assert_eq!(
        config
            .url_for_hostname("demo.apps.treer.ai")
            .expect("ingress URL")
            .as_str(),
        "https://demo.apps.treer.ai/"
    );
}

#[tokio::test]
async fn trailing_slash_browser_tunnel_route_is_registered() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/virtual-hosts/self.test/proxy/")
                .body(Body::empty())
                .expect("tunnel request"),
        )
        .await
        .expect("route response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn workspace_deletion_route_requires_auth_and_deletes_the_workspace() {
    let auth = AuthStore::for_test("admin-password").await;
    let (invite, _) = auth
        .create_personal_invitation()
        .await
        .expect("personal invitation");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "invite": invite,
                        "email": "owner@example.com",
                        "preferred_name": "Owner",
                        "password": "password123",
                    }))
                    .expect("register body"),
                ))
                .expect("register request"),
        )
        .await
        .expect("register response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/organizations")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("organization list request"),
        )
        .await
        .expect("organization list response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("organization list body");
    let payload: Value = serde_json::from_slice(&bytes).expect("organization list JSON");
    let organization_id = payload["organizations"]
        .as_array()
        .expect("organization array")
        .first()
        .expect("personal organization")
        .get("organization_id")
        .expect("organization id")
        .as_str()
        .expect("organization id text")
        .to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "organization_id": organization_id,
                        "name": "Deletable",
                    }))
                    .expect("create workspace body"),
                ))
                .expect("create workspace request"),
        )
        .await
        .expect("create workspace response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("create workspace body");
    let payload: Value = serde_json::from_slice(&bytes).expect("create workspace JSON");
    let workspace_id = payload["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .body(Body::empty())
                .expect("unauthenticated delete"),
        )
        .await
        .expect("unauthenticated response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("authenticated delete"),
        )
        .await
        .expect("delete response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("delete body");
    let payload: Value = serde_json::from_slice(&bytes).expect("delete JSON");
    assert_eq!(payload["workspace_id"], workspace_id);
    assert_eq!(payload["name"], "Deletable");
    assert_eq!(payload["organization_id"], organization_id);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/workspaces?organization_id={organization_id}"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("workspace list request"),
        )
        .await
        .expect("list response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("list body");
    let payload: Value = serde_json::from_slice(&bytes).expect("list JSON");
    assert!(payload["workspaces"]
        .as_array()
        .expect("workspace array")
        .is_empty());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("repeat delete"),
        )
        .await
        .expect("repeat delete response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn session_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("treer_session="))
        .map(|value| value.split(';').next().expect("cookie pair").to_string())
        .expect("session cookie")
}

#[tokio::test]
async fn rollout_gates_core_message_traffic() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create rollout test enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim rollout test machine");
    let authorization = HeaderValue::from_str(&format!("Bearer {}", machine.machine_token))
        .expect("machine authorization header");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::new(false),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );

    for path in [
        "/agent/workspaces/default/messages",
        "/api/apps/svc_mail/messages",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::AUTHORIZATION, authorization.clone())
                    .body(Body::empty())
                    .expect("gated request"),
            )
            .await
            .expect("gated route response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read rollout error");
        let error: ApiError = serde_json::from_slice(&body).expect("decode rollout error");
        assert_eq!(error.error.code, "core_messages_disabled");
    }
}

#[tokio::test]
async fn machine_routes_expose_managed_apps_but_deny_direct_network_publication() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create App route enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim App route machine");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/agent/workspaces/default/apps")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .body(Body::empty())
                .expect("App list request"),
        )
        .await
        .expect("App list response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read App list response");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("decode App list"),
        json!({ "apps": [] })
    );

    for (method, path) in [
        (Method::POST, "/agent/workspaces/default/services"),
        (Method::PATCH, "/agent/workspaces/default/services/svc_old"),
        (Method::DELETE, "/agent/workspaces/default/services/svc_old"),
        (Method::POST, "/agent/workspaces/default/virtual-hosts"),
        (
            Method::DELETE,
            "/agent/workspaces/default/virtual-hosts/old.internal",
        ),
        (Method::POST, "/agent/workspaces/default/ingresses"),
        (Method::PATCH, "/agent/workspaces/default/ingresses/ing_old"),
        (
            Method::DELETE,
            "/agent/workspaces/default/ingresses/ing_old",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", machine.machine_token),
                    )
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("network mutation request"),
            )
            .await
            .expect("network mutation response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read network mutation response");
        let error: ApiError = serde_json::from_slice(&body).expect("decode network mutation error");
        assert_eq!(error.error.code, "managed_app_required", "{path}");
    }
}

#[tokio::test]
async fn workspace_ingress_redirects_humans_to_proxy_authorization() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "private app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: Some("private".to_string()),
                access: ServiceIngressAccess::Workspace,
            },
        )
        .await
        .expect("create ingress");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/dashboard?tab=active")
        .header(header::HOST, &ingress.hostname)
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(AppState::new()),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .expect("authorization redirect");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location text");
    assert!(location.starts_with("https://proxy.treer.ai/.treer/ingress/authorize?"));
    assert!(location.contains("hostname=private-"));
    assert!(location.contains("return_path=%2Fdashboard%3Ftab%3Dactive"));
}

#[test]
fn browser_access_accepts_only_the_configured_origin_when_present() {
    let browser = test_browser_access();
    assert!(browser.validate_if_present(&HeaderMap::new()).is_ok());

    let mut allowed = HeaderMap::new();
    allowed.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://app.treer.ai"),
    );
    assert!(browser.validate_if_present(&allowed).is_ok());

    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://other.treer.ai"),
    );
    let error = browser
        .validate_if_present(&denied)
        .expect_err("other browser origin must be denied");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "browser_origin_denied");
}

#[tokio::test]
async fn cors_preflight_allows_the_configured_app_with_credentials() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/auth/login")
                .header(header::ORIGIN, "https://app.treer.ai")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                .body(Body::empty())
                .expect("preflight request"),
        )
        .await
        .expect("preflight response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("https://app.treer.ai"))
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        Some(&HeaderValue::from_static("true"))
    );
    if let Some(allow_headers) = response.headers().get(header::ACCESS_CONTROL_ALLOW_HEADERS) {
        assert!(
            !allow_headers
                .to_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("x-treer-client"),
            "native client header must not be CORS-allowed"
        );
    }
}

#[tokio::test]
async fn cors_headers_are_present_on_authenticated_route_errors() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header(header::ORIGIN, "https://app.treer.ai")
                .body(Body::empty())
                .expect("authenticated request"),
        )
        .await
        .expect("authenticated response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("https://app.treer.ai"))
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        Some(&HeaderValue::from_static("true"))
    );
}

#[tokio::test]
async fn admin_update_requires_an_admin_session() {
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .body(Body::empty())
                .expect("update request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn voice_asr_status_requires_authentication() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/asr")
                .body(Body::empty())
                .expect("voice asr status request"),
        )
        .await
        .expect("voice asr status response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn voice_command_status_requires_authentication() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/command")
                .body(Body::empty())
                .expect("voice command status request"),
        )
        .await
        .expect("voice command status response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

async fn voice_member_token(auth: &AuthStore, app: Router, workspace_id: &str) -> (Router, String) {
    let (invite, _) = auth
        .create_personal_invitation()
        .await
        .expect("personal invitation");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/register")
                .header("X-Treer-Client", "mobile")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "email": "voice@example.com",
                        "preferred_name": "Voice",
                        "password": "password123",
                        "invite": invite,
                    }))
                    .expect("register body"),
                ))
                .expect("register request"),
        )
        .await
        .expect("register response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("register body bytes"),
    )
    .expect("register json");
    let token = body["token"].as_str().expect("native token").to_string();
    let user_id = body["user_id"].as_str().expect("user id");
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES($1, $2, 'owner', $3)",
    )
    .bind(format!("org_{workspace_id}"))
    .bind(user_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&auth.pool())
    .await
    .expect("workspace membership");
    (app, token)
}

#[tokio::test]
async fn voice_command_is_unavailable_until_configured() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let state = AppState::new();
    state.ensure_workspace("default", "default").await;
    let app = router(
        state,
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let (app, token) = voice_member_token(&auth, app, "default").await;
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("status request"),
        )
        .await
        .expect("status response");
    assert_eq!(status.status(), StatusCode::OK);
    let status_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(status.into_body(), usize::MAX)
            .await
            .expect("status body"),
    )
    .expect("status json");
    assert_eq!(status_body["enabled"], false);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"text":"看一下 mac 上的 reviewer"}"#))
                .expect("command request"),
        )
        .await
        .expect("command response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: ApiError = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body"),
    )
    .expect("error json");
    assert_eq!(error.error.code, "voice_llm_unavailable");
}

#[tokio::test]
async fn voice_command_turns_asr_text_into_agent_prompt() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let state = AppState::new();
    state.ensure_workspace("default", "lab").await;
    let now = Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "srv_mac".to_string(),
        workspace_id: "default".to_string(),
        name: "mac".to_string(),
        hostname: "MacBook-Pro.local".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let agent = treer_protocol::AgentInfo {
        agent_id: "ag_reviewer".to_string(),
        workspace_id: "default".to_string(),
        server_id: "srv_mac".to_string(),
        kind: "codex".to_string(),
        name: "reviewer".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let connection_id = Uuid::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, tx)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![agent],
            },
        )
        .await
        .expect("snapshot");
    let responder = state.clone();
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Text(encoded) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } =
                serde_json::from_str(&encoded).expect("command")
            else {
                continue;
            };
            responder
                .complete_command(CommandResult::success(
                    envelope.command_id,
                    json!({"ok": true}),
                ))
                .await;
        }
    });
    let upstream = crate::voice_llm::spawn_scripted_upstream(vec![
            json!({
                "id": "resp_1",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "treer",
                    "arguments": "{\"argv\":[\"agent\",\"prompt\",\"--machine\",\"mac\",\"reviewer\",\"给这个仓库写测试\"]}"
                }]
            }),
            json!({
                "id": "resp_2",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"已经把写测试的任务发给 Mac 上的 reviewer 了。"}]
                }]
            }),
        ])
        .await;
    let llm = crate::voice_llm::VoiceLlmConfig::for_test(
        upstream.as_str(),
        crate::voice_llm::WireApi::Responses,
        "sk-test",
        "gpt-5.6-luna",
    );
    let app = router(
        state,
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::with_llm(llm),
    );
    let (app, token) = voice_member_token(&auth, app, "default").await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"text":"请看一下 mac 这个设备上运行的 reviewer agent，让它进行写测试"}"#,
                ))
                .expect("command request"),
        )
        .await
        .expect("command response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("command body"),
    )
    .expect("command json");
    assert!(body["reply"].as_str().expect("reply").contains("reviewer"));
    assert_eq!(body["tools"][0]["ok"], true);
    assert_eq!(body["tools"][0]["argv"][1], "prompt");
}

#[tokio::test]
async fn admin_update_is_unconfigured_without_a_sidecar() {
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let (app, cookie) = admin_cookie(app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("update request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let error: ApiError = serde_json::from_slice(&body).expect("decode");
    assert_eq!(error.error.code, "updater_unconfigured");
}

#[tokio::test]
async fn admin_update_forwards_to_the_sidecar() {
    let sidecar = spawn_updater_sidecar().await;
    let updater =
        crate::updater::UpdaterClient::new(sidecar, "secret".to_string()).expect("client");
    let app = admin_router(updater).await;
    let (app, cookie) = admin_cookie(app).await;
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .expect("status request"),
        )
        .await
        .expect("status response");
    assert_eq!(status.status(), StatusCode::OK);
    let apply = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("apply request"),
        )
        .await
        .expect("apply response");
    assert_eq!(apply.status(), StatusCode::ACCEPTED);
}

#[test]
fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
    let config = test_config();
    let url = install_script_url(&config.public_url);
    assert_eq!(url.as_str(), "https://treer.example/install.sh");
    assert!(url.query().is_none());
}

#[tokio::test]
async fn legacy_enrollment_requests_without_identity_remain_supported() {
    let auth = AuthStore::for_test("admin-password").await;
    let enrollment = auth
        .create_machine_enrollment("default", "admin")
        .await
        .expect("create enrollment");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {enrollment}")).expect("enrollment authorization"),
    );
    let response = enroll_machine(State(AppState::new()), Extension(auth), headers, None)
        .await
        .unwrap_or_else(|error| panic!("legacy enrollment: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn bootstrap_separates_public_installation_from_workspace_connection() {
    let config = test_config();
    let key = "enr_v1_64656661756c74_abc.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let (install, connect) = bootstrap_commands(&config.public_url, key);
    assert_eq!(
        install,
        "curl -fsSL 'https://treer.example/install.sh' | sh"
    );
    assert!(!install.contains("enr_"));
    assert!(!install.contains("connect"));
    assert_eq!(
        connect,
        format!("treer-agent-server connect --key '{key}' --proxy 'https://treer.example/'")
    );
    assert!(!connect.contains("install.sh"));
}

#[test]
fn installer_is_posix_shell_and_only_installs_binaries() {
    let config = test_config();
    let script = render_install_script(&config.public_url);
    assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
    assert!(script.contains("platform=linux-aarch64"));
    assert!(script.contains("transparent agent networking requires unshare(1)"));
    assert!(script.contains("persistent proxy and agent host"));
    assert!(script.contains("container, or other sandbox"));
    assert!(script.contains(".local/libexec/treer"));
    assert!(script.contains("treer-agent-host"));
    assert!(script
        .contains("ln -sf \"$server_dir/treer-agent-server\" \"$install_dir/treer-agent-server\""));
    assert!(script.contains("https://treer.example/artifacts"));
    assert!(!script.contains("service --workspace"));
    assert!(!script.contains("machine_token"));
    assert!(!script.contains("TREER_MACHINE_TOKEN"));
    assert!(!script.contains("TREER_ENROLLMENT_KEY"));
    assert!(!script.contains("systemctl"));
    assert!(!script.contains("launchctl"));
    assert!(!script.contains("nohup"));
}

#[cfg(unix)]
#[test]
fn rendered_installer_has_valid_shell_syntax() {
    let config = test_config();
    let script = render_install_script(&config.public_url);
    let mut child = Command::new("sh")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .expect("start shell parser");
    child
        .stdin
        .take()
        .expect("shell stdin")
        .write_all(script.as_bytes())
        .expect("write installer");
    assert!(child.wait().expect("wait for shell parser").success());
}

#[test]
fn artifact_paths_reject_directory_traversal() {
    assert!(valid_artifact_component("linux-aarch64"));
    assert!(!valid_artifact_component("../linux-aarch64"));
    assert!(!valid_artifact_component("linux/aarch64"));
}

#[test]
fn release_artifact_names_match_tagged_assets() {
    let config = test_config();
    let url = release_artifact_url(&config, "darwin-aarch64", "treer-agent-host")
        .unwrap_or_else(|_| panic!("release artifact URL"));
    assert_eq!(
        url.as_str(),
        "https://github.example/releases/latest/download/treer-agent-host-darwin-aarch64"
    );
}

#[tokio::test]
async fn missing_local_artifacts_redirect_to_the_release() {
    let mut config = test_config();
    config.artifacts_dir = std::env::temp_dir().join(format!(
        "treer-missing-artifacts-{}",
        Uuid::new_v4().simple()
    ));
    let response = download_artifact(
        Extension(config),
        Path(("darwin-aarch64".to_string(), "treer".to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("missing artifact should redirect"));
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://github.example/releases/latest/download/treer-darwin-aarch64")
    );
}

#[test]
fn display_names_are_trimmed_and_validated() {
    assert_eq!(
        normalize_display_name("  build machine  ".to_string()).expect("valid name"),
        "build machine"
    );
    assert!(normalize_display_name("  ".to_string()).is_err());
    assert!(normalize_display_name("bad\nname".to_string()).is_err());
    assert!(normalize_display_name("x".repeat(81)).is_err());
}

#[test]
fn launch_profiles_become_interactive_shell_requests() {
    let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
    let profile = AgentLaunchProfile {
        profile_id: "alp_review".to_string(),
        workspace_id: "default".to_string(),
        name: "Reviewer".to_string(),
        description: String::new(),
        cwd: "packages/api".to_string(),
        command: "codex".to_string(),
        args: vec![
            "review".to_string(),
            "--base".to_string(),
            "main".to_string(),
        ],
        created_at: timestamp,
        created_by: "user".to_string(),
        updated_at: timestamp,
        updated_by: "user".to_string(),
    };
    let request = agent_request_from_launch_profile(
        &profile,
        LaunchAgentProfileRequest {
            server_id: Some("machine-a".to_string()),
            agent_name: None,
            cwd: Some("reviews/42".to_string()),
            cols: 100,
            rows: 30,
        },
    )
    .expect("build create request");
    assert_eq!(request.server_id.as_deref(), Some("machine-a"));
    assert_eq!(request.kind, "shell");
    assert_eq!(request.name, "Reviewer");
    assert_eq!(request.cwd, "reviews/42");
    assert_eq!(request.args, ["codex", "review", "--base", "main"]);
    assert_eq!((request.cols, request.rows), (100, 30));

    let request = agent_request_from_launch_profile(
        &profile,
        LaunchAgentProfileRequest {
            server_id: None,
            agent_name: None,
            cwd: None,
            cols: 120,
            rows: 36,
        },
    )
    .expect("build request with profile cwd");
    assert_eq!(request.cwd, "packages/api");
}

#[test]
fn machine_principals_cannot_target_another_machine() {
    let subject = PolicySubject::Machine {
        server_id: "machine-a".to_string(),
    };
    assert!(require_machine_target(Some(&subject), "machine-a").is_ok());
    let error = require_machine_target(Some(&subject), "machine-b")
        .expect_err("cross-machine operation must require Agent identity");
    assert_eq!(error.error.code, "agent_identity_required");
}

#[test]
fn same_machine_agents_can_probe_sibling_services() {
    let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
    let service = MachineService {
        service_id: "svc_ui".to_string(),
        workspace_id: "default".to_string(),
        name: "codex-ui".to_string(),
        server_id: "machine-a".to_string(),
        target_agent_id: Some("ag_ui".to_string()),
        target_host: "127.0.0.1".to_string(),
        target_port: 4173,
        protocol: MachineServiceProtocol::Http,
        created_at: timestamp,
        created_by: "agent:ag_ui".to_string(),
        updated_at: timestamp,
        updated_by: "agent:ag_ui".to_string(),
    };
    let installer = PolicySubject::Agent {
        server_id: "machine-a".to_string(),
        agent_id: "ag_installer".to_string(),
    };
    let other_machine = PolicySubject::Agent {
        server_id: "machine-b".to_string(),
        agent_id: "ag_other".to_string(),
    };
    assert!(require_agent_can_probe_service(&installer, &service).is_ok());
    assert_eq!(
        require_agent_can_probe_service(&other_machine, &service)
            .expect_err("cross-machine probe")
            .error
            .code,
        "service_not_owned"
    );
}

#[tokio::test]
async fn agent_policy_subject_is_bound_to_authenticated_machine() {
    let state = state_with_managed_agent().await;
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let missing = agent_policy_subject(&state, &machine, &HeaderMap::new(), "default")
        .await
        .expect_err("agent identity is required");
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.error.code, "invalid_agent_identity");

    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let subject = agent_policy_subject(&state, &machine, &headers, "default")
        .await
        .unwrap_or_else(|error| panic!("matching subject: {}", error.error.message));
    assert_eq!(
        subject,
        PolicySubject::Agent {
            server_id: "machine-a".to_string(),
            agent_id: "agent-a".to_string(),
        }
    );

    let error = agent_policy_subject(
        &state,
        &MachineSession {
            server_id: Some("machine-b".to_string()),
            workspace_id: Some("default".to_string()),
        },
        &headers,
        "default",
    )
    .await
    .expect_err("foreign machine must not claim the agent");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "policy_subject_mismatch");
}

#[tokio::test]
async fn agent_identity_tokens_use_the_canonical_service_audience() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test",
            CreateMachineServiceRequest {
                name: "api".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

    let response = agent_issue_identity_token(
        State(state.clone()),
        Extension(WorkloadIdentityApi {
            auth: auth.clone(),
            policy: PolicyEngine::allow_all(),
            issuer: identity.clone(),
        }),
        Extension(machine),
        headers,
        Path("default".to_string()),
        Json(WorkloadIdentityTokenRequest {
            audience: "api".to_string(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("issue identity token: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read token response");
    let token: treer_protocol::WorkloadIdentityTokenResponse =
        serde_json::from_slice(&body).expect("decode token response");
    assert_eq!(token.audience, service.service_id);
    let verified = identity.verify(&token.access_token, &service.service_id);
    assert!(verified.active);
    let claims = verified.claims.expect("verified claims");
    assert_eq!(claims.sub, "agent-a");
    assert_eq!(claims.machine_id, "machine-a");

    let mut app_headers = HeaderMap::new();
    app_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token))
            .expect("App authorization header"),
    );
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &app_headers, &service.service_id)
            .await
            .expect("authenticate Agent App Message identity");
    assert_eq!(workspace_id, "default");
    assert_eq!(principal.id, "agent-a");
    assert!(matches!(
        subject,
        PolicySubject::Agent { server_id, agent_id }
            if server_id == "machine-a" && agent_id == "agent-a"
    ));
}

#[test]
fn app_recipients_share_one_agent_and_human_namespace() {
    let principals = vec![
        AppPrincipal {
            kind: AppPrincipalKind::Agent,
            id: "agent-reviewer".to_string(),
            name: "reviewer".to_string(),
            role: None,
        },
        AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: "user-owner".to_string(),
            name: "Owner".to_string(),
            role: Some("owner".to_string()),
        },
        AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: "user-reviewer".to_string(),
            name: "reviewer".to_string(),
            role: Some("member".to_string()),
        },
    ];
    let owner = resolve_app_principal(&principals, "Owner").expect("unique human name");
    assert_eq!(owner.kind, AppPrincipalKind::Human);
    assert_eq!(owner.id, "user-owner");
    let stable = resolve_app_principal(&principals, "agent-reviewer").expect("stable Agent ID");
    assert_eq!(stable.kind, AppPrincipalKind::Agent);
    let ambiguous =
        resolve_app_principal(&principals, "reviewer").expect_err("ambiguous display name");
    assert_eq!(ambiguous.code, "recipient_ambiguous");
}

#[tokio::test]
async fn message_api_pins_durable_policy_and_hides_recipient_resolution_details() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO users(id, email, email_verified, preferred_name, password_hash, created_at) \
             VALUES('user-reviewer', 'reviewer@example.test', TRUE, 'reviewer', 'unused', $1)",
    )
    .bind(&now)
    .execute(&auth.pool())
    .await
    .expect("seed duplicate-name human");
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES('org_default', 'user-reviewer', 'member', $1)",
    )
    .bind(&now)
    .execute(&auth.pool())
    .await
    .expect("seed duplicate-name membership");

    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let policy_store = WorkspacePolicyStore::new(auth.pool());
    let deny_send = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::from([(ACTION_MESSAGE_SEND.to_string(), PolicyEffect::Deny)]),
        groups: BTreeMap::new(),
        rules: Vec::new(),
    };
    let actor = PolicyPrincipalRef {
        kind: PolicyPrincipalKind::Human,
        id: "policy-owner".to_string(),
    };
    let monitor = policy_store
        .replace(
            "default",
            0,
            PolicyMode::Monitor,
            deny_send.clone(),
            actor.clone(),
        )
        .await
        .expect("install monitor policy");
    assert_eq!(monitor.revision, 1);

    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let secret_body = "api-policy-body-must-not-enter-metadata";
    let request = |recipient: &str, key: &str, body: &str| SendMessageRequest {
        recipients: vec![recipient.to_string()],
        context_ids: Vec::new(),
        body: body.to_string(),
        expires_at: None,
        idempotency_key: Some(key.to_string()),
        correlation_id: Some("cor_api_policy".to_string()),
        trace_id: Some("trace_api_policy".to_string()),
        external_source: None,
    };

    let sent = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::durable(policy_store.clone())),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("agent-b", "api-monitor-send", secret_body)),
    )
    .await
    .expect("monitor policy must observe rather than deny");

    let envelope: Value = sqlx::query_scalar(
        "SELECT envelope FROM core_message_outbox \
             WHERE workspace_id = 'default' AND action = 'message.created' \
             ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&auth.pool())
    .await
    .expect("load Message outbox envelope");
    assert_eq!(envelope["resource"]["id"], sent.0.message.message_id);
    assert_eq!(envelope["workspace_revision"], monitor.revision);
    assert!(!envelope.to_string().contains(secret_body));

    let audit_payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM organization_audit_events")
            .fetch_all(&auth.pool())
            .await
            .expect("load audit payloads");
    assert!(
        audit_payloads
            .iter()
            .all(|payload| !payload.contains(secret_body)),
        "Message bodies must not enter audit payloads"
    );

    let nonexistent = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("missing-recipient", "api-missing", "missing body")),
    )
    .await
    .expect_err("nonexistent recipient must be hidden");
    let duplicate = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("reviewer", "api-duplicate", "duplicate body")),
    )
    .await
    .expect_err("duplicate-name recipient must be hidden");

    let enforce = policy_store
        .replace(
            "default",
            monitor.revision,
            PolicyMode::Enforce,
            deny_send,
            actor,
        )
        .await
        .expect("enforce policy");
    assert_eq!(enforce.revision, 2);
    let hidden = send_core_message(
        State(state),
        Extension(auth.clone()),
        Extension(PolicyEngine::durable(policy_store)),
        Extension(messages),
        Extension(machine),
        Path("default".to_string()),
        headers,
        Json(request("agent-b", "api-hidden", "hidden body")),
    )
    .await
    .expect_err("enforced policy must hide the recipient");

    for error in [&nonexistent, &duplicate, &hidden] {
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.error.code, "message_recipient_unavailable");
        assert_eq!(
            error.error.message,
            "a recipient does not exist or is not available to this sender"
        );
    }
    let stored_messages: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'default'")
            .fetch_one(&auth.pool())
            .await
            .expect("count stored Messages");
    assert_eq!(stored_messages, 1, "all denied sends must be atomic");
}

#[tokio::test]
async fn browser_tunnel_rejects_tcp_services_before_opening_a_stream() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "database".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                target_host: "127.0.0.1".to_string(),
                target_port: 5432,
                protocol: treer_protocol::MachineServiceProtocol::Tcp,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "database.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let error = proxy_virtual_network_host(
        AppState::new(),
        auth,
        "default".to_string(),
        "database.internal".to_string(),
        String::new(),
        Request::new(Body::empty()),
    )
    .await
    .expect_err("TCP services must not enter the HTTP tunnel");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.error.code, "service_protocol_mismatch");
}

#[tokio::test]
async fn browser_tunnel_forwards_http_without_leaking_gateway_credentials() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "app.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        assert_eq!(open.kind, treer_protocol::NetworkBinaryKind::Open);
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");

        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        assert!(request.starts_with("GET /status?full=1 HTTP/1.1\r\n"));
        assert!(request.contains("host: app.internal\r\n"));
        assert!(!request.to_ascii_lowercase().contains("cookie:"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));

        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nSet-Cookie: internal=secret\r\n\r\nhello".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });

    let request = Request::builder()
        .uri("/ignored?full=1")
        .header(header::COOKIE, "treer_session=secret")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .expect("browser request");
    let response = proxy_virtual_network_host(
        state,
        auth,
        "default".to_string(),
        "app.internal".to_string(),
        "status".to_string(),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("tunnel request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("read tunnel response");
    assert_eq!(&body[..], b"hello");
    controller.await.expect("join controller");
}

#[tokio::test]
async fn public_ingress_preserves_application_auth_and_strips_treer_headers() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "controller-test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "0.1.2".to_string(),
            git_commit: "host-test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "public app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: Some("demo".to_string()),
                access: ServiceIngressAccess::Public,
            },
        )
        .await
        .expect("create ingress");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");
        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        let lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /api/items?limit=2 HTTP/1.1\r\n"));
        assert!(lower.contains("authorization: bearer application-token\r\n"));
        assert!(lower.contains("cookie: app_session=visible\r\n"));
        assert!(!lower.contains("treer_ingress"));
        assert!(!lower.contains("x-treer-spoofed"));
        assert!(lower.contains("host: demo-"));
        assert!(lower.contains(".apps.treer.ai\r\n"));
        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nSet-Cookie: app_session=updated; Path=/\r\n\r\nok".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/api/items?limit=2")
        .header(header::HOST, &ingress.hostname)
        .header(header::AUTHORIZATION, "Bearer application-token")
        .header(
            header::COOKIE,
            "__Host-treer_ingress=private; app_session=visible",
        )
        .header("x-treer-spoofed", "false")
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(state),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("ingress request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::SET_COOKIE),
        Some(&HeaderValue::from_static("app_session=updated; Path=/"))
    );
    controller.await.expect("join controller");
}
