use super::*;
use tower::ServiceExt;
use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind, ServerInfo};

async fn fixture() -> (
    AuthStore,
    IdentityIssuer,
    AppDeployment,
    AppState,
    Uuid,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let now = Utc::now();
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('alice','alice@example.test',TRUE,'Alice','unused',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','alice','member',$1)")
        .bind(now.to_rfc3339()).execute(&auth.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('login','alice',$1,$2)",
    )
    .bind(now.to_rfc3339())
    .bind((now + chrono::Duration::hours(1)).to_rfc3339())
    .execute(&auth.pool())
    .await
    .unwrap();
    let identity =
        IdentityIssuer::load(&auth, &Url::parse("https://proxy.treer.example/").unwrap())
            .await
            .unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "alice",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                server_id: Some("machine-a".into()),
                name: "Issues".into(),
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
    (auth, identity, app, state, connection, rx)
}

fn observe(
    state: AppState,
    connection: Uuid,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let open = loop {
            if let SocketFrame::Binary(bytes) = rx.recv().await.unwrap() {
                let frame = NetworkBinaryFrame::decode(&bytes).unwrap();
                if frame.kind == NetworkBinaryKind::Open {
                    break frame;
                }
            }
        };
        state
            .relay_network_frame(
                "default",
                "machine-a",
                connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: vec![],
                },
            )
            .await
            .unwrap();
        let mut request = String::new();
        while !request.contains("\r\n\r\n") {
            if let SocketFrame::Binary(bytes) = rx.recv().await.unwrap() {
                let frame = NetworkBinaryFrame::decode(&bytes).unwrap();
                if frame.kind == NetworkBinaryKind::Data {
                    request.push_str(&String::from_utf8(frame.payload).unwrap());
                }
            }
        }
        state
            .relay_network_frame(
                "default",
                "machine-a",
                connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: open.stream_id.clone(),
                    payload: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        .to_vec(),
                },
            )
            .await
            .unwrap();
        state
            .relay_network_frame(
                "default",
                "machine-a",
                connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: vec![],
                },
            )
            .await
            .unwrap();
        request
    })
}

fn token_in(request: &str) -> Option<&str> {
    request.split("\r\n").find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(TREER_IDENTITY_TOKEN_HEADER)
            .then(|| value.trim())
    })
}

fn assert_human(identity: &IdentityIssuer, service: &str, token: &str) {
    let verified = identity.verify_app(token, service);
    assert!(verified.active);
    let claims = verified.claims.unwrap();
    assert_eq!(claims.sub, "alice");
    assert_eq!(claims.workspace_id, "default");
    assert_eq!(claims.principal_kind, AppPrincipalKind::Human);
    assert_eq!(claims.role.as_deref(), Some("member"));
    assert_eq!(claims.name, "Alice");
    assert_eq!(claims.exp - claims.iat, 60);
    assert!(!identity.verify_app(token, "another-service").active);
}

fn embedded_router(state: AppState, auth: AuthStore, identity: IdentityIssuer) -> Router {
    let browser = BrowserAccess::new(
        &Url::parse("https://app.treer.example/").unwrap(),
        &Url::parse("https://proxy.treer.example/").unwrap(),
    )
    .unwrap();
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/ui/",
            any(proxy_app_ui_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/ui/{*path}",
            any(proxy_app_ui_path),
        )
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_workspace_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_user,
        ))
        .layer(Extension(auth))
        .layer(Extension(identity))
        .layer(Extension(browser))
        .with_state(state)
}

#[tokio::test]
async fn app_human_identity_embedded_session_is_forwarded_after_sanitizing() {
    let (auth, identity, app, state, connection, rx) = fixture().await;
    let observer = observe(state.clone(), connection, rx);
    let router = embedded_router(state, auth, identity.clone());
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/workspaces/default/apps/{}/ui/v1/me?full=1",
                    app.app_id
                ))
                .header(
                    header::COOKIE,
                    "treer_session=login; app_session=do-not-forward",
                )
                .header("x-treer-identity-token", "forged")
                .header("x-treer-workload-credential", "secret")
                .header("treer-authorization", "Bearer forged")
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let forwarded = observer.await.unwrap();
    assert!(forwarded.starts_with("GET /v1/me?full=1 HTTP/1.1\r\n"));
    assert_human(&identity, &app.service_id, token_in(&forwarded).unwrap());
    let lower = forwarded.to_ascii_lowercase();
    assert!(!lower.contains("cookie:"));
    assert!(!lower.contains("authorization:"));
    assert!(!lower.contains("forged"));
    assert!(!lower.contains("x-treer-workload-credential"));
    assert!(lower.contains("sec-fetch-site: same-origin"));
}

#[tokio::test]
async fn app_human_identity_workspace_ingress_issues_short_token_but_public_ingress_does_not() {
    for access in [
        ServiceIngressAccess::Workspace,
        ServiceIngressAccess::Public,
    ] {
        let (auth, identity, app, state, connection, rx) = fixture().await;
        let ingress = auth
            .ensure_app_ingress(&app, "alice", "apps.treer.example", Some(access))
            .await
            .unwrap();
        let code = auth
            .create_ingress_auth_code(&ingress, "alice", "/")
            .await
            .unwrap();
        let session = auth
            .consume_ingress_auth_code(&ingress.hostname, &code)
            .await
            .unwrap();
        let observer = observe(state.clone(), connection, rx);
        let config = IngressConfig::new(
            Some(Url::parse("https://apps.treer.example/").unwrap()),
            &Url::parse("https://proxy.treer.example/").unwrap(),
            &Url::parse("https://app.treer.example/").unwrap(),
        )
        .unwrap();
        let response = proxy_service_ingress(State(state),Extension(auth),Extension(config),Extension(identity.clone()),
            Request::builder().uri("/v1/me").header(header::HOST,&ingress.hostname)
                .header(header::COOKIE,format!("__Host-treer_ingress={}; treer_session=login; treer_admin_session=admin; app_session=kept",session.session_token))
                .header("x-treer-identity-token","forged").header("x-treer-spoofed","secret")
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let forwarded = observer.await.unwrap();
        if access == ServiceIngressAccess::Workspace {
            assert_human(&identity, &app.service_id, token_in(&forwarded).unwrap());
        } else {
            assert!(token_in(&forwarded).is_none());
        }
        assert!(!forwarded.contains("forged"));
        assert!(!forwarded.contains("treer_session="));
        assert!(!forwarded.contains("treer_admin_session="));
        assert!(!forwarded.contains("treer_ingress="));
        assert!(forwarded.contains("cookie: app_session=kept"));
    }
}

#[tokio::test]
async fn app_human_identity_membership_revocation_role_changes_and_workspace_boundary() {
    let (auth, identity, app, state, _, _) = fixture().await;
    let first = issue_browser_app_identity(&auth, &identity, "default", "alice", &app.service_id)
        .await
        .unwrap();
    sqlx::query("UPDATE organization_members SET role='owner' WHERE user_id='alice'")
        .execute(&auth.pool())
        .await
        .unwrap();
    let second = issue_browser_app_identity(&auth, &identity, "default", "alice", &app.service_id)
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        identity
            .verify_app(&second, &app.service_id)
            .claims
            .unwrap()
            .role
            .as_deref(),
        Some("owner")
    );
    auth.seed_test_workspace("other").await;
    assert!(
        issue_browser_app_identity(&auth, &identity, "other", "alice", &app.service_id)
            .await
            .is_err()
    );
    let ingress = auth
        .ensure_app_ingress(&app, "alice", "apps.treer.example", None)
        .await
        .unwrap();
    let code = auth
        .create_ingress_auth_code(&ingress, "alice", "/")
        .await
        .unwrap();
    let session = auth
        .consume_ingress_auth_code(&ingress.hostname, &code)
        .await
        .unwrap();
    sqlx::query("DELETE FROM organization_members WHERE user_id='alice'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(
        issue_browser_app_identity(&auth, &identity, "default", "alice", &app.service_id)
            .await
            .is_err()
    );
    assert!(auth
        .authenticate_ingress_session(&ingress.hostname, &session.session_token)
        .await
        .unwrap()
        .is_none());
    let verified = verify_app_identity(
        Extension(auth.clone()),
        Extension(identity.clone()),
        Json(AppIdentityVerifyRequest {
            token: second,
            audience: app.service_id.clone(),
        }),
    )
    .await;
    let body = axum::body::to_bytes(verified.into_body(), 4096)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["active"],
        false
    );
    let config = IngressConfig::new(
        Some(Url::parse("https://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    let denied = proxy_service_ingress(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(config),
        Extension(identity.clone()),
        Request::builder()
            .uri("/v1/me")
            .header(header::HOST, &ingress.hostname)
            .header(
                header::COOKIE,
                format!("__Host-treer_ingress={}", session.session_token),
            )
            .header(TREER_IDENTITY_TOKEN_HEADER, "forged")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(denied.status(), StatusCode::SEE_OTHER);
    assert!(denied.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .contains("/.treer/ingress/authorize"));
    let response = embedded_router(state, auth, identity)
        .oneshot(
            Request::builder()
                .uri(format!("/api/workspaces/default/apps/{}/ui/", app.app_id))
                .header(header::COOKIE, "treer_session=login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn app_human_identity_agent_ingress_preserves_its_original_token() {
    let (auth, identity, app, state, connection, rx) = fixture().await;
    let ingress = auth
        .ensure_app_ingress(&app, "alice", "apps.treer.example", None)
        .await
        .unwrap();
    let agent = identity
        .issue("default", "machine-a", "agent-a", &app.service_id)
        .unwrap();
    let observer = observe(state.clone(), connection, rx);
    let config = IngressConfig::new(
        Some(Url::parse("https://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    let response = proxy_service_ingress(
        State(state),
        Extension(auth),
        Extension(config),
        Extension(identity.clone()),
        Request::builder()
            .uri("/v1/me")
            .header(header::HOST, &ingress.hostname)
            .header(
                TREER_AUTHORIZATION_HEADER,
                format!("Bearer {}", agent.access_token),
            )
            .header(TREER_IDENTITY_TOKEN_HEADER, "forged")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let forwarded = observer.await.unwrap();
    assert_eq!(token_in(&forwarded), Some(agent.access_token.as_str()));
    assert_eq!(
        identity
            .verify_app(token_in(&forwarded).unwrap(), &app.service_id)
            .claims
            .unwrap()
            .principal_kind,
        AppPrincipalKind::Agent
    );
}

#[test]
fn app_human_identity_sanitizers_strip_spoofed_headers_even_on_upgrades() {
    for upgraded in [false, true] {
        let mut headers = HeaderMap::new();
        for name in [
            "x-treer-identity-token",
            "x-treer-agent-id",
            "x-treer-workload-credential",
            "treer-authorization",
            "authorization",
            "proxy-authorization",
        ] {
            headers.insert(name, HeaderValue::from_static("spoofed"));
        }
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("treer_session=login"),
        );
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        sanitize_tunnel_request_headers(&mut headers, upgraded, "app.internal").unwrap();
        assert!(!headers
            .keys()
            .any(|name| name.as_str().starts_with("x-treer-")));
        assert!(!headers.contains_key(TREER_AUTHORIZATION_HEADER));
        assert!(!headers.contains_key(header::COOKIE));
        assert!(!headers.contains_key(header::AUTHORIZATION));
        assert_eq!(headers["sec-fetch-site"], "cross-site");
    }
}
