use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind};

async fn relay_fixture(
    workspace: &str,
) -> (
    AuthStore,
    AppState,
    MachineService,
    String,
    MachineSession,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    uuid::Uuid,
) {
    let auth = AuthStore::for_test("owner-password").await;
    auth.seed_test_workspace(workspace).await;
    let enrollment = auth
        .create_machine_enrollment(workspace, "owner")
        .await
        .unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    let state = AppState::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let connection = uuid::Uuid::new_v4();
    let build = treer_protocol::BuildInfo {
        version: "test".into(),
        git_commit: "test".into(),
    };
    state
        .register_server(
            treer_protocol::ServerInfo {
                server_id: machine.server_id.clone(),
                workspace_id: workspace.into(),
                name: "test".into(),
                hostname: "test".into(),
                root: "/tmp".into(),
                controller_build: build.clone(),
                host_build: build,
                supervision: None,
                labels: Default::default(),
                available_agents: None,
                status: treer_protocol::ServerStatus::Online,
                connected_at: Utc::now(),
                last_seen_at: Utc::now(),
                capabilities: vec![],
            },
            connection,
            sender,
        )
        .await
        .unwrap();
    let service = auth
        .create_machine_service(
            workspace,
            "owner",
            CreateMachineServiceRequest {
                name: "echo".into(),
                server_id: machine.server_id.clone(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".into(),
                target_port: 3000,
                protocol: treer_protocol::MachineServiceProtocol::Tcp,
            },
        )
        .await
        .unwrap();
    let session = MachineSession {
        server_id: Some(machine.server_id),
        workspace_id: Some(workspace.into()),
    };
    (
        auth,
        state,
        service,
        machine.machine_token,
        session,
        receiver,
        connection,
    )
}

/// Readiness waits for a Controller-bound frame the test caused. Under correct
/// behavior the frame arrives; its absence is exactly what the test must still
/// detect, so the bound is one generous absolute deadline per expected
/// condition that excludes a hung test without pacing a loaded machine (#128:
/// fixed 5 s waits elapsed at a load average of 16 while the suite took
/// 249 s). The deadline is computed once per condition and passed through
/// every skip loop, so irrelevant frames never extend the condition's budget.
/// Nothing is retried and no assertion changes.
const CONTROLLER_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

fn frame_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + CONTROLLER_FRAME_DEADLINE
}

async fn await_frame(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    deadline: tokio::time::Instant,
) -> SocketFrame {
    tokio::time::timeout_at(deadline, receiver.recv())
        .await
        .expect("the Controller-bound frame this test caused did not arrive before its deadline")
        .expect("the fixture Controller connection stays open")
}

async fn next_frame(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    deadline: tokio::time::Instant,
) -> NetworkBinaryFrame {
    loop {
        match await_frame(receiver, deadline).await {
            SocketFrame::Binary(data) => return NetworkBinaryFrame::decode(&data).unwrap(),
            _ => continue,
        }
    }
}

async fn relay(
    state: &AppState,
    service: &MachineService,
    connection: uuid::Uuid,
    id: &str,
    kind: NetworkBinaryKind,
    payload: &[u8],
) {
    state
        .relay_network_frame(
            &service.workspace_id,
            &service.server_id,
            connection,
            NetworkBinaryFrame {
                kind,
                stream_id: id.into(),
                payload: payload.to_vec(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn machine_restore_route_authenticates_scopes_and_reports_waiting_endpoints() {
    use crate::policy::{PolicyDecision, PolicyDenial};
    use tower::ServiceExt;
    let (auth, state, service, token, _session, mut receiver, _connection) =
        relay_fixture("restore-route").await;
    auth.seed_test_workspace("other-workspace").await;
    let mut other = state
        .resolve_server(&service.workspace_id, &service.server_id)
        .await
        .unwrap();
    other.server_id = "different-machine".into();
    other.name = "different-machine".into();
    let (other_sender, _other_receiver) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(other, uuid::Uuid::new_v4(), other_sender)
        .await
        .unwrap();
    let url = Url::parse("https://proxy.example/").unwrap();
    let browser_url = Url::parse("https://app.example/").unwrap();
    let bootstrap = BootstrapConfig::new(
        url.clone(),
        PathBuf::from("dist"),
        Url::parse("https://example.test/releases/").unwrap(),
    );
    let identity = IdentityIssuer::load(&auth, &url).await.unwrap();
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let browser = BrowserAccess::new(&browser_url, &url).unwrap();
    let ingress = IngressConfig::new(None, &url, &browser_url).unwrap();
    let make_router = |policy| {
        router(
            state.clone(),
            bootstrap.clone(),
            auth.clone(),
            policy,
            identity.clone(),
            browser.clone(),
            ingress.clone(),
            messages.clone(),
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
            crate::voice::VoiceServices::disabled(),
        )
    };
    let app = make_router(PolicyEngine::allow_all());
    let path = format!(
        "/agent/workspaces/{}/machines/{}/restore",
        service.workspace_id, service.server_id
    );
    let request = |path: &str, authenticated: bool| {
        let mut builder = Request::builder().method("POST").uri(path);
        if authenticated {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(request(&path, false))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let wrong_workspace = format!(
        "/agent/workspaces/other-workspace/machines/{}/restore",
        service.server_id
    );
    assert_eq!(
        app.clone()
            .oneshot(request(&wrong_workspace, true))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let wrong_machine = format!(
        "/agent/workspaces/{}/machines/different-machine/restore",
        service.workspace_id
    );
    assert_eq!(
        app.clone()
            .oneshot(request(&wrong_machine, true))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let credential = auth
        .create_agent_credential(&service.workspace_id, &service.server_id, "agent-restore")
        .await
        .unwrap();
    let agent_request = Request::builder()
        .method("POST")
        .uri(&path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(AGENT_ID_HEADER, "agent-restore")
        .header(treer_protocol::WORKLOAD_CREDENTIAL_HEADER, credential)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(agent_request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let denied = make_router(PolicyEngine::new(
        PolicyDecision::Deny(PolicyDenial::new("restore_denied", "restore denied")),
        vec![],
    ));
    assert_eq!(
        denied.oneshot(request(&path, true)).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        receiver.try_recv().is_err(),
        "denied requests must not send Controller commands"
    );
    let responder_state = state.clone();
    let workspace = service.workspace_id.clone();
    let server_id = service.server_id.clone();
    let responder = tokio::spawn(async move {
        let mut actions = Vec::new();
        let deadline = frame_deadline();
        while actions.len() < 2 {
            let frame = await_frame(&mut receiver, deadline).await;
            let SocketFrame::Text(encoded) = frame else {
                continue;
            };
            let treer_protocol::ProxyMessage::Command { envelope } =
                serde_json::from_str(&encoded).unwrap()
            else {
                continue;
            };
            let value = match envelope.command {
                AgentCommand::RestoreRuntime {} => {
                    actions.push("restore");
                    json!({"workspace_id":workspace,"server_id":server_id,"items":[]})
                }
                AgentCommand::ProbeNetwork { .. } => {
                    actions.push("probe");
                    json!({"healthy":false,"error":"external process has not started"})
                }
                command => panic!("unexpected command {command:?}"),
            };
            responder_state
                .complete_command(treer_protocol::CommandResult::success(
                    envelope.command_id,
                    value,
                ))
                .await;
        }
        actions
    });
    let response = app.oneshot(request(&path, true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let report: Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(report["items"][0]["workload_id"], service.service_id);
    assert_eq!(report["items"][0]["state"], "waiting_endpoint");
    assert_eq!(responder.await.unwrap(), vec!["restore", "probe"]);
}

#[tokio::test]
async fn private_service_socket_by_name_uses_stable_policy_id_and_revocation() {
    private_service_transport_revocation(false).await;
}

#[tokio::test]
async fn private_service_websocket_preserves_half_close_and_policy_revocation() {
    private_service_transport_revocation(true).await;
}

async fn private_service_transport_revocation(websocket: bool) {
    use crate::policy::{
        PolicyDecision, PolicyDenial, PolicyEvaluation, PolicyEvaluator, PolicyFuture,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Revocable(Arc<AtomicBool>, String);
    impl PolicyEvaluator for Revocable {
        fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a> {
            Box::pin(async move {
                assert_eq!(request.resource.id, self.1);
                Ok(PolicyEvaluation::Decide(if self.0.load(Ordering::SeqCst) {
                    PolicyDecision::Deny(PolicyDenial::new("revoked", "revoked"))
                } else {
                    PolicyDecision::Allow
                }))
            })
        }
    }
    let (auth, state, service, token, session, mut receiver, connection) =
        relay_fixture("private-socket").await;
    let revoked = Arc::new(AtomicBool::new(false));
    let policy = PolicyEngine::new(
        PolicyDecision::Allow,
        vec![Arc::new(Revocable(
            revoked.clone(),
            service.service_id.clone(),
        ))],
    );
    let app = Router::new()
        .route(
            "/workspaces/{workspace_id}/services/{service_id}/connect",
            get(agent_connect_machine_service).post(agent_connect_machine_service),
        )
        .layer(Extension(auth))
        .layer(Extension(policy))
        .layer(Extension(session))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!(
        "http://{address}/workspaces/{}/services/{}/connect",
        service.workspace_id,
        service.name.to_uppercase()
    );
    let request = tokio::spawn(async move {
        if websocket {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut request = url
                .replacen("http://", "ws://", 1)
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
            Ok(tokio_tungstenite::connect_async(request).await.unwrap().0)
        } else {
            Err(reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .header("connection", "upgrade")
                .header("upgrade", "treer-tcp")
                .send()
                .await
                .unwrap())
        }
    });
    let open = next_frame(&mut receiver, frame_deadline()).await;
    assert_eq!(open.kind, NetworkBinaryKind::Open);
    let connect: treer_protocol::NetworkConnectRequest =
        serde_json::from_slice(&open.payload).unwrap();
    assert_eq!(connect.source_server_id, service.server_id);
    relay(
        &state,
        &service,
        connection,
        &open.stream_id,
        NetworkBinaryKind::Opened,
        &[],
    )
    .await;
    let response = match request.await.unwrap() {
        Ok(mut client) => {
            use tokio_tungstenite::tungstenite::Message as WireMessage;
            client
                .send(WireMessage::Binary(b"ping".to_vec().into()))
                .await
                .unwrap();
            assert_eq!(
                next_frame(&mut receiver, frame_deadline()).await.payload,
                b"ping"
            );
            client
                .send(WireMessage::Text(treer_protocol::SERVICE_STREAM_EOF.into()))
                .await
                .unwrap();
            let half_close = next_frame(&mut receiver, frame_deadline()).await;
            assert_eq!(half_close.kind, NetworkBinaryKind::HalfClose);
            relay(
                &state,
                &service,
                connection,
                &open.stream_id,
                NetworkBinaryKind::Data,
                b"pong",
            )
            .await;
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                WireMessage::Binary(b"pong".to_vec().into())
            );
            revoked.store(true, Ordering::SeqCst);
            let closed = tokio::time::timeout(std::time::Duration::from_secs(3), client.next())
                .await
                .unwrap();
            assert!(matches!(
                closed,
                None | Some(Err(_)) | Some(Ok(WireMessage::Close(_)))
            ));
            let deadline = frame_deadline();
            let reset = loop {
                let frame = next_frame(&mut receiver, deadline).await;
                if frame.kind == NetworkBinaryKind::Reset {
                    break frame;
                }
            };
            assert_eq!(reset.stream_id, open.stream_id);
            server.abort();
            return;
        }
        Err(response) => response,
    };
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let mut client = response.upgrade().await.unwrap();
    client.write_all(b"ping").await.unwrap();
    assert_eq!(
        next_frame(&mut receiver, frame_deadline()).await.payload,
        b"ping"
    );
    revoked.store(true, Ordering::SeqCst);
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), client.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let deadline = frame_deadline();
    let reset = loop {
        let frame = next_frame(&mut receiver, deadline).await;
        if frame.kind == NetworkBinaryKind::Reset {
            break frame;
        }
    };
    assert_eq!(reset.stream_id, open.stream_id);
    server.abort();
}

#[test]
fn agent_service_mutations_require_both_bound_machine_and_agent() {
    let service = MachineService {
        service_id: "svc_test".into(),
        workspace_id: "workspace".into(),
        name: "preview".into(),
        server_id: "machine-a".into(),
        target_agent_id: Some("agent-a".into()),
        primary: false,
        target_host: "127.0.0.1".into(),
        target_port: 3000,
        protocol: treer_protocol::MachineServiceProtocol::Http,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        created_by: "agent-a".into(),
        updated_by: "agent-a".into(),
    };
    assert!(require_service_owner(
        &PolicySubject::Agent {
            server_id: "machine-a".into(),
            agent_id: "agent-a".into()
        },
        &service
    )
    .is_ok());
    for (machine, agent) in [("machine-a", "agent-b"), ("machine-b", "agent-a")] {
        assert!(require_service_owner(
            &PolicySubject::Agent {
                server_id: machine.into(),
                agent_id: agent.into()
            },
            &service
        )
        .is_err());
    }
    assert!(require_service_owner(
        &PolicySubject::Machine {
            server_id: "machine-b".into()
        },
        &service
    )
    .is_err());
}

#[tokio::test]
#[ignore = "requires isolated database and an explicitly selected public TCP range"]
async fn public_tcp_allocation_is_stable_and_explicitly_revocable() {
    let (auth, state, service, _token, _session, mut receiver, connection) =
        relay_fixture("tcp-test").await;
    spawn_public_tcp_listeners(state.clone(), auth.clone());
    let (start, _) = public_tcp_range().unwrap();
    for _ in 0..20 {
        if public_tcp_ready().read().unwrap().contains(&start) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let config = IngressConfig::new(
        None,
        &Url::parse("https://proxy.example/").unwrap(),
        &Url::parse("https://app.example/").unwrap(),
    )
    .unwrap();
    let publish = || {
        publish_public_tcp_service(
            Extension(auth.clone()),
            Extension(config.clone()),
            Path(("tcp-test".into(), service.service_id.clone())),
            Json(json!({"access":"public"})),
        )
    };
    let first = publish().await.unwrap().0;
    let second = publish().await.unwrap().0;
    assert_eq!(first["port"], second["port"]);
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE action='ingress.tcp.publish'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "repeat publication is not a state change");
    assert_eq!(first["hostname"], "proxy.example");
    let _ = unpublish_public_tcp_service(
        Extension(auth.clone()),
        Path(("tcp-test".into(), service.service_id.clone())),
    )
    .await
    .unwrap();
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM public_tcp_services WHERE service_id=$1")
            .bind(&service.service_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert!(!enabled);
    let _ = unpublish_public_tcp_service(
        Extension(auth.clone()),
        Path(("tcp-test".into(), service.service_id.clone())),
    )
    .await
    .unwrap();
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE action='ingress.tcp.unpublish'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "repeat revocation is not a state change");
    assert_eq!(publish().await.unwrap().0["port"], first["port"]);
    let port = first["port"].as_u64().unwrap() as u16;
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let open = next_frame(&mut receiver, frame_deadline()).await;
    relay(
        &state,
        &service,
        connection,
        &open.stream_id,
        NetworkBinaryKind::Opened,
        &[],
    )
    .await;
    client.write_all(b"request").await.unwrap();
    assert_eq!(
        next_frame(&mut receiver, frame_deadline()).await.payload,
        b"request"
    );
    client.shutdown().await.unwrap();
    assert_eq!(
        next_frame(&mut receiver, frame_deadline()).await.kind,
        NetworkBinaryKind::HalfClose
    );
    relay(
        &state,
        &service,
        connection,
        &open.stream_id,
        NetworkBinaryKind::Data,
        b"response after EOF",
    )
    .await;
    relay(
        &state,
        &service,
        connection,
        &open.stream_id,
        NetworkBinaryKind::HalfClose,
        &[],
    )
    .await;
    let mut data = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.read_to_end(&mut data),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(data, b"response after EOF");
    // Drain the completed stream's acknowledgements before opening another.
    while let Ok(SocketFrame::Binary(_)) = receiver.try_recv() {}
    let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let deadline = frame_deadline();
    let open = loop {
        let frame = next_frame(&mut receiver, deadline).await;
        if frame.kind == NetworkBinaryKind::Open {
            break frame;
        }
    };
    relay(
        &state,
        &service,
        connection,
        &open.stream_id,
        NetworkBinaryKind::Opened,
        &[],
    )
    .await;
    let _ = unpublish_public_tcp_service(
        Extension(auth.clone()),
        Path(("tcp-test".into(), service.service_id.clone())),
    )
    .await
    .unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), client.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn raw_public_tcp_is_refused_for_the_active_policy_provider_service() {
    let (auth, _state, service, _token, _session, _receiver, _connection) =
        relay_fixture("tcp-provider").await;
    let config = IngressConfig::new(
        None,
        &Url::parse("https://proxy.example/").unwrap(),
        &Url::parse("https://app.example/").unwrap(),
    )
    .unwrap();
    let app = auth
        .create_app_deployment(
            "tcp-provider",
            "owner",
            service.server_id.clone(),
            treer_protocol::CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some(service.server_id.clone()),
                name: "Policy".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8888,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let service = auth
        .resolve_machine_service("tcp-provider", &app.service_id)
        .await
        .unwrap();
    crate::policy_provider_store::PolicyProviderStore::new(auth.pool())
        .set(
            "tcp-provider",
            &app.app_id,
            &service.service_id,
            treer_protocol::PolicyProviderFailureMode::FailClosed,
            0,
            0,
            "owner",
        )
        .await
        .unwrap();
    assert!(public_tcp::is_active_policy_provider_service(
        &auth.pool(),
        "tcp-provider",
        &service.service_id
    )
    .await
    .unwrap());
    assert!(!public_tcp::is_active_policy_provider_service(
        &auth.pool(),
        "other",
        &service.service_id
    )
    .await
    .unwrap());
    let error = publish_public_tcp_service(
        Extension(auth.clone()),
        Extension(config),
        Path(("tcp-provider".into(), service.service_id.clone())),
        Json(json!({"access":"public"})),
    )
    .await
    .expect_err("provider service must stay private");
    assert_eq!(error.error.code, "policy_provider_must_be_private");
    let mapped: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public_tcp_services WHERE service_id=$1)")
            .bind(&service.service_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert!(!mapped);
}

#[tokio::test]
async fn concurrent_provider_selection_and_raw_tcp_publication_are_mutually_exclusive() {
    let (auth, _state, service, _token, _session, _receiver, _connection) =
        relay_fixture("tcp-race").await;
    let app = auth
        .create_app_deployment(
            "tcp-race",
            "owner",
            service.server_id.clone(),
            treer_protocol::CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some(service.server_id.clone()),
                name: "Policy".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8888,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let pool = auth.pool();
    // Selection in flight (uncommitted) while a publication of the same service starts.
    let mut selecting = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO workspace_policy_providers(workspace_id,app_id,service_id,updated_at,updated_by) VALUES('tcp-race',$1,$2,'now','owner')")
        .bind(&app.app_id)
        .bind(&app.service_id)
        .execute(&mut *selecting)
        .await
        .unwrap();
    let publishing = {
        let pool = pool.clone();
        let service_id = app.service_id.clone();
        tokio::spawn(async move {
            sqlx::query("INSERT INTO public_tcp_services(service_id,public_port,enabled) VALUES($1,39991,TRUE)")
                .bind(service_id)
                .execute(&pool)
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !publishing.is_finished(),
        "publication must wait for the selection transaction"
    );
    selecting.commit().await.unwrap();
    let error = publishing
        .await
        .unwrap()
        .expect_err("publication must be rejected after selection");
    assert!(error
        .to_string()
        .contains("policy_provider_must_be_private"));
    // Reverse order: an enabled mapping blocks later selection.
    sqlx::query("DELETE FROM workspace_policy_providers WHERE workspace_id='tcp-race'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO public_tcp_services(service_id,public_port,enabled) VALUES($1,39992,TRUE)",
    )
    .bind(&app.service_id)
    .execute(&pool)
    .await
    .unwrap();
    let error = sqlx::query("INSERT INTO workspace_policy_providers(workspace_id,app_id,service_id,updated_at,updated_by) VALUES('tcp-race',$1,$2,'now','owner')")
        .bind(&app.app_id)
        .bind(&app.service_id)
        .execute(&pool)
        .await
        .expect_err("selection must be rejected while raw TCP is enabled");
    assert!(error
        .to_string()
        .contains("policy_provider_must_be_private"));
}
