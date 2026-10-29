use super::*;
use tower::ServiceExt;
use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind, ServerInfo};

#[tokio::test]
async fn oauth_navigation_exception_is_bound_to_ingress_pkce_audience_and_single_use() {
    use sha2::Digest;
    let (auth, _, app, _, _, _) = fixture().await;
    let ingress = auth
        .ensure_app_ingress(
            &app,
            "alice",
            "apps.treer.example",
            Some(ServiceIngressAccess::Workspace),
        )
        .await
        .unwrap();
    let config = IngressConfig::new(
        Some(Url::parse("https://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    let redirect = format!("https://{}/auth/callback", ingress.hostname);
    for candidate in [
        format!("https://{}.evil.example/auth/callback", ingress.hostname),
        format!("http://{}/auth/callback", ingress.hostname),
        format!("https://{}:444/auth/callback", ingress.hostname),
        format!("https://attacker@{}/auth/callback", ingress.hostname),
        format!("{redirect}#fragment"),
        "https://evil.example/auth/callback".to_owned(),
    ] {
        assert!(
            resolve_app_redirect(&auth, &config, &app.service_id, &candidate)
                .await
                .is_err(),
            "reject unregistered redirect origin/credentials/fragment"
        );
    }
    assert!(
        resolve_app_redirect(&auth, &config, "other-service", &redirect)
            .await
            .is_err()
    );
    let verifier = "v".repeat(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    let route = Router::new()
        .route("/authorize", axum::routing::get(authorize_workspace_app))
        .layer(Extension(auth.clone()))
        .layer(Extension(config));
    let mut uri = Url::parse("https://proxy.treer.example/authorize").unwrap();
    uri.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &app.service_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("state", "app-bound-state")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let target = format!("{}?{}", uri.path(), uri.query().unwrap());
    for (name, value) in [("sec-fetch-dest", "image"), ("sec-fetch-mode", "cors")] {
        let response = route
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&target)
                    .header(header::COOKIE, "treer_session=login")
                    .header(name, value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let codes: i64 = sqlx::query_scalar("SELECT count(*) FROM app_oauth_codes")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(codes, 0, "subresource requests must not issue codes");
    let members_before: i64 = sqlx::query_scalar("SELECT count(*) FROM organization_members")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    let response = route
        .oneshot(
            Request::builder()
                .uri(&target)
                .header(header::COOKIE, "treer_session=login")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let callback = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(callback.origin(), Url::parse(&redirect).unwrap().origin());
    let query: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    assert_eq!(query["state"], "app-bound-state");
    let code = &query["code"];
    assert!(auth
        .consume_app_oauth_code(code, "other-service", &redirect, &verifier)
        .await
        .is_err());
    assert!(auth
        .consume_app_oauth_code(code, &app.service_id, "https://evil.example/", &verifier)
        .await
        .is_err());
    assert!(auth
        .consume_app_oauth_code(code, &app.service_id, &redirect, &"x".repeat(64))
        .await
        .is_err());
    let ttl: bool = sqlx::query_scalar("SELECT (expires_at::timestamptz - created_at::timestamptz) <= interval '5 minutes' FROM app_oauth_codes").fetch_one(&auth.pool()).await.unwrap();
    assert!(ttl);
    let grant = auth
        .consume_app_oauth_code(code, &app.service_id, &redirect, &verifier)
        .await
        .unwrap();
    assert_eq!(grant.user_id, "alice");
    assert!(auth
        .consume_app_oauth_code(code, &app.service_id, &redirect, &verifier)
        .await
        .is_err());
    let members_after: i64 = sqlx::query_scalar("SELECT count(*) FROM organization_members")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    assert_eq!(members_after, members_before);
}

#[tokio::test]
async fn ingress_browser_binding_rejects_code_delivery_to_another_browser() {
    let (auth, _, app, _, _, _) = fixture().await;
    let ingress = auth
        .ensure_app_ingress(&app, "alice", "apps.treer.example", None)
        .await
        .unwrap();
    let config = IngressConfig::new(
        Some(Url::parse("https://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    let mut flows = Vec::new();
    for _ in 0..3 {
        let response = redirect_to_ingress_authorization(
            &config,
            &ingress.hostname,
            &"/issues?mine=1".parse().unwrap(),
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.starts_with("__Host-treer_ingress_nonce_"));
        assert!(cookie.contains("; Path=/; HttpOnly; SameSite=Lax; Max-Age=300; Secure"));
        assert!(!cookie.contains("Domain="));
        let cookie_pair = cookie.split(';').next().unwrap().to_string();
        let nonce = cookie_pair.split_once('=').unwrap().1;
        let location = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        assert!(!location.as_str().contains(nonce));
        let challenge = location
            .query_pairs()
            .find(|(k, _)| k == "browser_nonce_hash")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(challenge, auth::ingress_nonce_hash(nonce));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("proxy.treer.example"),
        );
        let login = authorize_service_ingress(State(AppState::new()), Extension(auth.clone()), Extension(PolicyEngine::new(crate::policy::PolicyDecision::Allow, vec![])), Extension(config.clone()), headers.clone(),
            Query(serde_json::from_value(json!({"hostname":ingress.hostname,"return_path":"/issues?mine=1","browser_nonce_hash":challenge})).unwrap())).await.unwrap();
        let login_url = Url::parse(login.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let return_to = login_url
            .query_pairs()
            .find(|(k, _)| k == "return_to")
            .unwrap()
            .1
            .into_owned();
        assert!(
            return_to.contains(&challenge),
            "login bounce preserves browser challenge"
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("treer_session=login"),
        );
        headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
        let authorized = authorize_service_ingress(State(AppState::new()), Extension(auth.clone()), Extension(PolicyEngine::new(crate::policy::PolicyDecision::Allow, vec![])), Extension(config.clone()), headers,
            Query(serde_json::from_value(json!({"hostname":ingress.hostname,"return_path":"/issues?mine=1","browser_nonce_hash":challenge})).unwrap())).await.unwrap();
        let callback =
            Url::parse(authorized.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let uri = format!("{}?{}", callback.path(), callback.query().unwrap())
            .parse::<Uri>()
            .unwrap();
        flows.push((cookie_pair, uri));
    }
    assert_ne!(
        flows[0].0, flows[1].0,
        "parallel tabs have independent cookies"
    );
    // Alice's valid code delivered into a second member's browser (no matching
    // App-domain verifier cookie) must not create any ingress session.
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM ingress_sessions")
        .fetch_one(&auth.pool())
        .await
        .unwrap();
    let denied = complete_ingress_authorization(
        &auth,
        &config,
        &ingress.hostname,
        &flows[0].1,
        &HeaderMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    assert!(denied.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        before,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ingress_sessions")
            .fetch_one(&auth.pool())
            .await
            .unwrap()
    );
    let mut both_tabs = HeaderMap::new();
    both_tabs.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("{}; {}", flows[1].0, flows[2].0)).unwrap(),
    );
    for (_, uri) in &flows {
        let response =
            complete_ingress_authorization(&auth, &config, &ingress.hostname, uri, &both_tabs)
                .await
                .unwrap();
        if uri == &flows[0].1 {
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "invalid delivery consumes code"
            );
        } else {
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert_eq!(response.headers()[header::LOCATION], "/issues?mine=1");
            assert_eq!(
                response
                    .headers()
                    .get_all(header::SET_COOKIE)
                    .iter()
                    .count(),
                2
            );
            let session = response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .split_once('=')
                .unwrap()
                .1;
            assert_eq!(
                auth.authenticate_ingress_session(&ingress.hostname, session)
                    .await
                    .unwrap()
                    .as_deref(),
                Some("alice")
            );
            assert_eq!(
                complete_ingress_authorization(&auth, &config, &ingress.hostname, uri, &both_tabs)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED,
                "callback refresh cannot replay"
            );
        }
    }
}

#[tokio::test]
async fn ingress_browser_codes_expiry_host_replay_and_old_proxy_fail_closed() {
    let (auth, _, app, _, _, _) = fixture().await;
    let ingress = auth
        .ensure_app_ingress(&app, "alice", "apps.treer.example", None)
        .await
        .unwrap();
    let nonce = "a".repeat(64);
    let hash = auth::ingress_nonce_hash(&nonce);
    let create = || auth.create_ingress_auth_code(&ingress, "alice", "/", &hash);
    for supplied in [None, Some("b".repeat(64)), Some(hash.clone())] {
        let code = create().await.unwrap();
        assert!(auth
            .consume_ingress_auth_code(&ingress.hostname, &code, supplied.as_deref())
            .await
            .is_err());
        assert!(auth
            .consume_ingress_auth_code(&ingress.hostname, &code, Some(&nonce))
            .await
            .is_err());
    }
    let code = create().await.unwrap();
    assert!(auth
        .consume_ingress_auth_code("other.apps.treer.example", &code, Some(&nonce))
        .await
        .is_err());
    let (one, two) = tokio::join!(
        auth.consume_ingress_auth_code(&ingress.hostname, &code, Some(&nonce)),
        auth.consume_ingress_auth_code(&ingress.hostname, &code, Some(&nonce))
    );
    assert_ne!(
        one.is_ok(),
        two.is_ok(),
        "atomic code consumption fences concurrent redemption"
    );
    let code = create().await.unwrap();
    sqlx::query("UPDATE ingress_browser_auth_codes SET expires_at=$2 WHERE code=$1")
        .bind(&code)
        .bind((Utc::now() - chrono::Duration::seconds(1)).to_rfc3339())
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(auth
        .consume_ingress_auth_code(&ingress.hostname, &code, Some(&nonce))
        .await
        .is_err());
    let code = create().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ingress_auth_codes WHERE code=$1")
            .bind(&code)
            .fetch_one(&auth.pool())
            .await
            .unwrap(),
        0,
        "old Proxy cannot redeem bound codes using its legacy table"
    );
    sqlx::query("INSERT INTO ingress_auth_codes(code,ingress_id,user_id,return_path,expires_at) VALUES('iac_old',$1,'alice','/',$2)").bind(&ingress.ingress_id).bind((Utc::now()+chrono::Duration::minutes(5)).to_rfc3339()).execute(&auth.pool()).await.unwrap();
    assert!(
        auth.consume_ingress_auth_code(&ingress.hostname, "iac_old", Some(&nonce))
            .await
            .is_err(),
        "new Proxy never accepts old unbound codes"
    );
    assert!(auth
        .create_ingress_auth_code(&ingress, "alice", "/", "invalid")
        .await
        .is_err());
}

#[tokio::test]
async fn ingress_callback_rejects_duplicate_or_unprefixed_verifier_cookies() {
    let (auth, _, app, _, _, _) = fixture().await;
    let ingress = auth
        .ensure_app_ingress(&app, "alice", "apps.treer.example", None)
        .await
        .unwrap();
    let config = IngressConfig::new(
        Some(Url::parse("https://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse("https://app.treer.example/").unwrap(),
    )
    .unwrap();
    let nonce = "a".repeat(64);
    let hash = auth::ingress_nonce_hash(&nonce);
    for cookie in [
        format!("treer_ingress_nonce_{hash}={nonce}"),
        format!(
            "__Host-treer_ingress_nonce_{hash}={nonce}; __Host-treer_ingress_nonce_{hash}={nonce}"
        ),
        format!("__Host-treer_ingress_nonce_{hash}={}", "b".repeat(64)),
    ] {
        let code = auth
            .create_ingress_auth_code(&ingress, "alice", "/", &hash)
            .await
            .unwrap();
        let uri: Uri = format!("/.treer/callback?code={code}&browser_nonce_hash={hash}")
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        let response =
            complete_ingress_authorization(&auth, &config, &ingress.hostname, &uri, &headers)
                .await
                .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(auth
            .consume_ingress_auth_code(&ingress.hostname, &code, Some(&nonce))
            .await
            .is_err());
    }
    for query in [
        "code=one&code=two",
        "code=one&browser_nonce_hash=one&browser_nonce_hash=two",
    ] {
        let uri: Uri = format!("/.treer/callback?{query}").parse().unwrap();
        let response = complete_ingress_authorization(
            &auth,
            &config,
            &ingress.hostname,
            &uri,
            &HeaderMap::new(),
        )
        .await
        .unwrap_or_else(IntoResponse::into_response);
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ingress_sessions")
            .fetch_one(&auth.pool())
            .await
            .unwrap(),
        0
    );
}

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
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Issues".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 9440,
                hostname: String::new(),
                public: false,
                ui_capabilities: Vec::new(),
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
                capabilities: vec![],
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
    rx: tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) -> tokio::task::JoinHandle<String> {
    observe_response(
        state,
        connection,
        rx,
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    )
}

fn observe_response(
    state: AppState,
    connection: Uuid,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    response: &'static [u8],
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
                    payload: response.to_vec(),
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

#[tokio::test]
async fn agent_ui_frame_policy_real_router_preserves_launcher_headers_and_strips_forgery() {
    // HTTP over the real machine-stream tunnel, with a synthetic Controller.
    // No launcher, browser session or enrolled machine is contacted.
    for kind in [
        "agent-root",
        "agent-slash",
        "agent-asset",
        "agent-websocket",
        "virtual-host",
        "app-ui",
    ] {
        let (auth, identity, app, state, connection, rx) = fixture().await;
        let server = state.resolve_server("default", "machine-a").await.unwrap();
        let now = Utc::now();
        state
            .apply_snapshot(
                connection,
                treer_protocol::AgentServerSnapshot {
                    server,
                    agents: vec![AgentInfo {
                        path: None,
                        run_generation: 0,
                        agent_id: "agent-ui".into(),
                        workspace_id: "default".into(),
                        server_id: "machine-a".into(),
                        kind: "codex".into(),
                        name: "test".into(),
                        cwd: ".".into(),
                        status: treer_protocol::AgentStatus::Idle,
                        pid: None,
                        started_at: now,
                        updated_at: now,
                        exited_at: None,
                        exit_code: None,
                        output_revision: 0,
                        interface: Some(treer_protocol::AgentInterfaceDescriptor {
                            origin_url: None,
                            protocol: treer_protocol::AGENT_INTERFACE_PROTOCOL_V1.into(),
                            instance_id: "actual-instance".into(),
                            port: 12345,
                            capabilities: vec![],
                            ui_path: Some("/launcher/".into()),
                            registered_at: now,
                        }),
                    }],
                },
            )
            .await
            .unwrap();
        let websocket = kind == "agent-websocket";
        let upstream: &'static [u8] = if websocket {
            b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n"
        } else {
            concat!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 8\r\n",
                "Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; form-action 'none'\r\n",
                "Content-Security-Policy: script-src 'self'; FRAME-ANCESTORS https://untrusted.example\r\n",
                "Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=(), clipboard-read=(), bluetooth=(), hid=(), serial=(), midi=(), accelerometer=(), gyroscope=(), magnetometer=(), screen-wake-lock=()\r\n",
                "Referrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n",
                "Set-Cookie: launcher=must-not-forward\r\nConnection: close\r\n\r\nlauncher",
            ).as_bytes()
        };
        let observer = observe_response(state.clone(), connection, rx, upstream);
        let proxy_url = Url::parse("https://proxy.treer.example/").unwrap();
        let console_url = Url::parse("https://app.treer.example/").unwrap();
        let messages = MessageStore::open(auth.pool()).await.unwrap();
        let route = router(
            state,
            BootstrapConfig::new(
                proxy_url.clone(),
                PathBuf::from("dist"),
                Url::parse("https://example.test/releases/").unwrap(),
            ),
            auth,
            PolicyEngine::allow_all(),
            identity,
            BrowserAccess::new(&console_url, &proxy_url).unwrap(),
            IngressConfig::new(None, &proxy_url, &console_url).unwrap(),
            messages,
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
        );
        let path = match kind {
            "agent-root" => "/api/workspaces/default/agents/agent-ui/interface/ui".into(),
            "agent-slash" => "/api/workspaces/default/agents/agent-ui/interface/ui/".into(),
            "agent-asset" => "/api/workspaces/default/agents/agent-ui/interface/ui/app.js".into(),
            "agent-websocket" => "/api/workspaces/default/agents/agent-ui/interface/ui/ws".into(),
            "virtual-host" => format!(
                "/api/workspaces/default/virtual-hosts/{}/proxy/",
                app.hostname
            ),
            "app-ui" => format!("/api/workspaces/default/apps/{}/ui/", app.app_id),
            _ => unreachable!(),
        };
        let mut request = Request::builder();
        if websocket {
            request = request
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONNECTION, "Upgrade")
                .header(header::UPGRADE, "websocket");
        }
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            route.oneshot(
                request
                    .uri(path)
                    .header(header::COOKIE, "treer_session=login")
                    // Ordinary top-level navigations have no Origin header.
                    .header("sec-fetch-mode", "navigate")
                    .header("sec-fetch-dest", "document")
                    .header("x-treer-interface-source", "controller-api")
                    .header("x-treer-interface-instance", "forged-instance")
                    .header("x-treer-agent-id", "forged-agent")
                    .header("x-treer-identity-token", "forged-token")
                    .header("x-treer-custom-identity", "forged-custom")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            response.status(),
            if websocket {
                StatusCode::SWITCHING_PROTOCOLS
            } else {
                StatusCode::OK
            },
            "{kind}"
        );
        let headers = response.headers();
        if websocket {
            assert_eq!(headers[header::UPGRADE], "websocket");
            assert_eq!(headers[header::CONNECTION], "Upgrade");
            assert_eq!(
                headers[header::CONTENT_SECURITY_POLICY],
                "frame-ancestors 'self' https://app.treer.example"
            );
            assert!(!headers.contains_key(header::X_FRAME_OPTIONS));
            let forwarded = tokio::time::timeout(Duration::from_secs(30), observer)
                .await
                .unwrap()
                .unwrap();
            assert!(forwarded.starts_with("GET /launcher/ws HTTP/1.1\r\n"));
            assert!(!forwarded.to_ascii_lowercase().contains("forged"));
            continue;
        }
        assert_eq!(
            headers["permissions-policy"],
            "camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=(), clipboard-read=(), bluetooth=(), hid=(), serial=(), midi=(), accelerometer=(), gyroscope=(), magnetometer=(), screen-wake-lock=()"
        );
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");
        assert!(!headers.contains_key(header::SET_COOKIE));
        let policies: Vec<_> = headers
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        if kind == "virtual-host" {
            assert_eq!(
                policies,
                [
                    "default-src 'self'; frame-ancestors 'none'; form-action 'none'",
                    "script-src 'self'; FRAME-ANCESTORS https://untrusted.example"
                ]
            );
            assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        } else {
            assert_eq!(policies, ["default-src 'self'; form-action 'none'; frame-ancestors 'self' https://app.treer.example",
                "script-src 'self'; frame-ancestors 'self' https://app.treer.example"]);
            assert!(!headers.contains_key(header::X_FRAME_OPTIONS));
        }
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "launcher"
        );
        let forwarded = tokio::time::timeout(Duration::from_secs(30), observer)
            .await
            .unwrap()
            .unwrap();
        let lower = forwarded.to_ascii_lowercase();
        assert!(!lower.contains("forged"), "{kind}");
        assert!(!lower.contains("cookie:"));
        if kind.starts_with("agent-") {
            let expected_target = if kind == "agent-asset" {
                "/launcher/app.js"
            } else {
                "/launcher/"
            };
            assert!(forwarded.starts_with(&format!("GET {expected_target} HTTP/1.1\r\n")));
            assert!(lower.contains("x-treer-interface-source: proxy-ui\r\n"));
            assert!(lower.contains("x-treer-interface-instance: actual-instance\r\n"));
            assert!(lower.contains("x-treer-agent-id: agent-ui\r\n"));
            assert!(!lower.contains("x-treer-identity-token:"));
        }
    }
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
            .create_ingress_auth_code(
                &ingress,
                "alice",
                "/",
                &auth::ingress_nonce_hash(&"f".repeat(64)),
            )
            .await
            .unwrap();
        let session = auth
            .consume_ingress_auth_code(&ingress.hostname, &code, Some(&"f".repeat(64)))
            .await
            .unwrap();
        let observer = observe(state.clone(), connection, rx);
        let config = IngressConfig::new(
            Some(Url::parse("https://apps.treer.example/").unwrap()),
            &Url::parse("https://proxy.treer.example/").unwrap(),
            &Url::parse("https://app.treer.example/").unwrap(),
        )
        .unwrap();
        let response = proxy_service_ingress(State(state),Extension(auth),Extension(PolicyEngine::new(crate::policy::PolicyDecision::Allow, vec![])),Extension(config),Extension(identity.clone()),
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
        .create_ingress_auth_code(
            &ingress,
            "alice",
            "/",
            &auth::ingress_nonce_hash(&"f".repeat(64)),
        )
        .await
        .unwrap();
    let session = auth
        .consume_ingress_auth_code(&ingress.hostname, &code, Some(&"f".repeat(64)))
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
        Extension(PolicyEngine::new(
            crate::policy::PolicyDecision::Allow,
            vec![],
        )),
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
        Extension(PolicyEngine::new(
            crate::policy::PolicyDecision::Allow,
            vec![],
        )),
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
