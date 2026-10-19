//! Embed sessions (#156) at the handler level against isolated PostgreSQL and
//! the fake machine tunnel: the mint gate, the fixed bootstrap page, code
//! binding (nonce, Agent origin, single use, expiry, membership), the
//! partitioned cookie, Console-only framing of embed responses with standalone
//! sessions untouched, cookie-kind separation, logout, and the refuse fallback.
use super::agent_origin_tests::{
    agent_fixture, allow, deny, error_of, establish_session, ingress_sessions, send, AGENT_HOST,
    AGENT_ID, UNKNOWN_HOST,
};
use super::ingress_protection_tests::{config, upstream, Fixture, Upstream};
use super::*;
use sqlx::Row as _;

const CONSOLE: &str = "https://app.treer.example";

fn session(user: &str) -> CurrentSession {
    CurrentSession {
        token: format!("tok-{user}"),
        user_id: user.into(),
        email: format!("{user}@example.test"),
        preferred_name: user.into(),
    }
}

fn nonce_pair() -> (String, String) {
    let nonce = Uuid::new_v4().simple().to_string() + &Uuid::new_v4().simple().to_string();
    let hash = auth::ingress_nonce_hash(&nonce);
    (nonce, hash)
}

async fn mint(
    fixture: &Fixture,
    policy: PolicyEngine,
    config: &IngressConfig,
    user: Option<&str>,
    agent: &str,
    nonce_hash: &str,
) -> Result<Response, ApiFailure> {
    mint_with_purpose(fixture, policy, config, user, agent, nonce_hash, None).await
}

async fn mint_with_purpose(
    fixture: &Fixture,
    policy: PolicyEngine,
    config: &IngressConfig,
    user: Option<&str>,
    agent: &str,
    nonce_hash: &str,
    purpose: Option<&str>,
) -> Result<Response, ApiFailure> {
    mint_agent_interface_embed(
        State(fixture.state.clone()),
        Extension(fixture.auth.clone()),
        Extension(policy),
        Extension(config.clone()),
        user.map(|user| Extension(session(user))),
        Path(("default".to_string(), agent.to_string())),
        Json(EmbedAuthorizationRequest {
            browser_nonce_hash: nonce_hash.to_string(),
            purpose: purpose.map(str::to_owned),
        }),
    )
    .await
}

/// Mint with a purpose and redeem; returns (cookie pair, Set-Cookie line).
async fn establish_embed_session_with_purpose(
    fixture: &Fixture,
    config: &IngressConfig,
    user: &str,
    purpose: Option<&str>,
) -> (String, String) {
    let (nonce, hash) = nonce_pair();
    let response = mint_with_purpose(
        fixture,
        allow(),
        config,
        Some(user),
        AGENT_ID,
        &hash,
        purpose,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let response = redeem(fixture, config, AGENT_HOST, &code, &nonce)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let pair = cookie.split_once("; ").unwrap().0.to_string();
    (pair, cookie)
}

async fn minted_code(fixture: &Fixture, config: &IngressConfig, user: &str) -> (String, String) {
    let (nonce, hash) = nonce_pair();
    let response = mint(fixture, allow(), config, Some(user), AGENT_ID, &hash)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["origin"], format!("https://{AGENT_HOST}"));
    assert_eq!(value["expires_in"], 30);
    let code = value["code"].as_str().unwrap().to_string();
    assert!(code.starts_with("iec_v1_"), "{code}");
    (code, nonce)
}

fn redeem_request(host: &str, code: &str, nonce: &str) -> axum::http::request::Builder {
    let _ = (code, nonce);
    Request::builder()
        .method("POST")
        .uri("/.treer/embed/session")
        .header(header::HOST, host)
        .header("sec-fetch-site", "same-origin")
        .header("sec-fetch-mode", "cors")
        .header(header::CONTENT_TYPE, "application/json")
}

fn redeem_body(code: &str, nonce: &str) -> Body {
    Body::from(json!({ "code": code, "nonce": nonce }).to_string())
}

async fn redeem(
    fixture: &Fixture,
    config: &IngressConfig,
    host: &str,
    code: &str,
    nonce: &str,
) -> Result<Response, ApiFailure> {
    send(
        fixture,
        allow(),
        config,
        redeem_request(host, code, nonce)
            .body(redeem_body(code, nonce))
            .unwrap(),
    )
    .await
}

/// Mint and redeem for `user`; returns the embed cookie pair.
pub(super) async fn establish_embed_session(
    fixture: &Fixture,
    config: &IngressConfig,
    user: &str,
) -> String {
    let (code, nonce) = minted_code(fixture, config, user).await;
    let response = redeem(fixture, config, AGENT_HOST, &code, &nonce)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    let (pair, attributes) = cookie.split_once("; ").unwrap();
    assert!(
        pair.starts_with("__Host-treer_ingress_embed=ies_"),
        "{cookie}"
    );
    assert_eq!(
        attributes,
        "Path=/; Secure; HttpOnly; SameSite=None; Partitioned; Max-Age=43200"
    );
    pair.to_string()
}

async fn check(
    fixture: &Fixture,
    config: &IngressConfig,
    host: &str,
    cookie: Option<&str>,
) -> StatusCode {
    // The bootstrap's own fetch is same-origin; the check requires it (5580).
    let mut request = Request::builder()
        .uri("/.treer/embed/check")
        .header(header::HOST, host)
        .header("sec-fetch-site", "same-origin");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    match send(
        fixture,
        allow(),
        config,
        request.body(Body::empty()).unwrap(),
    )
    .await
    {
        Ok(response) => response.status(),
        Err(failure) => failure.status,
    }
}

#[tokio::test]
async fn embed_mint_applies_membership_and_the_interface_gate() {
    let fixture = agent_fixture().await;
    let config = config(None);
    let (_, hash) = nonce_pair();
    assert_eq!(
        error_of(mint(&fixture, allow(), &config, None, AGENT_ID, &hash).await).await,
        (StatusCode::UNAUTHORIZED, "authentication_required".into())
    );
    // A signed-in non-member is answered like an unknown id (#164 a).
    assert_eq!(
        error_of(
            mint(
                &fixture,
                allow(),
                &config,
                Some("stranger"),
                AGENT_ID,
                &hash
            )
            .await
        )
        .await,
        (StatusCode::NOT_FOUND, "agent_not_found".into())
    );
    assert_eq!(
        error_of(
            mint(
                &fixture,
                allow(),
                &config,
                Some("member"),
                "ag_ffffffffffffffffffffffffffffffff",
                &hash
            )
            .await
        )
        .await,
        (StatusCode::NOT_FOUND, "agent_not_found".into())
    );
    assert_eq!(
        error_of(mint(&fixture, deny(), &config, Some("member"), AGENT_ID, &hash).await).await,
        (StatusCode::FORBIDDEN, "policy_denied".into())
    );
    assert_eq!(
        error_of(
            mint(
                &fixture,
                allow(),
                &config,
                Some("member"),
                AGENT_ID,
                "not-a-hash"
            )
            .await
        )
        .await,
        (StatusCode::BAD_REQUEST, "invalid_request".into())
    );
    // Without an HTTPS wildcard ingress domain nothing is minted or served.
    let plain = IngressConfig::new(
        Some(Url::parse("http://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse(CONSOLE).unwrap(),
    )
    .unwrap();
    assert_eq!(
        error_of(mint(&fixture, allow(), &plain, Some("member"), AGENT_ID, &hash).await).await,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "embed_not_configured".into()
        )
    );
    assert_eq!(
        error_of(
            send(
                &fixture,
                allow(),
                &plain,
                Request::builder()
                    .uri("/.treer/embed")
                    .header(header::HOST, AGENT_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        )
        .await,
        (StatusCode::NOT_FOUND, "route_not_found".into())
    );
    // The member passes and the code is bound to this Agent origin.
    let codes_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ingress_browser_auth_codes")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap();
    minted_code(&fixture, &config, "member").await;
    let row = sqlx::query(
        "SELECT c.embed, c.agent_id, i.hostname FROM ingress_browser_auth_codes c \
         JOIN service_ingresses i ON i.ingress_id = c.ingress_id ORDER BY c.expires_at DESC LIMIT 1",
    )
    .fetch_one(&fixture.auth.pool())
    .await
    .unwrap();
    assert!(row.get::<bool, _>("embed"));
    assert_eq!(row.get::<String, _>("agent_id"), AGENT_ID);
    assert_eq!(row.get::<String, _>("hostname"), AGENT_HOST);
    let codes_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ingress_browser_auth_codes")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(codes_after, codes_before + 1);
    // Security 4750 P3-A: at most 8 unused, unexpired codes per user; the
    // ninth mint is refused until one is redeemed or expires, and another
    // user's budget is independent.
    for _ in 0..7 {
        minted_code(&fixture, &config, "member").await;
    }
    let (_, hash) = nonce_pair();
    assert_eq!(
        error_of(mint(&fixture, allow(), &config, Some("member"), AGENT_ID, &hash).await).await,
        (StatusCode::TOO_MANY_REQUESTS, "embed_rate_limited".into())
    );
    minted_code(&fixture, &config, "owner").await;
    sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = $1 WHERE user_id = 'member' AND embed = TRUE")
        .bind(Utc::now().to_rfc3339())
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    minted_code(&fixture, &config, "member").await;
}

#[tokio::test]
async fn embed_bootstrap_page_is_fixed_and_frameable_by_the_console_only() {
    let fixture = agent_fixture().await;
    let config = config(None);
    for host in [AGENT_HOST, UNKNOWN_HOST] {
        let response = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/.treer/embed")
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
        let headers = response.headers().clone();
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.contains(&format!("frame-ancestors {CONSOLE}")), "{csp}");
        assert!(csp.contains("script-src 'nonce-"), "{csp}");
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        assert!(headers.get(header::SET_COOKIE).is_none());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("/.treer/embed/session"));
        assert!(
            body.contains(&format!("var CONSOLE = \"{CONSOLE}\"")),
            "{body}"
        );
        // A failure report after the handshake began carries the nonce hash,
        // which only the bootstrap document knows (web-dev 4782).
        assert!(
            body.contains("if (hash) message.nonce_hash = hash;"),
            "{body}"
        );
        assert!(
            !body.contains(AGENT_ID),
            "the page reveals nothing about the Agent"
        );
    }
    // Only GET/HEAD serve it; other reserved paths stay closed.
    for (method, path) in [("POST", "/.treer/embed"), ("GET", "/.treer/embed/other")] {
        assert_eq!(
            error_of(
                send(
                    &fixture,
                    allow(),
                    &config,
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(header::HOST, AGENT_HOST)
                        .header("sec-fetch-site", "same-origin")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
            )
            .await,
            (StatusCode::NOT_FOUND, "route_not_found".into()),
            "{method} {path}"
        );
    }
}

#[tokio::test]
async fn embed_code_redeems_once_on_its_agent_origin_with_the_right_nonce() {
    let fixture = agent_fixture().await;
    let config = config(None);
    // A wrong nonce burns the code: the right nonce no longer works.
    let (code, nonce) = minted_code(&fixture, &config, "member").await;
    let (other_nonce, _) = nonce_pair();
    assert_eq!(
        error_of(redeem(&fixture, &config, AGENT_HOST, &code, &other_nonce).await).await,
        (
            StatusCode::UNAUTHORIZED,
            "invalid_ingress_authorization".into()
        )
    );
    assert_eq!(
        error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await,
        (
            StatusCode::UNAUTHORIZED,
            "invalid_ingress_authorization".into()
        )
    );
    // Another Agent origin cannot redeem it (and learns nothing).
    let (code, nonce) = minted_code(&fixture, &config, "member").await;
    assert_eq!(
        error_of(redeem(&fixture, &config, UNKNOWN_HOST, &code, &nonce).await).await,
        (
            StatusCode::UNAUTHORIZED,
            "invalid_ingress_authorization".into()
        )
    );
    // A cross-site browser request is refused before anything is consumed.
    let (code, nonce) = minted_code(&fixture, &config, "member").await;
    assert_eq!(
        error_of(
            send(
                &fixture,
                allow(),
                &config,
                Request::builder()
                    .method("POST")
                    .uri("/.treer/embed/session")
                    .header(header::HOST, AGENT_HOST)
                    .header("sec-fetch-site", "cross-site")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(redeem_body(&code, &nonce))
                    .unwrap(),
            )
            .await
        )
        .await,
        (StatusCode::FORBIDDEN, "ingress_request_rejected".into())
    );
    // The same-origin redeem succeeds once, with the partitioned cookie.
    let response = redeem(&fixture, &config, AGENT_HOST, &code, &nonce)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("; Partitioned; "), "{cookie}");
    assert!(cookie.contains("; SameSite=None; "), "{cookie}");
    assert!(cookie.contains("; Secure; "), "{cookie}");
    let pair = cookie.split(';').next().unwrap().to_string();
    // Replay fails; the check proves the cookie is live for this host only.
    assert_eq!(
        error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await,
        (
            StatusCode::UNAUTHORIZED,
            "invalid_ingress_authorization".into()
        )
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&pair)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        check(&fixture, &config, UNKNOWN_HOST, Some(&pair)).await,
        StatusCode::UNAUTHORIZED
    );
    // An embed token presented through the standalone cookie is nobody.
    let standalone_name = pair.replace("__Host-treer_ingress_embed=", "__Host-treer_ingress=");
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&standalone_name)).await,
        StatusCode::UNAUTHORIZED
    );
    // Expiry is enforced at redeem time.
    let (code, nonce) = minted_code(&fixture, &config, "member").await;
    sqlx::query(
        "UPDATE ingress_browser_auth_codes SET expires_at = '2000-01-01T00:00:00Z' WHERE code = $1",
    )
    .bind(&code)
    .execute(&fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(
        error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await,
        (
            StatusCode::UNAUTHORIZED,
            "invalid_ingress_authorization".into()
        )
    );
    // Membership revoked between mint and redeem: no session, code burned.
    let (code, nonce) = minted_code(&fixture, &config, "member").await;
    sqlx::query("DELETE FROM organization_members WHERE user_id = 'member'")
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    let sessions_before = ingress_sessions(&fixture).await;
    let (status, _) = error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await;
    assert!(status.is_client_error(), "{status}");
    assert_eq!(ingress_sessions(&fixture).await, sessions_before);
    let used: Option<String> =
        sqlx::query_scalar("SELECT used_at FROM ingress_browser_auth_codes WHERE code = $1")
            .bind(&code)
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    assert!(used.is_some(), "the code was consumed");
}

#[tokio::test]
async fn embed_sessions_are_framed_by_the_console_only_and_standalone_sessions_are_untouched() {
    let mut fixture = agent_fixture().await;
    let config = config(None);
    let (heads, _) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Head(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\
             Content-Security-Policy: default-src 'self'; frame-ancestors 'none'\r\n\
             X-Frame-Options: DENY\r\n",
        ),
    );
    let embed = establish_embed_session(&fixture, &config, "member").await;
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/?x=1")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, format!("{embed}; app_pref=dark"))
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        format!("default-src 'self'; frame-ancestors {CONSOLE}")
    );
    assert!(response.headers().get(header::X_FRAME_OPTIONS).is_none());
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    // No timing headers on handshake or UI responses (security 5530).
    assert!(response.headers().get("server-timing").is_none());
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    let head = heads.lock().unwrap().last().cloned().unwrap();
    assert!(head.starts_with("GET /ui?x=1 HTTP/1.1\r\n"), "{head}");
    assert!(!head.to_ascii_lowercase().contains("cookie:"), "{head}");
    // The standalone session keeps the Agent's own framing policy.
    let standalone = establish_session(&fixture, allow(), &config, AGENT_HOST, "tok-member").await;
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &standalone)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "default-src 'self'; frame-ancestors 'none'"
    );
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    // Tokens do not cross cookie kinds, and the embed cookie means nothing on
    // a sibling origin: each is an unauthenticated navigation.
    let swapped_embed = embed.replace("__Host-treer_ingress_embed=", "__Host-treer_ingress=");
    let swapped_standalone =
        standalone.replace("__Host-treer_ingress=", "__Host-treer_ingress_embed=");
    for (host, cookie) in [
        (AGENT_HOST, swapped_embed.as_str()),
        (AGENT_HOST, swapped_standalone.as_str()),
        (UNKNOWN_HOST, embed.as_str()),
    ] {
        let response = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/")
                .header(header::HOST, host)
                .header(header::COOKIE, cookie)
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{host} {cookie}");
        assert!(response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("/.treer/ingress/authorize"));
    }
    // The sibling defence applies to the embed cookie: a same-site browser
    // POST is refused before the session is even looked at.
    assert_eq!(
        error_of(
            send(
                &fixture,
                allow(),
                &config,
                Request::builder()
                    .method("POST")
                    .uri("/api/prompt")
                    .header(header::HOST, AGENT_HOST)
                    .header(header::COOKIE, &embed)
                    .header("sec-fetch-site", "same-site")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        )
        .await,
        (StatusCode::FORBIDDEN, "ingress_request_rejected".into())
    );
    // A later Policy denial refuses the embed session per request.
    assert_eq!(
        error_of(
            send(
                &fixture,
                deny(),
                &config,
                Request::builder()
                    .uri("/")
                    .header(header::HOST, AGENT_HOST)
                    .header(header::COOKIE, &embed)
                    .header("sec-fetch-site", "same-origin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        )
        .await,
        (StatusCode::FORBIDDEN, "policy_denied".into())
    );
    // Security 5125/5129: the embed cookie grants authority only in a
    // same-origin context. In every other context it is ignored and the
    // request takes the standalone path: with only the embed cookie that is
    // the authorize redirect (303) or, when the request says it is not a
    // top-level navigation, 401 invalid_ingress_authorization; never 403 and
    // never an embedded (Console-framed) response.
    for (sites, origin, dest) in [
        (vec![], None, None),
        (vec!["none"], None, None),
        (vec!["same-site"], None, None),
        (vec!["cross-site"], None, None),
        (vec!["same-origin", "same-origin"], None, None),
        (vec![], Some(&*format!("https://{AGENT_HOST}")), None),
        (vec!["cross-site"], None, Some("iframe")),
        (vec!["same-site"], None, Some("iframe")),
    ] {
        let mut request = Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &embed);
        for site in &sites {
            request = request.header("sec-fetch-site", *site);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(dest) = dest {
            request = request.header("sec-fetch-dest", dest);
        }
        let (status, code) = error_of(
            send(
                &fixture,
                allow(),
                &config,
                request.body(Body::empty()).unwrap(),
            )
            .await,
        )
        .await;
        assert!(
            (status == StatusCode::SEE_OTHER && code.is_empty())
                || (status == StatusCode::UNAUTHORIZED && code == "invalid_ingress_authorization"),
            "{sites:?} {origin:?} {dest:?}: {status} {code}"
        );
    }
    // Admitted embedded requests: a same-origin fetch or navigation, and a
    // WebSocket upgrade that names this origin exactly without Fetch Metadata
    // (Chromium); a same-origin upgrade with Fetch Metadata too.
    let ws_origin = format!("https://{AGENT_HOST}");
    for (site, upgrade, origin) in [
        (Some("same-origin"), false, None),
        (None, true, Some(ws_origin.as_str())),
        (Some("same-origin"), true, Some(ws_origin.as_str())),
    ] {
        let mut request = Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &embed);
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        if upgrade {
            request = request
                .header(header::UPGRADE, "websocket")
                .header(header::CONNECTION, "Upgrade");
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = send(
            &fixture,
            allow(),
            &config,
            request.body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{site:?} {upgrade} {origin:?}"
        );
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            format!("default-src 'self'; frame-ancestors {CONSOLE}"),
            "embedded responses are framed by the Console only"
        );
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    // Upgrades (security 5165): exactly one Origin byte-equal to this origin
    // is required in BOTH site modes (no Fetch Metadata, and same-origin);
    // the missing-metadata exception is for real WebSocket upgrades only. A
    // failing case is never served as embedded: it is refused by the gateway
    // (403), or takes the standalone path (303 or 401).
    let exact = format!("https://{AGENT_HOST}");
    for site in [None, Some("same-origin")] {
        for (label, origins, upgrade, connection) in [
            ("missing", vec![], "websocket", "Upgrade"),
            ("null", vec!["null"], "websocket", "Upgrade"),
            (
                "foreign",
                vec!["https://evil.example"],
                "websocket",
                "Upgrade",
            ),
            (
                "nested",
                vec![&*format!("https://x.{AGENT_HOST}")],
                "websocket",
                "Upgrade",
            ),
            (
                "http",
                vec![&*format!("http://{AGENT_HOST}")],
                "websocket",
                "Upgrade",
            ),
            (
                "userinfo",
                vec![&*format!("https://user@{AGENT_HOST}/extra")],
                "websocket",
                "Upgrade",
            ),
            (
                "port",
                vec![&*format!("https://{AGENT_HOST}:8443")],
                "websocket",
                "Upgrade",
            ),
            (
                "duplicate",
                vec![exact.as_str(), exact.as_str()],
                "websocket",
                "Upgrade",
            ),
            (
                "h2c",
                vec![exact.as_str()],
                "h2c",
                "Upgrade, HTTP2-Settings",
            ),
            (
                "no-connection-upgrade",
                vec![exact.as_str()],
                "websocket",
                "keep-alive",
            ),
        ] {
            // A same-origin site with an h2c or keep-alive upgrade still has the
            // exact Origin and same-origin metadata, which the rule admits; the
            // metadata-less exception is what these two variants test.
            if site.is_some() && matches!(label, "h2c" | "no-connection-upgrade") {
                continue;
            }
            let mut request = Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, &embed)
                .header(header::UPGRADE, upgrade)
                .header(header::CONNECTION, connection);
            if let Some(site) = site {
                request = request.header("sec-fetch-site", site);
            }
            for origin in &origins {
                request = request.header(header::ORIGIN, *origin);
            }
            let outcome = send(
                &fixture,
                allow(),
                &config,
                request.body(Body::empty()).unwrap(),
            )
            .await;
            match outcome {
                Ok(response) => {
                    let framed_by_console = response
                        .headers()
                        .get(header::CONTENT_SECURITY_POLICY)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|csp| csp.contains(CONSOLE));
                    assert!(
                        response.status().is_redirection() || response.status().is_client_error(),
                        "{site:?} {label}: {}",
                        response.status()
                    );
                    assert!(!framed_by_console, "{site:?} {label}: served as embedded");
                }
                Err(failure) => assert!(
                    failure.status.is_client_error(),
                    "{site:?} {label}: {}",
                    failure.status
                ),
            }
        }
    }
    // A top-level navigation ignores the embed cookie: alone it is an
    // unauthenticated standalone visit (authorize redirect, never 403), and
    // next to the standalone cookie it is the standalone session with the
    // Agent's own framing policy, even though the browser sent both cookies
    // (same-site Console and apps domain, or Partitioned ignored).
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &embed)
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-site", "none")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, format!("{embed}; {standalone}"))
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-site", "none")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "default-src 'self'; frame-ancestors 'none'",
        "a standalone visit keeps the Agent's CSP even when the embed cookie rides along"
    );
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    // With both cookies, a same-origin fetch is the embed session (Console
    // framing) and a cross-site fetch falls back to the standalone session.
    for (site, expect_console) in [("same-origin", true), ("cross-site", false)] {
        let response = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, format!("{embed}; {standalone}"))
                .header("sec-fetch-site", site)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{site}");
        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(csp.contains(CONSOLE), expect_console, "{site}: {csp}");
        axum::body::to_bytes(response.into_body(), 64)
            .await
            .unwrap();
    }
    // The standalone session is not subject to that rule for safe requests.
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, &standalone)
            .header("sec-fetch-site", "none")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
}

#[tokio::test]
async fn console_logout_ends_embed_sessions() {
    let fixture = agent_fixture().await;
    let config = config(None);
    establish_embed_session(&fixture, &config, "member").await;
    establish_embed_session(&fixture, &config, "owner").await;
    assert_eq!(ingress_sessions(&fixture).await, 2);
    fixture.auth.logout("tok-member").await.unwrap();
    assert_eq!(ingress_sessions(&fixture).await, 1);
    let remaining: String = sqlx::query_scalar("SELECT user_id FROM ingress_sessions")
        .fetch_one(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(remaining, "owner");
}

#[test]
fn the_tunnel_fallback_can_be_refused_only_where_embed_sessions_exist() {
    let config = config(None);
    assert!(require_tunnel_fallback_allowed(&config).is_ok());
    let strict = config.clone().with_embed_fallback(EmbedFallback::Refuse);
    let failure = require_tunnel_fallback_allowed(&strict).unwrap_err();
    assert_eq!(failure.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failure.error.code, "embedded_ui_requires_isolated_origin");
    // Without an HTTPS wildcard domain there is no embed path to insist on.
    let plain = IngressConfig::new(
        None,
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse(CONSOLE).unwrap(),
    )
    .unwrap()
    .with_embed_fallback(EmbedFallback::Refuse);
    assert!(require_tunnel_fallback_allowed(&plain).is_ok());
    assert_eq!(
        "refuse".parse::<EmbedFallback>().unwrap(),
        EmbedFallback::Refuse
    );
    assert_eq!(
        "Tunnel".parse::<EmbedFallback>().unwrap(),
        EmbedFallback::Tunnel
    );
    assert!("maybe".parse::<EmbedFallback>().is_err());
}

#[tokio::test]
async fn embed_capability_is_advertised_only_where_embed_sessions_can_work() {
    let fixture = agent_fixture().await;
    let embed = treer_protocol::CAPABILITY_AGENT_INTERFACE_EMBED;
    let capabilities = |value: Value| -> Vec<String> {
        value["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect()
    };
    // HTTPS wildcard ingress domain: advertised.
    let Json(value) = crate::auth::oauth_config(
        Extension(fixture.auth.clone()),
        Some(Extension(config(None))),
    )
    .await;
    assert!(capabilities(value).iter().any(|c| c == embed));
    // Plain HTTP ingress domain, or no ingress configuration at all: absent, so
    // the Console keeps the legacy frame instead of refusing (incident 4890).
    let plain = IngressConfig::new(
        Some(Url::parse("http://apps.treer.example/").unwrap()),
        &Url::parse("https://proxy.treer.example/").unwrap(),
        &Url::parse(CONSOLE).unwrap(),
    )
    .unwrap();
    let Json(value) =
        crate::auth::oauth_config(Extension(fixture.auth.clone()), Some(Extension(plain))).await;
    assert!(!capabilities(value).iter().any(|c| c == embed));
    let Json(value) = crate::auth::oauth_config(Extension(fixture.auth.clone()), None).await;
    let list = capabilities(value);
    assert!(!list.iter().any(|c| c == embed));
    assert!(list
        .iter()
        .any(|c| c == treer_protocol::CAPABILITY_AGENT_INTERFACE_ORIGIN));
}

/// #198: the check behind session reuse answers 204 only under the full
/// request-path gate (same 401 otherwise), and the bootstrap speaks the
/// version-2 handshake only when the Console hints `reuse=1`.
#[tokio::test]
async fn embed_check_applies_the_interface_gate_and_reuse_hint_selects_the_v2_handshake() {
    let fixture = agent_fixture().await;
    let config = config(None);
    let cookie = establish_embed_session(&fixture, &config, "member").await;
    // No timing headers anywhere on the handshake (security 5530).
    let (nonce, hash) = nonce_pair();
    let minted = mint(&fixture, allow(), &config, Some("owner"), AGENT_ID, &hash)
        .await
        .unwrap();
    assert!(minted.headers().get("server-timing").is_none());
    let body = axum::body::to_bytes(minted.into_body(), 64 * 1024)
        .await
        .unwrap();
    let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let redeemed = redeem(&fixture, &config, AGENT_HOST, &code, &nonce)
        .await
        .unwrap();
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);
    assert!(redeemed.headers().get("server-timing").is_none());

    // The check: 204 with a live session and an allowing Policy; the same 401
    // as "no session" when the interface gate denies, when there is no cookie,
    // and when membership is gone. Never a reason, never a body.
    let check_with = |policy: PolicyEngine, cookie: Option<String>| {
        let fixture = &fixture;
        let config = &config;
        async move {
            let mut request = Request::builder()
                .uri("/.treer/embed/check")
                .header(header::HOST, AGENT_HOST)
                .header("sec-fetch-site", "same-origin");
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            send(
                fixture,
                policy,
                config,
                request.body(Body::empty()).unwrap(),
            )
            .await
        }
    };
    let ok = check_with(allow(), Some(cookie.clone())).await.unwrap();
    assert_eq!(ok.status(), StatusCode::NO_CONTENT);
    assert_eq!(ok.headers()[header::CACHE_CONTROL], "no-store");
    assert!(ok.headers().get("server-timing").is_none());
    let refused = (
        StatusCode::UNAUTHORIZED,
        "ingress_authentication_required".to_string(),
    );
    assert_eq!(
        error_of(check_with(deny(), Some(cookie.clone())).await).await,
        refused
    );
    assert_eq!(error_of(check_with(allow(), None).await).await, refused);
    // The cookie counts only in the same-origin context the UI request
    // requires (security 5580): every other Fetch Metadata shape, including a
    // duplicated header, is the same 401 even with a live session.
    for sites in [
        vec!["cross-site"],
        vec!["same-site"],
        vec!["none"],
        vec![],
        vec!["same-origin", "same-origin"],
    ] {
        let mut request = Request::builder()
            .uri("/.treer/embed/check")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, cookie.clone());
        for site in &sites {
            request = request.header("sec-fetch-site", *site);
        }
        assert_eq!(
            error_of(
                send(
                    &fixture,
                    allow(),
                    &config,
                    request.body(Body::empty()).unwrap()
                )
                .await
            )
            .await,
            refused,
            "{sites:?}"
        );
    }
    // One cause at a time, each on its own session: Policy deny (above),
    // expiry, server-side logout, membership removal.
    let expired = establish_embed_session(&fixture, &config, "owner").await;
    sqlx::query("UPDATE ingress_sessions SET expires_at = '2000-01-01T00:00:00Z' WHERE token = $1")
        .bind(expired.split_once('=').unwrap().1)
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(
        error_of(check_with(allow(), Some(expired)).await).await,
        refused
    );
    let logged_out = establish_embed_session(&fixture, &config, "owner").await;
    assert_eq!(
        check_with(allow(), Some(logged_out.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    fixture.auth.logout("tok-owner").await.unwrap();
    assert_eq!(
        error_of(check_with(allow(), Some(logged_out)).await).await,
        refused
    );
    sqlx::query("DELETE FROM organization_members WHERE user_id = 'member'")
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    assert_eq!(
        error_of(check_with(allow(), Some(cookie)).await).await,
        refused
    );

    // The bootstrap: `reuse=1` exactly selects the version-2 ready and enables
    // the probe handling; anything else keeps the two-field ready. The hint
    // is stripped before the UI navigation and nothing about the Agent leaks.
    for (query, reuse) in [
        ("", false),
        ("?reuse=1", true),
        ("?reuse=1&theme=dark", true),
        ("?reuse=2", false),
        ("?reuse=true", false),
        ("?theme=dark&reuse=1", true),
    ] {
        let response = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri(format!("/.treer/embed{query}"))
                .header(header::HOST, AGENT_HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        assert!(response.headers().get("server-timing").is_none());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        let v2 = [
            "post({ treer_embed: \"ready\", version: 2, nonce_hash: hash });",
            "data.treer_embed === \"probe\" && data.version === 2 && data.nonce_hash === hash && Object.keys(data).length === 3",
            "post({ treer_embed: \"reused\", version: 2, nonce_hash: hash }); finish();",
            "post({ treer_embed: \"mint\", version: 2, nonce_hash: hash });",
        ];
        for needle in v2 {
            assert_eq!(body.contains(needle), reuse, "{query}: {needle}\n{body}");
        }
        // An old Console's page carries no version-2 code at all.
        assert_eq!(body.contains("version"), reuse, "{query}");
        assert_eq!(body.contains("probe"), reuse, "{query}");
        assert_eq!(body.contains("reused"), reuse, "{query}");
        assert_eq!(
            body.contains("post({ treer_embed: \"ready\", nonce_hash: hash });"),
            !reuse,
            "{query}"
        );
        for needle in [
            "params.delete(\"reuse\");",
            "/.treer/embed/check",
            "if (hash) message.nonce_hash = hash;",
        ] {
            assert!(body.contains(needle), "{query}: missing {needle}\n{body}");
        }
        assert!(!body.contains(AGENT_ID), "{body}");
    }
}

/// #198 step 3 (security 5580/5583/5595): prewarm mints have their own budget
/// and redeem into 10-minute sessions; live embed sessions per (user, Agent)
/// are capped with unrotated prewarm rows evicted first; the first authorized
/// document navigation rotates a prewarm session into a 12-hour one and keeps
/// the old row until its own expiry; every mint is audited by purpose.
#[tokio::test]
async fn prewarm_sessions_are_short_budgeted_capped_rotated_and_audited() {
    let fixture = agent_fixture().await;
    let config = config(None);
    // Purpose validation and the cookie lifetimes.
    let (_, hash) = nonce_pair();
    assert_eq!(
        error_of(
            mint_with_purpose(
                &fixture,
                allow(),
                &config,
                Some("member"),
                AGENT_ID,
                &hash,
                Some("hover")
            )
            .await
        )
        .await,
        (StatusCode::BAD_REQUEST, "invalid_request".into())
    );
    let (open_pair, open_cookie) =
        establish_embed_session_with_purpose(&fixture, &config, "member", Some("open")).await;
    assert!(open_cookie.ends_with("Max-Age=43200"), "{open_cookie}");
    let (warm_pair, warm_cookie) =
        establish_embed_session_with_purpose(&fixture, &config, "member", Some("prewarm")).await;
    assert!(warm_cookie.ends_with("Max-Age=600"), "{warm_cookie}");
    let warm_token = warm_pair.split_once('=').unwrap().1.to_string();
    let (prewarm, expires_at): (bool, String) =
        sqlx::query_as("SELECT prewarm, expires_at FROM ingress_sessions WHERE token = $1")
            .bind(&warm_token)
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    assert!(prewarm);
    let expires = expires_at.parse::<chrono::DateTime<Utc>>().unwrap();
    let left = expires - Utc::now();
    assert!(
        left <= chrono::Duration::minutes(10) && left > chrono::Duration::minutes(9),
        "{left}"
    );
    // Audit by purpose.
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM organization_audit_events WHERE action LIKE 'agent.interface.embed.%' ORDER BY sequence",
    )
    .fetch_all(&fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "agent.interface.embed.minted",
            "agent.interface.embed.prewarmed"
        ]
    );

    // Prewarm budget: 6 per user per minute, independent of the 30 overall;
    // the 7th prewarm is refused while an open mint still passes.
    for i in 0..5 {
        let (_, hash) = nonce_pair();
        let response = mint_with_purpose(
            &fixture,
            allow(),
            &config,
            Some("member"),
            AGENT_ID,
            &hash,
            Some("prewarm"),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "prewarm mint {i}");
        // Keep the outstanding-code cap out of the way: redeem each one.
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        let _ = code;
        sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
            .execute(&fixture.auth.pool())
            .await
            .unwrap();
    }
    let (_, hash) = nonce_pair();
    assert_eq!(
        error_of(
            mint_with_purpose(
                &fixture,
                allow(),
                &config,
                Some("member"),
                AGENT_ID,
                &hash,
                Some("prewarm")
            )
            .await
        )
        .await,
        (StatusCode::TOO_MANY_REQUESTS, "embed_rate_limited".into())
    );
    let (_, hash) = nonce_pair();
    assert_eq!(
        mint(&fixture, allow(), &config, Some("member"), AGENT_ID, &hash)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
        .execute(&fixture.auth.pool())
        .await
        .unwrap();

    // Rotation: a document navigation with the prewarm cookie gets a new
    // 12-hour cookie; the prewarm row is marked rotated and still valid; a
    // subresource request never rotates.
    let mut fixture_with_upstream = fixture;
    let (_, _) = upstream(
        fixture_with_upstream.state.clone(),
        fixture_with_upstream.connection,
        fixture_with_upstream.rx.take().unwrap(),
        Upstream::Head("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n"),
    );
    let fixture = fixture_with_upstream;
    let navigate = |cookie: String, navigation: bool| {
        let fixture = &fixture;
        let config = &config;
        async move {
            let mut request = Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, cookie)
                .header("sec-fetch-site", "same-origin");
            if navigation {
                request = request
                    .header("sec-fetch-mode", "navigate")
                    .header("sec-fetch-dest", "iframe");
            }
            let response = send(
                fixture,
                allow(),
                config,
                request.body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let set_cookie = response
                .headers()
                .get(header::SET_COOKIE)
                .map(|v| v.to_str().unwrap().to_string());
            axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            set_cookie
        }
    };
    assert_eq!(
        navigate(warm_pair.clone(), false).await,
        None,
        "subresource never rotates"
    );
    assert_eq!(
        navigate(open_pair.clone(), true).await,
        None,
        "a normal session never rotates"
    );
    let rotated = navigate(warm_pair.clone(), true)
        .await
        .expect("rotation sets the new cookie");
    assert!(
        rotated.starts_with("__Host-treer_ingress_embed=ies_")
            && rotated.ends_with("Max-Age=43200"),
        "{rotated}"
    );
    let new_pair = rotated.split_once("; ").unwrap().0.to_string();
    assert_ne!(new_pair, warm_pair);
    // Old row: rotated, still authenticating until its own expiry; rotating it
    // again does nothing.
    let (old_rotated, old_prewarm): (bool, bool) =
        sqlx::query_as("SELECT rotated, prewarm FROM ingress_sessions WHERE token = $1")
            .bind(&warm_token)
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    assert!(old_rotated && old_prewarm);
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&warm_pair)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        navigate(warm_pair.clone(), true).await,
        None,
        "rotation happens once"
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&new_pair)).await,
        StatusCode::NO_CONTENT
    );
    let (new_prewarm, new_expires): (bool, String) =
        sqlx::query_as("SELECT prewarm, expires_at FROM ingress_sessions WHERE token = $1")
            .bind(new_pair.split_once('=').unwrap().1)
            .fetch_one(&fixture.auth.pool())
            .await
            .unwrap();
    assert!(!new_prewarm);
    assert!(
        new_expires.parse::<chrono::DateTime<Utc>>().unwrap() - Utc::now()
            > chrono::Duration::hours(11)
    );

    // Cap: at most 8 live embed sessions per (user, Agent); unrotated prewarm
    // rows go first, then the oldest; the newest row always survives.
    let live = |user: &'static str| {
        let pool = fixture.auth.pool();
        async move {
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT count(*), count(*) FILTER (WHERE prewarm AND NOT rotated) FROM ingress_sessions \
                 WHERE user_id = $1 AND embed = TRUE AND expires_at > $2",
            )
            .bind(user)
            .bind(Utc::now().to_rfc3339())
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    // Owner: 2 unrotated prewarm + 7 open sessions -> 8 live, prewarm gone first.
    establish_embed_session_with_purpose(&fixture, &config, "owner", Some("prewarm")).await;
    establish_embed_session_with_purpose(&fixture, &config, "owner", Some("prewarm")).await;
    let mut owner_pairs = Vec::new();
    for _ in 0..7 {
        owner_pairs.push(
            establish_embed_session_with_purpose(&fixture, &config, "owner", None)
                .await
                .0,
        );
        sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
            .execute(&fixture.auth.pool())
            .await
            .unwrap();
    }
    assert_eq!(live("owner").await, (8, 1), "one prewarm evicted first");
    owner_pairs.push(
        establish_embed_session_with_purpose(&fixture, &config, "owner", None)
            .await
            .0,
    );
    assert_eq!(live("owner").await, (8, 0), "second prewarm evicted next");
    owner_pairs.push(
        establish_embed_session_with_purpose(&fixture, &config, "owner", None)
            .await
            .0,
    );
    assert_eq!(live("owner").await, (8, 0));
    // The oldest open session is the one evicted now; the newest still works.
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&owner_pairs[0])).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        check(
            &fixture,
            &config,
            AGENT_HOST,
            Some(owner_pairs.last().unwrap())
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&owner_pairs[1])).await,
        StatusCode::NO_CONTENT
    );

    // A hover never evicts a normal or rotated session (web-dev 5615): with
    // the cap full of normal rows the prewarm exchange is refused and nothing
    // else changes.
    let (nonce, hash) = nonce_pair();
    let minted = mint_with_purpose(
        &fixture,
        allow(),
        &config,
        Some("owner"),
        AGENT_ID,
        &hash,
        Some("prewarm"),
    )
    .await
    .unwrap();
    let body = axum::body::to_bytes(minted.into_body(), 64 * 1024)
        .await
        .unwrap();
    let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, error) =
        error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await;
    assert_eq!(
        (status, error.as_str()),
        (StatusCode::UNAUTHORIZED, "invalid_ingress_authorization")
    );
    assert_eq!(live("owner").await, (8, 0));
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&owner_pairs[1])).await,
        StatusCode::NO_CONTENT
    );

    // At the cap, rotating the oldest prewarm row never evicts that row: the
    // predecessor keeps its own expiry beside the eight others (5615).
    fixture.auth.logout("tok-owner").await.unwrap();
    sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES('tok-owner','owner',$1,'2999-01-01T00:00:00Z')")
        .bind(Utc::now().to_rfc3339())
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    let (oldest_warm, _) =
        establish_embed_session_with_purpose(&fixture, &config, "owner", Some("prewarm")).await;
    let mut normals = Vec::new();
    for _ in 0..7 {
        normals.push(
            establish_embed_session_with_purpose(&fixture, &config, "owner", None)
                .await
                .0,
        );
        sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
            .execute(&fixture.auth.pool())
            .await
            .unwrap();
    }
    assert_eq!(live("owner").await, (8, 1));
    let rotated = navigate(oldest_warm.clone(), true)
        .await
        .expect("rotation at the cap");
    let successor = rotated.split_once("; ").unwrap().0.to_string();
    assert_eq!(
        live("owner").await,
        (9, 0),
        "predecessor kept beside eight live rows"
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&oldest_warm)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&successor)).await,
        StatusCode::NO_CONTENT
    );
    for pair in &normals {
        assert_eq!(
            check(&fixture, &config, AGENT_HOST, Some(pair)).await,
            StatusCode::NO_CONTENT
        );
    }

    // The rotated grace row stays outside the cap for later inserts too
    // (security 5625 (1)): another open at cap evicts the oldest NORMAL row,
    // never the grace row, and a hover is refused because eight normal rows
    // remain.
    normals.push(
        establish_embed_session_with_purpose(&fixture, &config, "owner", None)
            .await
            .0,
    );
    assert_eq!(live("owner").await, (9, 0), "grace row + eight normals");
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&oldest_warm)).await,
        StatusCode::NO_CONTENT,
        "grace row survives a later insert at cap"
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&normals[0])).await,
        StatusCode::UNAUTHORIZED,
        "the oldest normal row went instead"
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&normals[1])).await,
        StatusCode::NO_CONTENT
    );
    sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
        .execute(&fixture.auth.pool())
        .await
        .unwrap();
    let (nonce, hash) = nonce_pair();
    let minted = mint_with_purpose(
        &fixture,
        allow(),
        &config,
        Some("owner"),
        AGENT_ID,
        &hash,
        Some("prewarm"),
    )
    .await
    .unwrap();
    let body = axum::body::to_bytes(minted.into_body(), 64 * 1024)
        .await
        .unwrap();
    let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = error_of(redeem(&fixture, &config, AGENT_HOST, &code, &nonce).await).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "hover refused at a cap of normals"
    );
    assert_eq!(live("owner").await, (9, 0));

    // Logout removes prewarm sessions like any other.
    fixture.auth.logout("tok-owner").await.unwrap();
    assert_eq!(live("owner").await, (0, 0));
}

/// #198 (security 5618 item 4): concurrent exchanges for one (user, Agent)
/// serialize on the pair lock, so the cap is a real bound, never briefly 8+n.
#[tokio::test]
async fn concurrent_embed_exchanges_never_exceed_the_cap() {
    let fixture = agent_fixture().await;
    let config = config(None);
    // Six live normal sessions, then eight codes redeemed at the same time.
    for _ in 0..6 {
        establish_embed_session_with_purpose(&fixture, &config, "member", None).await;
        sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
            .execute(&fixture.auth.pool())
            .await
            .unwrap();
    }
    let mut codes = Vec::new();
    for _ in 0..8 {
        let (nonce, hash) = nonce_pair();
        let minted = mint(&fixture, allow(), &config, Some("member"), AGENT_ID, &hash)
            .await
            .unwrap();
        assert_eq!(minted.status(), StatusCode::OK);
        let body = axum::body::to_bytes(minted.into_body(), 64 * 1024)
            .await
            .unwrap();
        let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        codes.push((code, nonce));
    }
    let results = futures_util::future::join_all(
        codes
            .iter()
            .map(|(code, nonce)| redeem(&fixture, &config, AGENT_HOST, code, nonce)),
    )
    .await;
    assert!(results.iter().all(|r| r
        .as_ref()
        .map(|r| r.status() == StatusCode::NO_CONTENT)
        .unwrap_or(false)));
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ingress_sessions WHERE user_id = 'member' AND embed = TRUE AND expires_at > $1 AND NOT rotated",
    )
    .bind(Utc::now().to_rfc3339())
    .fetch_one(&fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(live, 8, "fourteen inserts, eight survive");
}

/// #198 step 3: the prewarm bootstrap exists only with both flags, never
/// navigates, and uses the `warmed` / `warm_failed` terminal shapes; every
/// other page carries no prewarm text.
#[tokio::test]
async fn prewarm_bootstrap_is_rendered_only_with_both_flags_and_never_navigates() {
    let fixture = agent_fixture().await;
    let config = config(None);
    for (query, prewarm, reuse) in [
        ("?reuse=1&prewarm=1", true, true),
        ("?prewarm=1&reuse=1&theme=dark", true, true),
        ("?prewarm=1", false, false),
        ("?reuse=1", false, true),
        ("?reuse=1&prewarm=true", false, true),
        ("", false, false),
    ] {
        let response = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri(format!("/.treer/embed{query}"))
                .header(header::HOST, AGENT_HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(body.contains("warmed"), prewarm, "{query}");
        assert_eq!(body.contains("warm_failed"), prewarm, "{query}");
        assert_eq!(
            body.contains("window.location.replace(next)"),
            !prewarm,
            "{query}: a prewarm page never navigates"
        );
        assert_eq!(
            body.contains("\"reused\""),
            reuse && !prewarm,
            "{query}: prewarm never reports reused"
        );
        assert_eq!(body.contains("\"probe\""), reuse, "{query}");
        assert!(
            body.contains("params.delete(\"reuse\"); params.delete(\"prewarm\");"),
            "{query}"
        );
        if prewarm {
            assert!(
                body.contains("post({ treer_embed: \"warmed\", version: 2, nonce_hash: hash });"),
                "{body}"
            );
            assert!(
                body.contains(
                    "var message = { treer_embed: \"warm_failed\", version: 2, nonce_hash: hash };"
                ),
                "{body}"
            );
            assert!(!body.contains("cookies_blocked\" }"), "{body}");
        } else {
            assert!(
                body.contains("var message = { treer_embed: kind };"),
                "{body}"
            );
        }
        assert!(!body.contains(AGENT_ID));
    }
}

/// #198 (security 5618 item 3): rotation happens only once the upstream
/// response exists and the request passed the gate; a tunnel failure or a
/// Policy denial leaves the prewarm row unrotated so the next navigation can
/// still rotate it.
#[tokio::test]
async fn prewarm_rotation_waits_for_the_upstream_response_and_the_gate() {
    let mut fixture = agent_fixture().await;
    let config = config(None);
    let (warm_pair, _) =
        establish_embed_session_with_purpose(&fixture, &config, "member", Some("prewarm")).await;
    let warm_token = warm_pair.split_once('=').unwrap().1.to_string();
    let rotated = |pool: sqlx::PgPool, token: String| async move {
        sqlx::query_as::<_, (bool, i64)>(
            "SELECT (SELECT rotated FROM ingress_sessions WHERE token = $1), \
             (SELECT count(*) FROM ingress_sessions WHERE user_id = 'member' AND embed = TRUE)",
        )
        .bind(token)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let navigation = |cookie: String| {
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, cookie)
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "iframe")
            .body(Body::empty())
            .unwrap()
    };
    // Policy denies: 403 before any tunnel, nothing rotated.
    let (status, _) =
        error_of(send(&fixture, deny(), &config, navigation(warm_pair.clone())).await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        rotated(fixture.auth.pool(), warm_token.clone()).await,
        (false, 1)
    );
    // No machine side at all: the tunnel fails, nothing rotated, no orphan row.
    drop(fixture.rx.take());
    let result = send(&fixture, allow(), &config, navigation(warm_pair.clone())).await;
    assert!(
        result
            .as_ref()
            .map(|r| !r.status().is_success())
            .unwrap_or(true),
        "tunnel failure expected: {:?}",
        result.as_ref().map(|r| r.status())
    );
    assert_eq!(
        rotated(fixture.auth.pool(), warm_token.clone()).await,
        (false, 1)
    );
    assert_eq!(
        check(&fixture, &config, AGENT_HOST, Some(&warm_pair)).await,
        StatusCode::NO_CONTENT
    );
}

/// With a healthy upstream the same prewarm row rotates on its next navigation
/// and the rotated cookie is the only Set-Cookie on the response.
#[tokio::test]
async fn prewarm_rotation_succeeds_on_the_next_navigation_with_an_upstream() {
    let mut fixture = agent_fixture().await;
    let config = config(None);
    let (warm_pair, _) =
        establish_embed_session_with_purpose(&fixture, &config, "member", Some("prewarm")).await;
    let (_, _) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Head(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nSet-Cookie: agent=own\r\n",
        ),
    );
    let response = send(
        &fixture,
        allow(),
        &config,
        Request::builder()
            .uri("/")
            .header(header::HOST, AGENT_HOST)
            .header(header::COOKIE, warm_pair.clone())
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookies: Vec<String> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert_eq!(cookies.len(), 1, "{cookies:?}");
    assert!(
        cookies[0].starts_with("__Host-treer_ingress_embed=ies_")
            && cookies[0].ends_with("Max-Age=43200"),
        "{cookies:?}"
    );
    axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    let (old_rotated, count): (bool, i64) = sqlx::query_as(
        "SELECT (SELECT rotated FROM ingress_sessions WHERE token = $1), \
         (SELECT count(*) FROM ingress_sessions WHERE user_id = 'member' AND embed = TRUE)",
    )
    .bind(warm_pair.split_once('=').unwrap().1)
    .fetch_one(&fixture.auth.pool())
    .await
    .unwrap();
    assert!(old_rotated);
    assert_eq!(count, 2);
}

/// #198 (security 5630): a rotation and an exchange for the same (user, Agent)
/// acquire the pair lock in the same order, so running them concurrently at
/// the cap ends in one consistent state instead of a broken deadlock.
#[tokio::test]
async fn concurrent_rotation_and_exchange_take_the_pair_lock_in_one_order() {
    let mut fixture = agent_fixture().await;
    let config = config(None);
    let (warm_pair, _) =
        establish_embed_session_with_purpose(&fixture, &config, "member", Some("prewarm")).await;
    for _ in 0..7 {
        establish_embed_session_with_purpose(&fixture, &config, "member", None).await;
        sqlx::query("UPDATE ingress_browser_auth_codes SET used_at = '2000-01-01T00:00:00Z' WHERE used_at IS NULL")
            .execute(&fixture.auth.pool())
            .await
            .unwrap();
    }
    let (nonce, hash) = nonce_pair();
    let minted = mint(&fixture, allow(), &config, Some("member"), AGENT_ID, &hash)
        .await
        .unwrap();
    let body = axum::body::to_bytes(minted.into_body(), 64 * 1024)
        .await
        .unwrap();
    let code = serde_json::from_slice::<Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, _) = upstream(
        fixture.state.clone(),
        fixture.connection,
        fixture.rx.take().unwrap(),
        Upstream::Head("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n"),
    );
    for _round in 0..3 {
        let navigation = send(
            &fixture,
            allow(),
            &config,
            Request::builder()
                .uri("/")
                .header(header::HOST, AGENT_HOST)
                .header(header::COOKIE, warm_pair.clone())
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "iframe")
                .body(Body::empty())
                .unwrap(),
        );
        let exchange = redeem(&fixture, &config, AGENT_HOST, &code, &nonce);
        let (navigation, exchange) = futures_util::future::join(navigation, exchange).await;
        // Neither side fails with a database error (a broken deadlock would be
        // one); when the exchange evicted the prewarm row first, the navigation
        // simply finds no session, which is the ordinary eviction outcome.
        match navigation {
            Ok(response) => {
                axum::body::to_bytes(response.into_body(), 64)
                    .await
                    .unwrap();
            }
            Err(failure) => assert_ne!(failure.error.code, "database_error", "{failure:?}"),
        }
        match exchange {
            Ok(response) => assert_eq!(response.status(), StatusCode::NO_CONTENT),
            Err(failure) => assert_eq!(
                failure.error.code, "invalid_ingress_authorization",
                "code already consumed"
            ),
        }
    }
    let (live, rotated): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE NOT rotated), count(*) FILTER (WHERE rotated) FROM ingress_sessions \
         WHERE user_id = 'member' AND embed = TRUE AND expires_at > $1",
    )
    .bind(Utc::now().to_rfc3339())
    .fetch_one(&fixture.auth.pool())
    .await
    .unwrap();
    assert_eq!(live, 8, "the cap holds");
    assert!(
        rotated <= 1,
        "at most the one prewarm row rotated: {rotated}"
    );
}
