use super::*;
use axum::{body::Body, routing::post, Router};
use tower::ServiceExt;

async fn user(store: &AuthStore, email: &str) -> CurrentSession {
    let (token, _) = store.create_personal_invitation().await.unwrap();
    store
        .register(Some(&token), email, "User", "password123")
        .await
        .unwrap()
}

/// An owner, their organization "Team" and an organization invitation token.
async fn team_invitation(store: &AuthStore) -> (CurrentSession, String, String) {
    let owner = user(store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    (owner, org.organization_id, token)
}

async fn in_org(store: &AuthStore, user: &CurrentSession, org: &str) -> bool {
    store
        .list_organizations(&user.user_id)
        .await
        .unwrap()
        .iter()
        .any(|o| o.organization_id == org)
}

fn oauth_profile(subject: &str, email: &str) -> OAuthProfile {
    OAuthProfile {
        provider: "github",
        subject: subject.into(),
        email: email.into(),
        preferred_name: "OAuth".into(),
    }
}

fn github_oauth() -> Arc<OAuthConfig> {
    Arc::new(OAuthConfig::new(
        Some(OAuthProviderConfig::github("fixture".into(), "fixture".into()).unwrap()),
        None,
        true,
    ))
}

fn state_of(url: &Url) -> String {
    url.query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned()
}

async fn send(router: &Router, request: axum::http::request::Builder, body: Body) -> Response {
    router
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn pending_invitation_http_requires_human_session_and_explicit_origin_acceptance() {
    let store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    let recipient = user(&store, "recipient@example.test").await;
    let other = user(&store, "other@example.test").await;
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    let router = Router::new()
        .route(
            "/pending",
            axum::routing::get(pending_invitation).delete(discard_pending_invitation),
        )
        .route("/accept", post(accept_invitation))
        .layer(Extension(store.clone()));
    let origin = store.app_public_url.origin().ascii_serialization();
    let call =
        |method: &str, path: &str, session: &CurrentSession, with_origin: bool, body: Value| {
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", format!("{SESSION_COOKIE}={}", session.token))
                .header("content-type", "application/json");
            if with_origin {
                request = request.header("origin", origin.clone());
            }
            let router = router.clone();
            async move { send(&router, request, Body::from(body.to_string())).await }
        };
    assert_eq!(
        call("GET", "/pending", &other, false, json!({}))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    let response = call("GET", "/pending", &recipient, false, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(&token));
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(pending["organization_id"], org);
    let body = json!({"pending_id": pending["pending_id"]});
    for (method, path, session, with_origin, body, status) in [
        (
            "POST",
            "/accept",
            &other,
            true,
            body.clone(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "DELETE",
            "/pending",
            &recipient,
            false,
            json!({}),
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/accept",
            &recipient,
            false,
            body.clone(),
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/accept",
            &recipient,
            true,
            json!({"token": token, "pending_id": pending["pending_id"]}),
            StatusCode::BAD_REQUEST,
        ),
        ("POST", "/accept", &recipient, true, body, StatusCode::OK),
        (
            "GET",
            "/pending",
            &recipient,
            false,
            json!({}),
            StatusCode::NO_CONTENT,
        ),
    ] {
        assert_eq!(
            call(method, path, session, with_origin, body)
                .await
                .status(),
            status,
            "{method} {path}"
        );
    }
    assert!(in_org(&store, &recipient, &org).await);
}

#[tokio::test]
async fn pending_invitation_is_session_bound_one_time_replaceable_and_expiring() {
    let store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    let recipient = user(&store, "recipient@example.test").await;
    let other = user(&store, "other@example.test").await;
    let pending_id = |value: &Value| value["pending_id"].as_str().unwrap().to_string();
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    let first = store.pending_invitation(&recipient).await.unwrap().unwrap();
    assert!(!first.to_string().contains(&token));
    assert!(store.pending_invitation(&other).await.unwrap().is_none());
    assert!(store
        .consume_pending_invitation(&other, &pending_id(&first))
        .await
        .is_err());
    let mut another_session = recipient.clone();
    another_session.token = "different-session-fixture".into();
    assert!(store
        .pending_invitation(&another_session)
        .await
        .unwrap()
        .is_none());
    // Retaining again replaces the previous pending id.
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    let second = store.pending_invitation(&recipient).await.unwrap().unwrap();
    assert!(store
        .consume_pending_invitation(&recipient, &pending_id(&first))
        .await
        .is_err());
    assert_eq!(
        store
            .consume_pending_invitation(&recipient, &pending_id(&second))
            .await
            .unwrap(),
        token
    );
    assert!(store
        .consume_pending_invitation(&recipient, &pending_id(&second))
        .await
        .is_err());
    assert!(!in_org(&store, &recipient, &org).await);
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    sqlx::query("UPDATE pending_organization_invitations SET expires_at=now()-interval '1 minute'")
        .execute(&store.pool)
        .await
        .unwrap();
    assert!(store
        .pending_invitation(&recipient)
        .await
        .unwrap()
        .is_none());
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    store.discard_pending_invitation(&recipient).await.unwrap();
    assert!(store
        .pending_invitation(&recipient)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn oauth_start_requires_json_custom_header_and_get_never_carries_invite() {
    let mut store = AuthStore::for_test("fixture").await;
    store.oauth = github_oauth();
    let router = Router::new()
        .route(
            "/api/auth/oauth/{provider}/start",
            axum::routing::get(oauth_start).post(oauth_start_json),
        )
        .layer(Extension(store.clone()));
    let path = "/api/auth/oauth/github/start";
    let origin = store.app_public_url.origin().ascii_serialization();
    let json_body = r#"{"invite_token":"inv_fixture","join_organization":true}"#;
    let post = |origin: Option<&str>, console: bool, content_type: &str| {
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", content_type);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if console {
            request = request.header("x-treer-console", "1");
        }
        request
    };
    for (request, body, status) in [
        (
            post(None, true, "application/json"),
            json_body,
            StatusCode::FORBIDDEN,
        ),
        (
            post(Some(&origin), false, "application/json"),
            json_body,
            StatusCode::FORBIDDEN,
        ),
        (
            post(Some(&origin), true, "application/x-www-form-urlencoded"),
            "invite_token=inv_fixture&join_organization=true",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            send(&router, request, Body::from(body)).await.status(),
            status
        );
    }
    let response = send(
        &router,
        post(Some(&origin), true, "application/json"),
        Body::from(json_body),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("HttpOnly"));
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["authorization_url"]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/"));
    assert!(!String::from_utf8_lossy(&bytes).contains("inv_fixture"));
    let (invite_hash, join): (String, bool) =
        sqlx::query_as("SELECT invite_hash, join_organization FROM oauth_states")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!((invite_hash, join), (fast_secret_hash("inv_fixture"), true));
    sqlx::query("DELETE FROM oauth_states")
        .execute(&store.pool)
        .await
        .unwrap();
    let response = send(
        &router,
        axum::http::Request::builder()
            .uri(format!("{path}?invite=inv_fixture&join_organization=true")),
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM oauth_states")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "legacy GET must not create OAuth state");
}

#[tokio::test]
async fn registration_consent_consumes_once_and_caps_legacy_role_to_member() {
    let store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    sqlx::query("UPDATE invitations SET role='owner' WHERE token=$1")
        .bind(&token)
        .execute(&store.pool)
        .await
        .unwrap();
    let joined = store
        .register_with_client(
            Some(&token),
            "new@example.test",
            "New",
            "password123",
            None,
            true,
        )
        .await
        .unwrap();
    let orgs = store.list_organizations(&joined.user_id).await.unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(
        (orgs[0].organization_id.as_str(), orgs[0].role.as_str()),
        (org.as_str(), "member")
    );
    assert!(store
        .register_with_client(
            Some(&token),
            "other@example.test",
            "Other",
            "password123",
            None,
            true
        )
        .await
        .is_err());
}

#[tokio::test]
async fn new_oauth_account_joins_only_with_bound_consent_hash_and_state() {
    let mut store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    store.oauth = github_oauth();
    let url = store
        .oauth_authorization_url("github", Some(&token), true, "browser-fixture")
        .await
        .unwrap();
    assert!(!url.as_str().contains(&token));
    let state = state_of(&url);
    let (invite_token, invite_hash, browser_hash): (Option<String>, String, String) =
        sqlx::query_as(
            "SELECT invite_token, invite_hash, browser_hash FROM oauth_states WHERE state=$1",
        )
        .bind(&state)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert!(invite_token.is_none());
    assert_eq!(invite_hash, fast_secret_hash(&token));
    assert_eq!(browser_hash, fast_secret_hash("browser-fixture"));
    assert!(store
        .consume_oauth_state("github", &state, "wrong")
        .await
        .is_err());
    let pending = store
        .consume_oauth_state("github", &state, "browser-fixture")
        .await
        .unwrap();
    assert!(store
        .consume_oauth_state("github", &state, "browser-fixture")
        .await
        .is_err());
    let joined = store
        .complete_oauth_login(
            oauth_profile("new-subject", "new@example.test"),
            pending.invite.as_deref(),
            pending.join_organization,
        )
        .await
        .unwrap();
    let orgs = store.list_organizations(&joined.user_id).await.unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(
        (orgs[0].organization_id.as_str(), orgs[0].role.as_str()),
        (org.as_str(), "member")
    );
    // Expired state is refused.
    let state = state_of(
        &store
            .oauth_authorization_url("github", None, false, "browser-fixture")
            .await
            .unwrap(),
    );
    sqlx::query("UPDATE oauth_states SET expires_at='2000-01-01' WHERE state=$1")
        .bind(&state)
        .execute(&store.pool)
        .await
        .unwrap();
    assert!(store
        .consume_oauth_state("github", &state, "browser-fixture")
        .await
        .is_err());
}

#[tokio::test]
async fn invitation_preview_rate_budget_is_shared_and_atomic() {
    let store = AuthStore::for_test("fixture").await;
    let other_proxy = store.clone();
    for _ in 0..59 {
        store
            .reserve_invitation_attempt("fixture-source")
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        store.reserve_invitation_attempt("fixture-source"),
        other_proxy.reserve_invitation_attempt("fixture-source")
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(store
        .reserve_invitation_attempt("another-source")
        .await
        .is_ok());
    // Missing and empty tokens are indistinguishable.
    assert_eq!(
        store
            .preview_invitation("missing-fixture")
            .await
            .unwrap()
            .status,
        "invalid"
    );
    assert_eq!(
        serde_json::to_string(&store.preview_invitation("").await.unwrap()).unwrap(),
        serde_json::to_string(&store.preview_invitation("missing-fixture").await.unwrap()).unwrap()
    );
    sqlx::query("UPDATE invitation_attempts SET attempts=600 WHERE bucket='global'")
        .execute(&store.pool)
        .await
        .unwrap();
    assert!(other_proxy
        .reserve_invitation_attempt("new-source-after-global-exhaustion")
        .await
        .is_err());
}

#[tokio::test]
async fn organization_registration_without_consent_does_not_consume_or_join() {
    let mut store = AuthStore::for_test("fixture").await;
    let (owner, org, token) = team_invitation(&store).await;
    let preview = store.preview_invitation(&token).await.unwrap();
    assert_eq!(preview.kind.as_deref(), Some("organization"));
    assert_eq!(preview.organization_name.as_deref(), Some("Team"));
    assert_eq!(
        store
            .register_with_client(
                Some(&token),
                "blocked@example.test",
                "Blocked",
                "password123",
                None,
                false
            )
            .await
            .unwrap_err()
            .code(),
        "invalid_invitation"
    );
    assert_eq!(
        store.preview_invitation(&token).await.unwrap().status,
        "valid"
    );
    // With open registration the account is created but does not join.
    store.oauth = Arc::new(OAuthConfig::new(None, None, false));
    let newcomer = store
        .register_with_client(
            Some(&token),
            "new@example.test",
            "New",
            "password123",
            None,
            false,
        )
        .await
        .unwrap();
    assert!(!in_org(&store, &newcomer, &org).await);
    assert!(store
        .register(Some(&token), "other@example.test", "Other", "password123")
        .await
        .is_ok());
    assert!(
        store
            .accept_invitation(&token, &owner.user_id)
            .await
            .unwrap()
            .already_member
    );
    let accepted = store
        .accept_invitation(&token, &newcomer.user_id)
        .await
        .unwrap();
    assert_eq!(
        (accepted.organization_id.as_str(), accepted.role.as_str()),
        (org.as_str(), "member")
    );
    assert!(!accepted.already_member);
    assert!(
        store
            .accept_invitation(&token, &newcomer.user_id)
            .await
            .unwrap()
            .already_member
    );
    assert_eq!(
        store.preview_invitation(&token).await.unwrap().status,
        "invalid"
    );
    let events = store
        .list_audit_events(&org, &owner.user_id, None, None, 100)
        .await
        .unwrap();
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(serialized.contains("invitation.accepted"));
    assert!(!serialized.contains(&token));
}

#[tokio::test]
async fn oauth_admission_requires_explicit_accept_and_remains_single_use() {
    let store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    let profile = || oauth_profile("fixture-subject", "oauth@example.test");
    assert_eq!(
        store
            .complete_oauth_login(profile(), Some(&token), false)
            .await
            .unwrap_err()
            .code(),
        "invalid_invitation"
    );
    let (personal, _) = store.create_personal_invitation().await.unwrap();
    let newcomer = store
        .complete_oauth_login(profile(), Some(&personal), false)
        .await
        .unwrap();
    store
        .complete_oauth_login(profile(), Some(&token), true)
        .await
        .unwrap();
    assert!(!in_org(&store, &newcomer, &org).await);
    store
        .accept_invitation(&token, &newcomer.user_id)
        .await
        .unwrap();
    assert!(in_org(&store, &newcomer, &org).await);
}

#[tokio::test]
async fn invitation_accept_is_atomic_and_never_changes_existing_roles() {
    let store = AuthStore::for_test("fixture").await;
    let (owner, org, token) = team_invitation(&store).await;
    let first = user(&store, "first@example.test").await;
    let second = user(&store, "second@example.test").await;
    sqlx::query("UPDATE invitations SET role='owner' WHERE token=$1")
        .bind(&token)
        .execute(&store.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        store.accept_invitation(&token, &first.user_id),
        store.accept_invitation(&token, &second.user_id)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(a.as_ref().ok().or(b.as_ref().ok()).unwrap().role, "member");
    let (owner_invite, _) = store.create_invitation(&org, &owner.user_id).await.unwrap();
    let accepted = store
        .accept_invitation(&owner_invite, &owner.user_id)
        .await
        .unwrap();
    assert!(accepted.already_member);
    assert_eq!(accepted.role, "owner");
    assert_eq!(
        store
            .preview_invitation(&owner_invite)
            .await
            .unwrap()
            .status,
        "valid",
        "existing members must not consume the invite"
    );
    let (personal, _) = store.create_personal_invitation().await.unwrap();
    assert!(store
        .accept_invitation(&personal, &first.user_id)
        .await
        .is_err());
}

#[tokio::test]
async fn invitation_http_requires_cookie_origin_and_rejects_delegated_tokens() {
    let store = AuthStore::for_test("fixture").await;
    let (_owner, org, token) = team_invitation(&store).await;
    let invited = user(&store, "invited@example.test").await;
    let app = Router::new()
        .route("/preview", post(preview_invitation))
        .route("/accept", post(accept_invitation))
        .layer(Extension(store.clone()));
    let body = || Body::from(json!({"token": token}).to_string());
    let request = |uri: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
    };
    for (cookie, origin, bearer, fetch_site) in [
        (false, Some("https://app.treer.example"), false, "same-site"),
        (true, None, false, "same-site"),
        (true, Some("https://evil.example"), false, "same-site"),
        (true, Some("https://app.treer.example"), true, "same-site"),
        (true, Some("https://app.treer.example"), false, "cross-site"),
    ] {
        let mut request = request("/accept").header("sec-fetch-site", fetch_site);
        if cookie {
            request = request.header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", invited.token),
            );
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if bearer {
            request = request.header(header::AUTHORIZATION, "Bearer fixture-service-token");
        }
        assert!(send(&app, request, body()).await.status().is_client_error());
    }
    assert!(!in_org(&store, &invited, &org).await);
    let response = send(&app, request("/preview"), body()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = send(
        &app,
        request("/accept")
            .header(header::ORIGIN, "https://app.treer.example")
            .header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", invited.token),
            ),
        body(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(in_org(&store, &invited, &org).await);
}
