//! Public ingress protection (#146) at the handler level against isolated
//! PostgreSQL and a controllable fake upstream behind the machine tunnel:
//! admission (client bucket before service bucket, in-flight cap), body cap
//! and framing checks, upstream timeouts versus long polls, the verified
//! client address header, workspace ingress exemptions, the owner override
//! route with its ceilings and audit, and the negative hostname cache.
use super::*;
use crate::ingress_guard::{IngressGuard, TrustedProxies};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use treer_protocol::{
    NetworkBinaryFrame, NetworkBinaryKind, ServerInfo, ServiceIngressAccess,
    UpdateIngressLimitsRequest, INGRESS_DEFAULT_IP_RATE_BURST, INGRESS_DEFAULT_RATE_BURST,
    TREER_CLIENT_IP_HEADER,
};

const HOSTS: &str = "https://apps.treer.example/";

pub(super) struct Fixture {
    pub(super) auth: AuthStore,
    pub(super) identity: IdentityIssuer,
    pub(super) app: AppDeployment,
    pub(super) state: AppState,
    pub(super) connection: Uuid,
    pub(super) rx: Option<tokio::sync::mpsc::UnboundedReceiver<SocketFrame>>,
}

pub(super) async fn fixture() -> Fixture {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let now = Utc::now();
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused',$1),('member','member@example.test',TRUE,'Member','unused',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner','owner',$1),('org_default','member','member',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    let identity =
        IdentityIssuer::load(&auth, &Url::parse("https://proxy.treer.example/").unwrap())
            .await
            .unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Inbox".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 9440,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let state = AppState::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let connection = Uuid::new_v4();
    let build = treer_protocol::BuildInfo {
        version: "test".into(),
        git_commit: "test".into(),
    };
    state
        .register_server(
            ServerInfo {
                server_id: "machine-a".into(),
                workspace_id: "default".into(),
                name: "machine-a".into(),
                hostname: "machine-a".into(),
                root: "/tmp".into(),
                controller_build: build.clone(),
                host_build: build,
                supervision: None,
                labels: Default::default(),
                available_agents: None,
                status: ServerStatus::Online,
                connected_at: now,
                last_seen_at: now,
            },
            connection,
            tx,
        )
        .await
        .unwrap();
    Fixture {
        auth,
        identity,
        app,
        state,
        connection,
        rx: Some(rx),
    }
}

pub(super) fn config(guard: Option<IngressGuard>) -> IngressConfig {
    let config = IngressConfig::new(
        Some(Url::parse(HOSTS).unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    match guard {
        Some(guard) => config.with_guard(guard),
        None => config,
    }
}

#[derive(Clone, Copy)]
pub(super) enum Upstream {
    /// Answer 200 after reading the request head (and body up to a sentinel).
    Immediate,
    /// Answer 200 after the delay.
    Delayed(Duration),
    /// Open the stream but never answer.
    Silent,
}

/// Fake machine side of the tunnel: serves every stream the Proxy opens.
/// Returns the request heads it saw (headers only, bodies dropped) and a
/// count of streams served.
pub(super) fn upstream(
    state: AppState,
    connection: Uuid,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    mode: Upstream,
) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let served = Arc::new(AtomicUsize::new(0));
    let (heads_out, served_out) = (heads.clone(), served.clone());
    tokio::spawn(async move {
        let mut buffers: std::collections::HashMap<String, String> = Default::default();
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Binary(bytes) = frame else {
                continue;
            };
            let Ok(frame) = NetworkBinaryFrame::decode(&bytes) else {
                continue;
            };
            match frame.kind {
                NetworkBinaryKind::Open => {
                    served.fetch_add(1, Ordering::SeqCst);
                    state
                        .relay_network_frame(
                            "default",
                            "machine-a",
                            connection,
                            NetworkBinaryFrame {
                                kind: NetworkBinaryKind::Opened,
                                stream_id: frame.stream_id.clone(),
                                payload: vec![],
                            },
                        )
                        .await
                        .unwrap();
                    buffers.insert(frame.stream_id, String::new());
                }
                NetworkBinaryKind::Data => {
                    let Some(buffer) = buffers.get_mut(&frame.stream_id) else {
                        continue;
                    };
                    let was_complete = buffer.contains("\r\n\r\n");
                    buffer.push_str(&String::from_utf8_lossy(&frame.payload));
                    if was_complete || !buffer.contains("\r\n\r\n") {
                        continue;
                    }
                    heads.lock().unwrap().push(buffer.clone());
                    let stream_id = frame.stream_id.clone();
                    let state = state.clone();
                    let respond = async move {
                        match mode {
                            Upstream::Silent => return,
                            Upstream::Delayed(delay) => tokio::time::sleep(delay).await,
                            Upstream::Immediate => {}
                        }
                        for (kind, payload) in [
                            (
                                NetworkBinaryKind::Data,
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                                    .to_vec(),
                            ),
                            (NetworkBinaryKind::HalfClose, vec![]),
                        ] {
                            let _ = state
                                .relay_network_frame(
                                    "default",
                                    "machine-a",
                                    connection,
                                    NetworkBinaryFrame {
                                        kind,
                                        stream_id: stream_id.clone(),
                                        payload,
                                    },
                                )
                                .await;
                        }
                    };
                    tokio::spawn(respond);
                }
                _ => {}
            }
        }
    });
    (heads_out, served_out)
}

pub(super) fn with_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let address: IpAddr = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::new(address, 40000)));
    request
}

async fn send(
    fixture: &Fixture,
    config: &IngressConfig,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    proxy_service_ingress(
        State(fixture.state.clone()),
        Extension(fixture.auth.clone()),
        Extension(PolicyEngine::new(
            crate::policy::PolicyDecision::Allow,
            vec![],
        )),
        Extension(config.clone()),
        Extension(fixture.identity.clone()),
        request,
    )
    .await
}

fn get(host: &str, peer: &str) -> Request<Body> {
    with_peer(
        Request::builder()
            .uri("/v1/outbox")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap(),
        peer,
    )
}

#[tokio::test]
async fn public_ingress_admits_per_client_then_per_service_and_answers_429() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    // Refill rates of one token per minute (the lowest an owner may set) keep
    // the bursts exact however slowly a loaded runner drives the loops: the
    // default buckets refill one token per 100 ms (service) and 500 ms (client),
    // which turned an exact count into a race (#149). Burst sizes stay default.
    fixture
        .auth
        .update_service_ingress_limits(
            "default",
            &ingress.ingress_id,
            "owner",
            &UpdateIngressLimitsRequest {
                rate_limit_per_minute: Some(Some(1)),
                ip_rate_limit_per_minute: Some(Some(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (heads, _) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Immediate,
    );
    let config = config(None);
    // Client A spends its burst; client B is untouched.
    for _ in 0..INGRESS_DEFAULT_IP_RATE_BURST {
        let response = send(&fixture, &config, get(&ingress.hostname, "203.0.113.1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    let refused = send(&fixture, &config, get(&ingress.hostname, "203.0.113.1"))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = refused.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(retry >= 1);
    assert_eq!(refused.headers()[header::CACHE_CONTROL], "no-store");
    let body = axum::body::to_bytes(refused.into_body(), 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "ingress_rate_limited");
    assert!(!String::from_utf8_lossy(&body).contains("203.0.113"));
    let ok = send(&fixture, &config, get(&ingress.hostname, "203.0.113.2"))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let counters = config.guard().counters(&ingress.ingress_id);
    assert_eq!(counters.ip_rate_limited, 1);
    assert_eq!(
        counters.rate_limited, 0,
        "the service budget was not drained"
    );
    // The refused request never reached the machine.
    assert_eq!(
        heads.lock().unwrap().len(),
        INGRESS_DEFAULT_IP_RATE_BURST as usize + 1
    );
    // The client address reaches the App as X-Treer-Client-IP and a spoofed
    // one never does.
    let spoof = with_peer(
        Request::builder()
            .uri("/v1/outbox")
            .header(header::HOST, &ingress.hostname)
            .header(TREER_CLIENT_IP_HEADER, "9.9.9.9")
            .header("x-forwarded-for", "8.8.8.8")
            .body(Body::empty())
            .unwrap(),
        "203.0.113.3",
    );
    axum::body::to_bytes(
        send(&fixture, &config, spoof).await.unwrap().into_body(),
        64,
    )
    .await
    .unwrap();
    let last = heads
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        last.contains("x-treer-client-ip: 203.0.113.3\r\n"),
        "{last}"
    );
    assert!(
        !last.contains("9.9.9.9") && !last.contains("8.8.8.8"),
        "{last}"
    );
    assert!(!last.contains("x-forwarded-for"), "{last}");

    // The service bucket is the backstop against many clients.
    let guard = IngressGuard::new(TrustedProxies::none());
    let config = config_with(&guard);
    let mut refused = 0;
    for n in 0..(INGRESS_DEFAULT_RATE_BURST + 5) {
        let peer = format!("198.51.{}.{}", n / 200 + 1, n % 200 + 1);
        let response = send(&fixture, &config, get(&ingress.hostname, &peer))
            .await
            .unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            refused += 1;
        } else {
            axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
        }
    }
    assert_eq!(refused, 5);
    assert_eq!(guard.counters(&ingress.ingress_id).rate_limited, 5);
}

fn config_with(guard: &IngressGuard) -> IngressConfig {
    config(Some(guard.clone()))
}

#[tokio::test]
async fn public_ingress_caps_bodies_refuses_ambiguous_framing_and_keeps_permits_bounded() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    // Lower the in-flight cap and the body cap as an owner would.
    fixture
        .auth
        .update_service_ingress_limits(
            "default",
            &ingress.ingress_id,
            "owner",
            &UpdateIngressLimitsRequest {
                concurrency_limit: Some(Some(2)),
                max_body_bytes: Some(Some(1024)),
                response_timeout_secs: Some(Some(5)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (heads, served) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Delayed(Duration::from_millis(400)),
    );
    let config = config(None);
    let guard = config.guard();

    // Declared oversize: refused before any tunnel stream opens.
    let big = with_peer(
        Request::builder()
            .method("POST")
            .uri("/v1/inbox")
            .header(header::HOST, &ingress.hostname)
            .header(header::CONTENT_LENGTH, "4096")
            .body(Body::from(vec![b'x'; 4096]))
            .unwrap(),
        "203.0.113.10",
    );
    let error = send(&fixture, &config, big).await.unwrap_err();
    assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error.error.code, "ingress_body_too_large");
    assert_eq!(served.load(Ordering::SeqCst), 0, "no tunnel opened");

    // Streamed oversize (no Content-Length): cut at the cap, reported as 413.
    let stream = futures_util::stream::iter(
        (0..8).map(|_| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b'y'; 512]))),
    );
    let chunked = with_peer(
        Request::builder()
            .method("POST")
            .uri("/v1/inbox")
            .header(header::HOST, &ingress.hostname)
            .header(header::TRANSFER_ENCODING, "chunked")
            .body(Body::from_stream(stream))
            .unwrap(),
        "203.0.113.11",
    );
    let error = send(&fixture, &config, chunked).await.unwrap_err();
    assert_eq!(
        error.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        error.error.code
    );
    assert_eq!(guard.counters(&ingress.ingress_id).body_too_large, 2);

    // Both framing headers at once: refused.
    let smuggle = with_peer(
        Request::builder()
            .method("POST")
            .uri("/v1/inbox")
            .header(header::HOST, &ingress.hostname)
            .header(header::CONTENT_LENGTH, "2")
            .header(header::TRANSFER_ENCODING, "chunked")
            .body(Body::from("ok"))
            .unwrap(),
        "203.0.113.12",
    );
    let error = send(&fixture, &config, smuggle).await.unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.error.code, "ingress_request_rejected");
    assert_eq!(guard.counters(&ingress.ingress_id).request_rejected, 1);

    // In-flight cap: two slow requests hold the permits, the third is 429
    // and nothing above the cap reaches the machine; permits return after
    // the responses complete.
    let served_before = served.load(Ordering::SeqCst);
    let first = tokio::spawn({
        let (fixture_state, auth, identity, config, host) = (
            fixture.state.clone(),
            fixture.auth.clone(),
            fixture.identity.clone(),
            config.clone(),
            ingress.hostname.clone(),
        );
        async move {
            let response = proxy_service_ingress(
                State(fixture_state),
                Extension(auth),
                Extension(PolicyEngine::new(
                    crate::policy::PolicyDecision::Allow,
                    vec![],
                )),
                Extension(config),
                Extension(identity),
                get(&host, "203.0.113.20"),
            )
            .await
            .unwrap();
            let status = response.status();
            axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            status
        }
    });
    let second = tokio::spawn({
        let (fixture_state, auth, identity, config, host) = (
            fixture.state.clone(),
            fixture.auth.clone(),
            fixture.identity.clone(),
            config.clone(),
            ingress.hostname.clone(),
        );
        async move {
            let response = proxy_service_ingress(
                State(fixture_state),
                Extension(auth),
                Extension(PolicyEngine::new(
                    crate::policy::PolicyDecision::Allow,
                    vec![],
                )),
                Extension(config),
                Extension(identity),
                get(&host, "203.0.113.21"),
            )
            .await
            .unwrap();
            let status = response.status();
            axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            status
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let third = send(&fixture, &config, get(&ingress.hostname, "203.0.113.22"))
        .await
        .unwrap();
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(third.into_body(), 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("ingress_concurrency_limited"));
    assert_eq!(first.await.unwrap(), StatusCode::OK);
    assert_eq!(second.await.unwrap(), StatusCode::OK);
    assert_eq!(served.load(Ordering::SeqCst) - served_before, 2);
    tokio::task::yield_now().await;
    assert_eq!(guard.in_flight(&ingress.ingress_id), 0, "permits released");
    assert_eq!(guard.counters(&ingress.ingress_id).concurrency_limited, 1);
    // A 400 ms "long poll" passes under the 5 s response timeout.
    let polled = send(&fixture, &config, get(&ingress.hostname, "203.0.113.23"))
        .await
        .unwrap();
    assert_eq!(polled.status(), StatusCode::OK);
    assert!(heads.lock().unwrap().len() >= 3);
}

#[tokio::test]
async fn public_ingress_times_out_a_silent_service_and_frees_its_permit() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    fixture
        .auth
        .update_service_ingress_limits(
            "default",
            &ingress.ingress_id,
            "owner",
            &UpdateIngressLimitsRequest {
                response_timeout_secs: Some(Some(5)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Silent,
    );
    let config = config(None);
    let started = std::time::Instant::now();
    let error = send(&fixture, &config, get(&ingress.hostname, "203.0.113.30"))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error.error.code, "ingress_upstream_timeout");
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        config
            .guard()
            .counters(&ingress.ingress_id)
            .upstream_timeout,
        1
    );
    tokio::task::yield_now().await;
    assert_eq!(config.guard().in_flight(&ingress.ingress_id), 0);
}

#[tokio::test]
async fn trusted_front_header_drives_client_limits_and_workspace_ingress_is_exempt() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    let (heads, _) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Immediate,
    );
    let trusted =
        TrustedProxies::parse(&["10.0.0.0/8".into()], Some("CF-Connecting-IP"), false).unwrap();
    let guard = IngressGuard::new(trusted);
    let config = config_with(&guard);
    let via_front = |client: &'static str| {
        with_peer(
            Request::builder()
                .uri("/v1/outbox")
                .header(header::HOST, &ingress.hostname)
                .header("cf-connecting-ip", client)
                .body(Body::empty())
                .unwrap(),
            "10.0.0.5",
        )
    };
    for _ in 0..INGRESS_DEFAULT_IP_RATE_BURST {
        let response = send(&fixture, &config, via_front("198.51.100.1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    assert_eq!(
        send(&fixture, &config, via_front("198.51.100.1"))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS,
        "limited by the header-supplied client"
    );
    assert_eq!(
        send(&fixture, &config, via_front("198.51.100.2"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "another client behind the same front is fine"
    );
    let last = heads
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        last.contains("x-treer-client-ip: 198.51.100.2\r\n"),
        "{last}"
    );
    assert!(
        !last.contains("cf-connecting-ip"),
        "the front's header is not forwarded: {last}"
    );
    // The same header from an untrusted peer is ignored: the peer is the client.
    let direct = with_peer(
        Request::builder()
            .uri("/v1/outbox")
            .header(header::HOST, &ingress.hostname)
            .header("cf-connecting-ip", "198.51.100.1")
            .body(Body::empty())
            .unwrap(),
        "203.0.113.40",
    );
    let response = send(&fixture, &config, direct).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    let last = heads
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        last.contains("x-treer-client-ip: 203.0.113.40\r\n"),
        "{last}"
    );

    // Workspace ingress: no client or service rate limit, no body cap, no
    // client IP header; the Agent token path is unchanged.
    let workspace = fixture
        .auth
        .update_service_ingress(
            "default",
            &ingress.ingress_id,
            "owner",
            treer_protocol::UpdateServiceIngressRequest {
                access: Some(ServiceIngressAccess::Workspace),
                enabled: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(workspace.access, ServiceIngressAccess::Workspace);
    let agent = fixture
        .identity
        .issue("default", "machine-a", "agent-a", &fixture.app.service_id)
        .unwrap()
        .access_token;
    for _ in 0..(INGRESS_DEFAULT_IP_RATE_BURST + 5) {
        let request = with_peer(
            Request::builder()
                .method("POST")
                .uri("/v1/upload")
                .header(header::HOST, &ingress.hostname)
                .header(TREER_AUTHORIZATION_HEADER, format!("Bearer {agent}"))
                .header(header::CONTENT_LENGTH, "4194304")
                .body(Body::from(vec![b'z'; 4 * 1024 * 1024]))
                .unwrap(),
            "203.0.113.50",
        );
        let response = send(&fixture, &config, request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    let last = heads
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
        .to_ascii_lowercase();
    assert!(!last.contains("x-treer-client-ip"), "{last}");
    assert_eq!(guard.counters(&ingress.ingress_id).body_too_large, 0);
}

#[tokio::test]
async fn owner_override_is_owner_only_bounded_audited_and_listed() {
    let fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    let config = config(None);
    let patch = |user: &str, body: Value| {
        let (state, auth, config, app) = (
            fixture.state.clone(),
            fixture.auth.clone(),
            config.clone(),
            fixture.app.app_id.clone(),
        );
        let session = CurrentSession {
            token: format!("session-{user}"),
            user_id: user.into(),
            email: format!("{user}@example.test"),
            preferred_name: user.into(),
        };
        async move {
            super::apps::update_app_ingress_limits(
                State(state),
                Extension(auth),
                Extension(config),
                Extension(session),
                HeaderMap::new(),
                Path(("default".into(), app)),
                Json(serde_json::from_value(body).unwrap()),
            )
            .await
        }
    };
    let error = patch("member", json!({"concurrency_limit": 8}))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    for (field, value) in [
        ("rate_limit_per_minute", json!(60001)),
        ("rate_limit_per_minute", json!(0)),
        ("concurrency_limit", json!(5000)),
        ("max_body_bytes", json!(64 * 1024 * 1024 + 1)),
        ("response_timeout_secs", json!(4)),
        ("response_timeout_secs", json!(601)),
    ] {
        let error = patch("owner", json!({ field: value })).await.unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::BAD_REQUEST, "ingress_limit_invalid"),
            "{field}={value}"
        );
    }
    assert!(
        serde_json::from_value::<UpdateIngressLimitsRequest>(json!({"unknown": 1})).is_err(),
        "unknown override fields are refused"
    );
    let updated = patch(
        "owner",
        json!({"concurrency_limit": 8, "max_body_bytes": 2048, "response_timeout_secs": 120}),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(updated["ingress"]["limits"]["concurrency_limit"], 8);
    assert_eq!(
        updated["ingress"]["effective_limits"]["concurrency_limit"],
        8
    );
    assert_eq!(
        updated["ingress"]["effective_limits"]["rate_limit_per_minute"],
        600
    );
    assert_eq!(updated["ingress"]["rejections"]["rate_limited"], 0);
    // Clearing with null returns to the default.
    let cleared = patch("owner", json!({"concurrency_limit": null}))
        .await
        .unwrap()
        .0;
    assert!(cleared["ingress"]["limits"]
        .get("concurrency_limit")
        .is_none());
    assert_eq!(
        cleared["ingress"]["effective_limits"]["concurrency_limit"],
        64
    );
    assert_eq!(
        cleared["ingress"]["effective_limits"]["max_body_bytes"],
        2048
    );
    let audit = crate::audit::list(
        &fixture.auth.pool(),
        "org_default",
        Some("default"),
        None,
        100,
    )
    .await
    .unwrap();
    let rows: Vec<_> = audit
        .iter()
        .filter(|event| event.action == "ingress.limits.update")
        .collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.actor_id.as_deref(), Some("owner"));
        assert_eq!(row.resource_id, ingress.ingress_id);
        assert!(row.payload.get("hostname").is_none());
        assert_eq!(row.payload["max_body_bytes"], 2048);
    }
    assert!(
        rows.iter().any(|row| row.payload["concurrency_limit"] == 8),
        "the first update recorded the override value"
    );
    assert!(
        rows.iter()
            .any(|row| row.payload.get("concurrency_limit").is_none()),
        "the clearing update recorded no override"
    );
    // The projected workspace view keeps only the numeric fields.
    let page = crate::audit::list_workspace(
        &fixture.auth.pool(),
        "default",
        &treer_protocol::WorkspaceAuditEventsQuery::default(),
    )
    .await
    .unwrap();
    let projected: Vec<_> = page
        .events
        .iter()
        .filter(|event| event.action == "ingress.limits.update")
        .collect();
    assert_eq!(projected.len(), 2);
    assert!(projected
        .iter()
        .any(|event| event.payload["concurrency_limit"] == 8));
    assert!(projected
        .iter()
        .all(|event| event.payload.get("ingress_id").is_none()
            && event.payload["max_body_bytes"] == 2048));
    // Listing shows the limits for every ingress.
    let listed = super::network::list_service_ingresses(
        Extension(fixture.auth.clone()),
        Extension(config.clone()),
        Path("default".into()),
    )
    .await
    .unwrap()
    .0;
    let entry = listed["ingresses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["ingress_id"] == ingress.ingress_id)
        .unwrap();
    assert_eq!(entry["effective_limits"]["max_body_bytes"], 2048);
    assert!(entry["rejections_since"].is_string());
}

#[tokio::test]
async fn unknown_hostnames_are_answered_from_the_snapshot_without_database_lookups() {
    let fixture = fixture().await;
    fixture.auth.refresh_service_ingresses().await.unwrap();
    // A row written behind the cache's back is invisible until the next
    // refresh: the resolver did not go to the database.
    sqlx::query("INSERT INTO machine_services(service_id, workspace_id, name, server_id, target_host, target_port, protocol, created_at, created_by, updated_at, updated_by) VALUES('svc_hidden','default','hidden','machine-a','127.0.0.1',9000,'http',$1,'owner',$1,'owner')")
        .bind(Utc::now().to_rfc3339()).execute(&fixture.auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO service_ingresses(ingress_id, workspace_id, service_id, hostname, access, enabled, created_at, created_by, updated_at, updated_by) VALUES('ing_hidden','default','svc_hidden','hidden.apps.treer.example','public',TRUE,$1,'owner',$1,'owner')")
        .bind(Utc::now().to_rfc3339()).execute(&fixture.auth.pool()).await.unwrap();
    assert!(fixture
        .auth
        .resolve_service_ingress_hostname("hidden.apps.treer.example")
        .await
        .unwrap()
        .is_none());
    for n in 0..200 {
        assert!(fixture
            .auth
            .resolve_service_ingress_hostname(&format!("random-{n}.apps.treer.example"))
            .await
            .unwrap()
            .is_none());
    }
    fixture.auth.refresh_service_ingresses().await.unwrap();
    assert!(fixture
        .auth
        .resolve_service_ingress_hostname("hidden.apps.treer.example")
        .await
        .unwrap()
        .is_some());
    // The global per-peer bucket refuses a flood before resolution.
    let config = config(None);
    let mut refused = 0;
    for _ in 0..(crate::ingress_guard::GLOBAL_PEER_BURST + 20) {
        let request = with_peer(
            Request::builder()
                .uri("/")
                .header(header::HOST, "nobody.apps.treer.example")
                .body(Body::empty())
                .unwrap(),
            "203.0.113.99",
        );
        match send(&fixture, &config, request).await {
            Ok(response) if response.status() == StatusCode::TOO_MANY_REQUESTS => refused += 1,
            Err(error) => assert_eq!(error.error.code, "ingress_not_found"),
            Ok(other) => panic!("unexpected {}", other.status()),
        }
    }
    assert_eq!(refused, 20);
}

/// #142's consumer requirement: a public-ingress long poll of a full 30 s
/// (the App's maximum) passes the default 35 s response timeout through the
/// real handler and tunnel, holding exactly one in-flight permit meanwhile.
/// Deliberately takes 30 s of wall time; it is the evidence apps-dev asked for.
#[tokio::test]
async fn public_ingress_thirty_second_long_poll_passes_the_default_timeout() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    let (heads, served) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Delayed(Duration::from_secs(30)),
    );
    let config = config(None);
    let started = std::time::Instant::now();
    let poll = tokio::spawn({
        let (state, auth, identity, config, host) = (
            fixture.state.clone(),
            fixture.auth.clone(),
            fixture.identity.clone(),
            config.clone(),
            ingress.hostname.clone(),
        );
        async move {
            let request = with_peer(
                Request::builder()
                    .uri("/v1/outbox?after=0&timeout=30")
                    .header(header::HOST, &host)
                    .body(Body::empty())
                    .unwrap(),
                "203.0.113.60",
            );
            let response = proxy_service_ingress(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::new(
                    crate::policy::PolicyDecision::Allow,
                    vec![],
                )),
                Extension(config),
                Extension(identity),
                request,
            )
            .await
            .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            (status, body)
        }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        config.guard().in_flight(&ingress.ingress_id),
        1,
        "the poll holds one permit while it waits"
    );
    let (status, body) = poll.await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"ok");
    assert!(elapsed >= Duration::from_secs(30), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(35), "{elapsed:?}");
    assert_eq!(served.load(Ordering::SeqCst), 1);
    let head = heads.lock().unwrap()[0].to_ascii_lowercase();
    assert!(
        head.starts_with("get /v1/outbox?after=0&timeout=30 http/1.1\r\n"),
        "{head}"
    );
    assert!(head.contains("x-treer-client-ip: 203.0.113.60\r\n"));
    tokio::task::yield_now().await;
    assert_eq!(config.guard().in_flight(&ingress.ingress_id), 0);
    assert_eq!(
        config
            .guard()
            .counters(&ingress.ingress_id)
            .upstream_timeout,
        0
    );
}

/// Security P3 (#146 3022): through the real server module, a WebSocket
/// upgrade on public ingress reaches the App with the verified client address
/// only; the client's own x-treer-client-ip / X-Forwarded-For never arrive.
#[tokio::test]
async fn websocket_upgrade_through_the_real_server_strips_client_address_headers() {
    let mut fixture = fixture().await;
    let ingress = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            "apps.treer.example",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    // Fake upstream that completes the upgrade and echoes nothing further.
    let state = fixture.state.clone();
    let connection = fixture.connection;
    let mut rx = fixture.rx.take().unwrap();
    let heads: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let heads_task = heads.clone();
    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut stream_id = None;
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Binary(bytes) = frame else {
                continue;
            };
            let Ok(frame) = NetworkBinaryFrame::decode(&bytes) else {
                continue;
            };
            match frame.kind {
                NetworkBinaryKind::Open => {
                    stream_id = Some(frame.stream_id.clone());
                    state
                        .relay_network_frame(
                            "default",
                            "machine-a",
                            connection,
                            NetworkBinaryFrame {
                                kind: NetworkBinaryKind::Opened,
                                stream_id: frame.stream_id,
                                payload: vec![],
                            },
                        )
                        .await
                        .unwrap();
                }
                NetworkBinaryKind::Data => {
                    buffer.push_str(&String::from_utf8_lossy(&frame.payload));
                    if buffer.contains("\r\n\r\n") && heads_task.lock().unwrap().is_empty() {
                        heads_task.lock().unwrap().push(buffer.clone());
                        let _ = state
                            .relay_network_frame(
                                "default",
                                "machine-a",
                                connection,
                                NetworkBinaryFrame {
                                    kind: NetworkBinaryKind::Data,
                                    stream_id: stream_id.clone().unwrap(),
                                    payload: b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n".to_vec(),
                                },
                            )
                            .await;
                    }
                }
                NetworkBinaryKind::HalfClose | NetworkBinaryKind::Reset => {
                    // A real App closes its side when the client is gone.
                    if let Some(id) = stream_id.clone() {
                        let _ = state
                            .relay_network_frame(
                                "default",
                                "machine-a",
                                connection,
                                NetworkBinaryFrame {
                                    kind: NetworkBinaryKind::HalfClose,
                                    stream_id: id,
                                    payload: vec![],
                                },
                            )
                            .await;
                    }
                }
                _ => {}
            }
        }
    });
    let config = config(None);
    let router = Router::new()
        .fallback(axum::routing::any(proxy_service_ingress))
        .layer(Extension(fixture.auth.clone()))
        .layer(Extension(PolicyEngine::allow_all()))
        .layer(Extension(config.clone()))
        .layer(Extension(fixture.identity.clone()))
        .with_state(fixture.state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        crate::server::serve(
            listener,
            router,
            crate::server::ServerLimits::default(),
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
        .unwrap();
    });
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "GET /ws HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nX-Treer-Client-IP: 9.9.9.9\r\nX-Forwarded-For: 8.8.8.8\r\nx-treer-identity-token: forged\r\n\r\n",
        ingress.hostname
    );
    tokio::io::AsyncWriteExt::write_all(&mut client, request.as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 1024];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let read = tokio::time::timeout_at(
            deadline,
            tokio::io::AsyncReadExt::read(&mut client, &mut buffer),
        )
        .await
        .expect("upgrade response")
        .unwrap();
        response.extend_from_slice(&buffer[..read]);
        if read == 0 || response.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&response);
    assert!(text.starts_with("HTTP/1.1 101"), "{text}");
    let head = heads.lock().unwrap()[0].to_ascii_lowercase();
    assert!(head.starts_with("get /ws http/1.1\r\n"), "{head}");
    assert!(head.contains("upgrade: websocket"), "{head}");
    assert!(
        head.contains("x-treer-client-ip: 127.0.0.1\r\n"),
        "verified peer: {head}"
    );
    assert!(
        !head.contains("9.9.9.9") && !head.contains("8.8.8.8"),
        "{head}"
    );
    assert!(
        !head.contains("x-forwarded-for") && !head.contains("forged"),
        "{head}"
    );
    // The upgraded connection holds its in-flight permit until it closes.
    assert_eq!(config.guard().in_flight(&ingress.ingress_id), 1);
    drop(client);
    let released = tokio::time::Instant::now() + Duration::from_secs(5);
    while config.guard().in_flight(&ingress.ingress_id) != 0 {
        assert!(
            tokio::time::Instant::now() < released,
            "permit not released after the socket closed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
}
