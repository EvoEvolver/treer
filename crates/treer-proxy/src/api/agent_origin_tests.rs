//! Agent interface origins (#143 Phase 2) at the handler level against
//! isolated PostgreSQL and the fake machine tunnel: the `ag-<id>` hostname
//! grammar, the no-oracle behaviour before authentication, the interface
//! gate on authorize and on every request, provenance and response baseline
//! headers, the same-site sibling defence for every ingress host, Console
//! routes refusing an Agent origin as a browser origin, lifecycle (delete,
//! logout, no access switching) and the `origin_url` read model.
use super::ingress_protection_tests::{config, fixture, upstream, with_peer, Fixture, Upstream};
use super::tests::test_config;
use super::*;
use std::collections::BTreeMap;
use tower::ServiceExt;
use treer_protocol::{
    PolicyEffect, PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef, ServiceIngressAccess,
    ServiceIngressTargetKind, UpdateIngressLimitsRequest, UpdateServiceIngressRequest,
    WorkspacePolicyDocument, AGENT_ID_HEADER, AGENT_INTERFACE_PROTOCOL_V1,
    AGENT_INTERFACE_SOURCE_HEADER, AGENT_INTERFACE_SOURCE_UI, POLICY_SCHEMA_VERSION,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

const AGENT_ID: &str = "ag_0123456789abcdef0123456789abcdef";
const AGENT_HOST: &str = "ag-0123456789abcdef0123456789abcdef.apps.treer.example";
const UNKNOWN_HOST: &str = "ag-ffffffffffffffffffffffffffffffff.apps.treer.example";
const BASE: &str = "apps.treer.example";

fn allow() -> PolicyEngine {
    PolicyEngine::allow_all()
}

fn deny() -> PolicyEngine {
    PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    )
}

/// The shared ingress fixture plus one managed Agent with a registered
/// interface UI, its unrevoked credential row and Console sessions for the
/// owner, a member and a non-member.
async fn agent_fixture() -> Fixture {
    let fixture = fixture().await;
    let now = Utc::now();
    sqlx::query("INSERT INTO agent_credentials(agent_id,workspace_id,server_id,secret_hash,created_at) VALUES($1,'default','machine-a','unused',$2)")
        .bind(AGENT_ID).bind(now.to_rfc3339()).execute(&fixture.auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('stranger','stranger@example.test',TRUE,'Stranger','unused',$1)")
        .bind(now.to_rfc3339()).execute(&fixture.auth.pool()).await.unwrap();
    for user in ["owner", "member", "stranger"] {
        sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,$2,$3,'2999-01-01T00:00:00Z')")
            .bind(format!("tok-{user}")).bind(user).bind(now.to_rfc3339())
            .execute(&fixture.auth.pool()).await.unwrap();
    }
    fixture
        .state
        .test_insert_agent(treer_protocol::AgentInfo {
            path: None,
            run_generation: 0,
            agent_id: AGENT_ID.into(),
            workspace_id: "default".into(),
            server_id: "machine-a".into(),
            kind: "claude".into(),
            name: "coder".into(),
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
                protocol: AGENT_INTERFACE_PROTOCOL_V1.into(),
                instance_id: "instance-1".into(),
                port: 9500,
                capabilities: vec![],
                ui_path: Some("/ui".into()),
                registered_at: now,
            }),
        })
        .await;
    fixture
}

async fn send(
    fixture: &Fixture,
    policy: PolicyEngine,
    config: &IngressConfig,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    proxy_service_ingress(
        State(fixture.state.clone()),
        Extension(fixture.auth.clone()),
        Extension(policy),
        Extension(config.clone()),
        Extension(fixture.identity.clone()),
        with_peer(request, "203.0.113.9"),
    )
    .await
}

fn navigation(host: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .uri(uri)
        .header(header::HOST, host)
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document")
}

async fn error_of(result: Result<Response, ApiFailure>) -> (StatusCode, String) {
    match result {
        Ok(response) => {
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let code = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| value["error"]["code"].as_str().map(str::to_owned))
                .unwrap_or_default();
            (status, code)
        }
        Err(failure) => (failure.status, failure.error.code),
    }
}

async fn agent_rows(fixture: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM service_ingresses WHERE agent_id IS NOT NULL")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap()
}

async fn ingress_sessions(fixture: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ingress_sessions")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap()
}

/// Runs the authorize step for `hostname` as the given Console session.
async fn authorize(
    fixture: &Fixture,
    policy: PolicyEngine,
    config: &IngressConfig,
    session: Option<&str>,
    hostname: &str,
    return_path: &str,
    challenge: &str,
) -> Result<Response, ApiFailure> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_static("proxy.treer.example"),
    );
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    if let Some(token) = session {
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("treer_session={token}")).unwrap(),
        );
    }
    authorize_service_ingress(
        State(fixture.state.clone()),
        Extension(fixture.auth.clone()),
        Extension(policy),
        Extension(config.clone()),
        headers,
        Query(
            serde_json::from_value(json!({
                "hostname": hostname,
                "return_path": return_path,
                "browser_nonce_hash": challenge,
            }))
            .unwrap(),
        ),
    )
    .await
}

/// The full browser flow on `host`: redirect to authorize, authorize as the
/// session, callback; returns the ingress session cookie pair.
async fn establish_session(
    fixture: &Fixture,
    policy: PolicyEngine,
    config: &IngressConfig,
    host: &str,
    session: &str,
) -> String {
    let start = send(
        fixture,
        policy.clone(),
        config,
        navigation(host, "/?tab=1").body(Body::empty()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(start.status(), StatusCode::SEE_OTHER);
    let nonce_cookie = start.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let location = Url::parse(start.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(location.host_str(), Some("proxy.treer.example"));
    assert_eq!(location.path(), "/.treer/ingress/authorize");
    let query: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(query["hostname"], host);
    assert_eq!(query["return_path"], "/?tab=1");
    let authorized = authorize(
        fixture,
        policy,
        config,
        Some(session),
        host,
        "/?tab=1",
        &query["browser_nonce_hash"],
    )
    .await
    .unwrap();
    assert_eq!(authorized.status(), StatusCode::SEE_OTHER);
    let callback = Url::parse(authorized.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(callback.host_str(), Some(host));
    assert_eq!(callback.path(), "/.treer/callback");
    let completed = send(
        fixture,
        allow(),
        config,
        navigation(
            host,
            &format!("{}?{}", callback.path(), callback.query().unwrap()),
        )
        .header(header::COOKIE, &nonce_cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(completed.status(), StatusCode::SEE_OTHER);
    assert_eq!(completed.headers()[header::LOCATION], "/?tab=1");
    completed
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .find(|pair| pair.starts_with("__Host-treer_ingress="))
        .expect("ingress session cookie")
}

#[test]
fn agent_origin_labels_are_recognised_only_when_well_formed() {
    assert_eq!(
        auth::platform_agent_hostname(AGENT_ID, BASE).unwrap(),
        AGENT_HOST
    );
    assert_eq!(
        auth::platform_agent_hostname("ag_0123456789ABCDEF0123456789abcdef", BASE).unwrap(),
        AGENT_HOST,
        "labels are lower-case"
    );
    for bad in [
        "agent-a",
        "ag_short",
        "ag_0123456789abcdef0123456789abcdeg",
        "svc_x",
    ] {
        assert!(auth::platform_agent_hostname(bad, BASE).is_err(), "{bad}");
    }
    assert_eq!(
        auth::agent_id_from_platform_hostname(AGENT_HOST, BASE).as_deref(),
        Some(AGENT_ID)
    );
    assert_eq!(
        auth::agent_id_from_platform_hostname(&AGENT_HOST.to_ascii_uppercase(), BASE).as_deref(),
        Some(AGENT_ID),
        "hostnames are case-insensitive"
    );
    for bad in [
        "svc-0123456789abcdef0123456789abcdef.apps.treer.example",
        "ag-0123456789abcdef0123456789abcde.apps.treer.example",
        "ag-0123456789abcdef0123456789abcdefg.apps.treer.example",
        "ag-0123456789abcdef0123456789abcdef.apps.treer.example.evil.test",
        "x.ag-0123456789abcdef0123456789abcdef.apps.treer.example",
        "ag-.apps.treer.example",
        "apps.treer.example",
    ] {
        assert_eq!(
            auth::agent_id_from_platform_hostname(bad, BASE),
            None,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn agent_origin_reveals_nothing_before_authentication() {
    let fixture = agent_fixture().await;
    let config = config(None);
    let mut shapes = Vec::new();
    for host in [AGENT_HOST, UNKNOWN_HOST] {
        // Top-level navigation: the authorize redirect, identical for an
        // existing and a non-existing Agent (same status, same URL shape).
        let response = send(
            &fixture,
            allow(),
            &config,
            navigation(host, "/chat").body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let mut query: Vec<_> = location
            .query_pairs()
            .map(|(key, _)| key.into_owned())
            .collect();
        query.sort();
        shapes.push((
            location.host_str().unwrap().to_owned(),
            location.path().to_owned(),
            query,
        ));
        // Subresource and Bearer requests are refused the same way.
        let (status, code) = error_of(
            send(
                &fixture,
                allow(),
                &config,
                Request::builder()
                    .uri("/chat")
                    .header(header::HOST, host)
                    .header("sec-fetch-dest", "image")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{host}");
        assert_eq!(code, "invalid_ingress_authorization", "{host}");
        let (status, code) = error_of(
            send(
                &fixture,
                allow(),
                &config,
                Request::builder()
                    .uri("/chat")
                    .header(header::HOST, host)
                    .header(TREER_AUTHORIZATION_HEADER, "Bearer anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{host}");
        assert_eq!(code, "ingress_authentication_required", "{host}");
        let (status, code) = error_of(
            send(
                &fixture,
                allow(),
                &config,
                Request::builder()
                    .uri("/.treer/anything")
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (StatusCode::NOT_FOUND, "route_not_found")
        );
        // Authorize without a Console session bounces to login for both.
        let login = authorize(
            &fixture,
            allow(),
            &config,
            None,
            host,
            "/chat",
            &"a".repeat(64),
        )
        .await
        .unwrap();
        assert_eq!(login.status(), StatusCode::SEE_OTHER);
        assert!(login.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("https://app.treer.example/"));
    }
    assert_eq!(shapes[0], shapes[1], "existence must not leak before login");
    assert_eq!(agent_rows(&fixture).await, 0, "no row before authorization");
    assert_eq!(ingress_sessions(&fixture).await, 0);
}

#[tokio::test]
async fn agent_origin_authorize_applies_the_interface_gate_before_creating_the_origin() {
    let fixture = agent_fixture().await;
    let config = config(None);
    let challenge = "b".repeat(64);
    // Authenticated non-existence is a 404 only after the session check.
    let (status, code) = error_of(
        authorize(
            &fixture,
            allow(),
            &config,
            Some("tok-member"),
            UNKNOWN_HOST,
            "/",
            &challenge,
        )
        .await,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::NOT_FOUND, "agent_not_found")
    );
    // A non-member never passes; a member denied by Policy gets no origin.
    let (status, code) = error_of(
        authorize(
            &fixture,
            allow(),
            &config,
            Some("tok-stranger"),
            AGENT_HOST,
            "/",
            &challenge,
        )
        .await,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::NOT_FOUND, "agent_not_found"),
        "a non-member is indistinguishable from an unknown id (#164 a)"
    );
    let (status, code) = error_of(
        authorize(
            &fixture,
            deny(),
            &config,
            Some("tok-member"),
            AGENT_HOST,
            "/",
            &challenge,
        )
        .await,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::FORBIDDEN, "policy_denied")
    );
    assert_eq!(agent_rows(&fixture).await, 0);
    let codes: i64 = sqlx::query_scalar("SELECT count(*) FROM ingress_browser_auth_codes")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(codes, 0, "denied members get no code");
    // Passing the gate creates the origin row once and issues a code.
    for _ in 0..2 {
        let authorized = authorize(
            &fixture,
            allow(),
            &config,
            Some("tok-member"),
            AGENT_HOST,
            "/",
            &challenge,
        )
        .await
        .unwrap();
        assert_eq!(authorized.status(), StatusCode::SEE_OTHER);
        let callback =
            Url::parse(authorized.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        assert_eq!(callback.host_str(), Some(AGENT_HOST));
        assert_eq!(callback.path(), "/.treer/callback");
    }
    assert_eq!(agent_rows(&fixture).await, 1);
    let row = fixture
        .auth
        .resolve_agent_interface_hostname(AGENT_HOST)
        .await
        .unwrap()
        .expect("origin row");
    assert_eq!(row.target_kind, ServiceIngressTargetKind::AgentInterface);
    assert_eq!(row.agent_id.as_deref(), Some(AGENT_ID));
    assert_eq!(row.service_id, None);
    assert_eq!(row.access, ServiceIngressAccess::Workspace);
    assert_eq!(row.hostname, AGENT_HOST);
    let listed = fixture
        .auth
        .list_service_ingresses("default")
        .await
        .unwrap();
    let entry = listed
        .iter()
        .find(|ingress| ingress.ingress_id == row.ingress_id)
        .expect("listed with the services");
    assert_eq!(entry.target_kind, ServiceIngressTargetKind::AgentInterface);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM organization_audit_events WHERE action = 'agent.interface.origin.create' AND resource_id = $1",
    )
    .bind(AGENT_ID)
    .fetch_one(&fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(audited, 1);
    // The origin's access and limits cannot be switched or edited.
    let switched = fixture
        .auth
        .update_service_ingress(
            "default",
            &row.ingress_id,
            "owner",
            UpdateServiceIngressRequest {
                access: Some(ServiceIngressAccess::Public),
                enabled: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(switched.code(), "ingress_target_not_switchable");
    let limited = fixture
        .auth
        .update_service_ingress_limits(
            "default",
            &row.ingress_id,
            "owner",
            &UpdateIngressLimitsRequest::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(limited.code(), "ingress_target_not_switchable");
    assert_eq!(
        fixture
            .auth
            .resolve_agent_interface_hostname(AGENT_HOST)
            .await
            .unwrap()
            .unwrap()
            .access,
        ServiceIngressAccess::Workspace
    );
}

#[tokio::test]
async fn agent_origin_serves_the_interface_only_to_gated_sessions() {
    let mut fixture = agent_fixture().await;
    let config = config(None);
    let (heads, served) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Immediate,
    );
    let cookie = establish_session(&fixture, allow(), &config, AGENT_HOST, "tok-member").await;
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/?x=1")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, format!("{cookie}; app_pref=dark"))
            .header("x-treer-interface-source", "controller-api")
            .header("x-treer-agent-id", "victim")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    let head = heads.lock().unwrap().last().cloned().unwrap();
    let lower = head.to_ascii_lowercase();
    assert!(head.starts_with("GET /ui?x=1 HTTP/1.1\r\n"), "{head}");
    assert!(
        lower.contains(&format!(
            "{AGENT_INTERFACE_SOURCE_HEADER}: {AGENT_INTERFACE_SOURCE_UI}"
        )),
        "{head}"
    );
    assert!(
        lower.contains(&format!("{AGENT_ID_HEADER}: {AGENT_ID}")),
        "{head}"
    );
    assert!(
        !lower.contains("cookie:"),
        "no browser cookie reaches the Agent: {head}"
    );
    assert!(!lower.contains("victim"), "{head}");
    // Paths are mapped under the registered UI path.
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/assets/app.js")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    assert!(heads
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .starts_with("GET /ui/assets/app.js HTTP/1.1\r\n"));
    let served_before = served.load(std::sync::atomic::Ordering::SeqCst);
    // The gate runs per request: a later denial refuses the same session.
    let (status, code) = error_of(
        send(
            &fixture,
            deny(),
            &config,
            Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::FORBIDDEN, "policy_denied")
    );
    // A Bearer alongside the cookie is refused, never silently ignored.
    let (status, code) = error_of(
        send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, &cookie)
                .header(TREER_AUTHORIZATION_HEADER, "Bearer x")
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::UNAUTHORIZED, "ingress_authentication_required")
    );
    // The session is bound to its host: on another Agent's origin it is
    // just an unauthenticated navigation.
    let response = send(
        &fixture,
        allow(),
        &config,
        navigation(UNKNOWN_HOST, "/")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        served.load(std::sync::atomic::Ordering::SeqCst),
        served_before
    );
    // Deleting the Agent removes the origin and every session on it.
    assert_eq!(ingress_sessions(&fixture).await, 1);
    fixture
        .auth
        .delete_agent("default", AGENT_ID)
        .await
        .unwrap();
    assert_eq!(agent_rows(&fixture).await, 0);
    assert_eq!(ingress_sessions(&fixture).await, 0);
    assert!(fixture
        .auth
        .resolve_agent_interface_hostname(AGENT_HOST)
        .await
        .unwrap()
        .is_none());
    let response = send(
        &fixture,
        allow(),
        &config,
        navigation(AGENT_HOST, "/")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        served.load(std::sync::atomic::Ordering::SeqCst),
        served_before
    );
}

#[tokio::test]
async fn console_logout_ends_agent_origin_sessions() {
    let fixture = agent_fixture().await;
    let config = config(None);
    establish_session(&fixture, allow(), &config, AGENT_HOST, "tok-member").await;
    establish_session(&fixture, allow(), &config, AGENT_HOST, "tok-owner").await;
    assert_eq!(ingress_sessions(&fixture).await, 2);
    fixture.auth.logout("tok-member").await.unwrap();
    assert_eq!(
        ingress_sessions(&fixture).await,
        1,
        "only the member's sessions end"
    );
    let remaining: String = sqlx::query_scalar("SELECT user_id FROM ingress_sessions")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(remaining, "owner");
}

#[tokio::test]
async fn ingress_hosts_refuse_same_site_and_foreign_origin_browser_requests() {
    let mut fixture = agent_fixture().await;
    let guard =
        crate::ingress_guard::IngressGuard::new(crate::ingress_guard::TrustedProxies::none());
    let config = config(Some(guard.clone()));
    let (_heads, served) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Immediate,
    );
    let cookie = establish_session(&fixture, allow(), &config, AGENT_HOST, "tok-member").await;
    let post = |site: Option<&str>| {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/prompt")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_LENGTH, "0");
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        request.body(Body::empty()).unwrap()
    };
    // Workspace host: a sibling origin on the same site is refused, as is a
    // cross-site page; same-origin, user-initiated and non-browser pass.
    for site in [Some("same-site"), Some("cross-site"), Some("weird")] {
        let before = served.load(std::sync::atomic::Ordering::SeqCst);
        let (status, code) = error_of(send(&fixture, allow(), &config, post(site)).await).await;
        assert_eq!(
            (status, code.as_str()),
            (StatusCode::FORBIDDEN, "ingress_request_rejected"),
            "{site:?}"
        );
        assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), before);
    }
    let mut doubled = post(Some("same-origin"));
    doubled
        .headers_mut()
        .append("sec-fetch-site", HeaderValue::from_static("same-origin"));
    let (status, _) = error_of(send(&fixture, allow(), &config, doubled).await).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "ambiguous metadata is refused"
    );
    // Without Fetch Metadata (old browser) a present Origin must name this
    // host; a foreign Origin is refused, a matching one passes (#164 b).
    {
        let host = post(None)
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut foreign = post(None);
        foreign.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://other.example"),
        );
        let (status, code) = error_of(send(&fixture, allow(), &config, foreign).await).await;
        assert_eq!(
            (status, code.as_str()),
            (StatusCode::FORBIDDEN, "ingress_request_rejected"),
            "foreign Origin without Fetch Metadata"
        );
        let mut own = post(None);
        own.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(&format!("https://{host}")).unwrap(),
        );
        let response = send(&fixture, allow(), &config, own).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "own Origin without Fetch Metadata"
        );
    }
    for site in [Some("same-origin"), Some("none"), None] {
        let response = send(&fixture, allow(), &config, post(site)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{site:?}");
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    // Upgrades must name this host as their Origin.
    for (origin, refused) in [
        ("https://other.apps.treer.example", true),
        ("https://app.treer.example", true),
        ("https://proxy.treer.example", true),
        (&format!("https://{AGENT_HOST}"), false),
    ] {
        let before = served.load(std::sync::atomic::Ordering::SeqCst);
        let result = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/ws")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, origin)
                .header(header::CONNECTION, "Upgrade")
                .header(header::UPGRADE, "websocket")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let (status, code) = error_of(result).await;
        if refused {
            assert_eq!(
                (status, code.as_str()),
                (StatusCode::FORBIDDEN, "ingress_request_rejected"),
                "{origin}"
            );
            assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), before);
        } else {
            assert_ne!(status, StatusCode::FORBIDDEN, "{origin}: {code}");
        }
    }
    // Public service hosts keep cross-site API callers (their own CORS) and
    // refuse only same-site siblings; the rejection is counted.
    let public = fixture
        .auth
        .ensure_app_ingress(
            &fixture.app,
            "owner",
            BASE,
            Some(ServiceIngressAccess::Public),
        )
        .await
        .unwrap();
    let public_post = |site: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/items")
            .header(header::HOST, &public.hostname)
            .header(header::CONTENT_LENGTH, "0")
            .header("sec-fetch-site", site)
            .body(Body::empty())
            .unwrap()
    };
    let (status, code) =
        error_of(send(&fixture, allow(), &config, public_post("same-site")).await).await;
    assert_eq!(
        (status, code.as_str()),
        (StatusCode::FORBIDDEN, "ingress_request_rejected")
    );
    assert_eq!(guard.counters(&public.ingress_id).request_rejected, 1);
    let response = send(&fixture, allow(), &config, public_post("cross-site"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn console_routes_refuse_an_agent_origin_as_browser_origin() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('member','member@example.test','Member','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','member','member','now')")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('tok-member','member','2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
        .execute(&auth.pool()).await.unwrap();
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity =
        IdentityIssuer::load(&auth, &Url::parse("https://proxy.treer.example/").unwrap())
            .await
            .unwrap();
    let app = router(
        super::tests::state_with_managed_agent().await,
        test_config(),
        auth.clone(),
        allow(),
        identity,
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap(),
        config(None),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let agent_origin = format!("https://{AGENT_HOST}");
    // An unsafe cookie-authenticated API route.
    for site in [None, Some("same-site"), Some("same-origin")] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/workspaces/default/agents/agent-a/prompt")
            .header(header::HOST, "proxy.treer.example")
            .header(header::COOKIE, "treer_session=tok-member")
            .header(header::ORIGIN, &agent_origin)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(r#"{"text":"hi"}"#)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{site:?}");
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
            "csrf_denied",
            "{site:?}"
        );
    }
    // The workspace event stream upgrade.
    let upgrade = |origin: &str| {
        Request::builder()
            .uri("/api/workspaces/default/events")
            .header(header::HOST, "proxy.treer.example")
            .header(header::COOKIE, "treer_session=tok-member")
            .header(header::ORIGIN, origin)
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(upgrade(&agent_origin)).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(upgrade("https://app.treer.example"))
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "the Console origin itself is still accepted"
    );
}

#[tokio::test]
async fn agent_origin_url_is_hydrated_only_for_members_passing_the_gate() {
    // The evaluation window is process-global; this test flips Policy between
    // requests, so it runs without the window and restores it when done.
    struct RestoreWindow;
    impl Drop for RestoreWindow {
        fn drop(&mut self) {
            super::network::set_origin_gate_window_for_test(Duration::from_secs(5));
        }
    }
    let _restore = RestoreWindow;
    super::network::set_origin_gate_window_for_test(Duration::ZERO);
    let fixture = agent_fixture().await;
    let store = WorkspacePolicyStore::new(fixture.auth.pool());
    // Prompting is denied by default; metadata and discovery stay allowed, so
    // the Agent is visible but carries no origin.
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([(
                    crate::policy::ACTION_AGENT_PROMPT.into(),
                    PolicyEffect::Deny,
                )]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let messages = MessageStore::open(fixture.auth.pool()).await.unwrap();
    let build = |policy: PolicyEngine| {
        router(
            fixture.state.clone(),
            test_config(),
            fixture.auth.clone(),
            policy,
            fixture.identity.clone(),
            BrowserAccess::new(
                &Url::parse("https://app.treer.example/").unwrap(),
                &Url::parse("https://proxy.treer.example/").unwrap(),
            )
            .unwrap(),
            config(None),
            messages.clone(),
            CapabilityRollout::all_enabled(),
            crate::updater::UpdaterClient::disabled(),
            crate::voice::VoiceServices::disabled(),
        )
    };
    let get = |path: &str| {
        Request::builder()
            .uri(path)
            .header(header::HOST, "proxy.treer.example")
            .header(header::COOKIE, "treer_session=tok-member")
            .body(Body::empty())
            .unwrap()
    };
    let agent_path = format!("/api/workspaces/default/agents/{AGENT_ID}");
    let expected = format!("https://{AGENT_HOST}/");
    for (policy, present) in [
        (allow(), true),
        (PolicyEngine::durable(store.clone()), false),
    ] {
        let app = build(policy);
        let response = app.clone().oneshot(get(&agent_path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let agent: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            agent["interface"]["origin_url"].as_str(),
            present.then_some(expected.as_str()),
            "agent read model, gate passed = {present}"
        );
        let response = app
            .clone()
            .oneshot(get("/api/workspaces/default/snapshot"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        let snapshot: Value = serde_json::from_slice(&body).unwrap();
        let listed = snapshot["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["agent_id"] == AGENT_ID)
            .expect("agent listed");
        assert_eq!(
            listed["interface"]["origin_url"].as_str(),
            present.then_some(expected.as_str()),
            "snapshot, gate passed = {present}"
        );
    }
    // Hydration never creates the origin row; only authorize does.
    assert_eq!(agent_rows(&fixture).await, 0);
    // With the window on, a member's result is reused only for the same
    // Policy revision: the allowing revision 2 is remembered, a published
    // revision 3 denying prompt is re-evaluated at once by a fresh engine
    // (fresh evaluator cache), and the bound is per member (the owner is
    // evaluated afresh under the remembered revision).
    super::network::set_origin_gate_window_for_test(Duration::from_secs(60));
    let read = |app: Router, token: &str| {
        let request = Request::builder()
            .uri(&agent_path)
            .header(header::HOST, "proxy.treer.example")
            .header(header::COOKIE, format!("treer_session={token}"))
            .body(Body::empty())
            .unwrap();
        async move {
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let agent: Value = serde_json::from_slice(&body).unwrap();
            agent["interface"]["origin_url"].as_str().map(str::to_owned)
        }
    };
    let allow_all_document = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::new(),
        groups: BTreeMap::new(),
        rules: vec![],
    };
    let owner = PolicyPrincipalRef {
        kind: PolicyPrincipalKind::Human,
        id: "owner".into(),
    };
    store
        .replace(
            "default",
            1,
            PolicyMode::Enforce,
            allow_all_document,
            owner.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        read(build(PolicyEngine::durable(store.clone())), "tok-member").await,
        Some(expected.clone()),
        "revision 2 allows"
    );
    let deny_prompt = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::from([(
            crate::policy::ACTION_AGENT_PROMPT.into(),
            PolicyEffect::Deny,
        )]),
        groups: BTreeMap::new(),
        rules: vec![],
    };
    store
        .replace("default", 2, PolicyMode::Enforce, deny_prompt, owner)
        .await
        .unwrap();
    assert_eq!(
        read(build(PolicyEngine::durable(store.clone())), "tok-member").await,
        None,
        "revision 3 is a new key: denied without waiting for the window"
    );
    // The remembered allow for revision 2 is never served to another member.
    let fixed_revision_two = PolicyEngine::new(crate::policy::PolicyDecision::Allow, vec![]);
    assert_eq!(
        read(build(fixed_revision_two.clone()), "tok-member").await,
        Some(expected.clone()),
        "an engine without a revision has its own key and is evaluated"
    );
    assert_eq!(
        read(build(deny()), "tok-member").await,
        Some(expected.clone()),
        "within the window, the same member, Agent and (absent) revision reuse the result"
    );
    assert_eq!(
        read(build(deny()), "tok-owner").await,
        None,
        "another member is evaluated afresh"
    );
    // Revision reporting: a durable engine reports the published revision, a
    // fixed engine has none, and an engine whose store cannot be reached
    // reports Unavailable, which bypasses the window entirely.
    use crate::policy::PolicyRevision;
    assert_eq!(
        PolicyEngine::durable(store.clone())
            .policy_revision("default")
            .await,
        PolicyRevision::Known(3)
    );
    assert_eq!(
        allow().policy_revision("default").await,
        PolicyRevision::Unrevisioned
    );
    let unreachable = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgres://127.0.0.1:1/unreachable")
        .unwrap();
    assert_eq!(
        PolicyEngine::durable(WorkspacePolicyStore::new(unreachable))
            .policy_revision("default")
            .await,
        PolicyRevision::Unavailable
    );
    let Json(health) = super::health().await;
    assert!(health["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!(treer_protocol::CAPABILITY_AGENT_INTERFACE_ORIGIN)));
}
