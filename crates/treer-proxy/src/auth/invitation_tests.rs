use super::*;
use axum::{body::Body, routing::post, Router};
use tower::ServiceExt;

#[tokio::test]
async fn pending_invitation_http_requires_human_session_and_explicit_origin_acceptance() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let recipient = user(&store, "recipient@example.test").await;
    let other = user(&store, "other@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
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
    let request =
        |method: &str, path: &str, session: &CurrentSession, origin: bool, body: Value| {
            let mut request = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", format!("{SESSION_COOKIE}={}", session.token))
                .header("content-type", "application/json");
            if origin {
                request = request.header(
                    "origin",
                    store.app_public_url.origin().ascii_serialization(),
                );
            }
            request.body(Body::from(body.to_string())).unwrap()
        };
    let response = router
        .clone()
        .oneshot(request("GET", "/pending", &other, false, json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = router
        .clone()
        .oneshot(request("GET", "/pending", &recipient, false, json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(&token));
    assert_eq!(pending["organization_id"], org.organization_id);
    let body = json!({"pending_id":pending["pending_id"]});
    assert_eq!(
        router
            .clone()
            .oneshot(request("POST", "/accept", &other, true, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("DELETE", "/pending", &recipient, false, json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("POST", "/accept", &recipient, false, body.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                "POST",
                "/accept",
                &recipient,
                true,
                json!({"token":token,"pending_id":pending["pending_id"]})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("POST", "/accept", &recipient, true, body))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        store
            .list_organizations(&recipient.user_id)
            .await
            .unwrap()
            .iter()
            .find(|o| o.organization_id == org.organization_id)
            .unwrap()
            .role,
        "member"
    );
    assert_eq!(
        router
            .oneshot(request("GET", "/pending", &recipient, false, json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn pending_invitation_is_session_bound_one_time_replaceable_and_expiring() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let recipient = user(&store, "recipient@example.test").await;
    let other = user(&store, "other@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    let first = store.pending_invitation(&recipient).await.unwrap().unwrap();
    assert!(!first.to_string().contains(&token));
    assert!(store.pending_invitation(&other).await.unwrap().is_none());
    assert!(store
        .consume_pending_invitation(&other, first["pending_id"].as_str().unwrap())
        .await
        .is_err());
    let mut another_session = recipient.clone();
    another_session.token = "different-session-fixture".into();
    assert!(store
        .pending_invitation(&another_session)
        .await
        .unwrap()
        .is_none());
    store
        .retain_pending_invitation(&recipient, &token)
        .await
        .unwrap();
    let second = store.pending_invitation(&recipient).await.unwrap().unwrap();
    assert_ne!(first["pending_id"], second["pending_id"]);
    assert!(store
        .consume_pending_invitation(&recipient, first["pending_id"].as_str().unwrap())
        .await
        .is_err());
    let resolved = store
        .consume_pending_invitation(&recipient, second["pending_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(resolved, token);
    assert!(store
        .consume_pending_invitation(&recipient, second["pending_id"].as_str().unwrap())
        .await
        .is_err());
    assert!(store
        .list_organizations(&recipient.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != org.organization_id));
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
    store.oauth = Arc::new(OAuthConfig::new(
        Some(OAuthProviderConfig::github("fixture".into(), "fixture".into()).unwrap()),
        None,
        true,
    ));
    let router = Router::new()
        .route(
            "/api/auth/oauth/{provider}/start",
            axum::routing::get(oauth_start).post(oauth_start_json),
        )
        .layer(Extension(store.clone()));
    let path = "/api/auth/oauth/github/start";
    let body = r#"{"invite_token":"inv_fixture","join_organization":true}"#;
    let cross = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    let missing_header = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    "origin",
                    store.app_public_url.origin().ascii_serialization(),
                )
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_header.status(), StatusCode::FORBIDDEN);
    let form = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    "origin",
                    store.app_public_url.origin().ascii_serialization(),
                )
                .header("x-treer-console", "1")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "invite_token=inv_fixture&join_organization=true",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(form.status(), StatusCode::BAD_REQUEST);
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header(
                    "origin",
                    store.app_public_url.origin().ascii_serialization(),
                )
                .header("content-type", "application/json")
                .header("x-treer-console", "1")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
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
    let row = sqlx::query("SELECT invite_hash,join_organization FROM oauth_states")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        row.get::<String, _>("invite_hash"),
        fast_secret_hash("inv_fixture")
    );
    assert!(row.get::<bool, _>("join_organization"));
    sqlx::query("DELETE FROM oauth_states")
        .execute(&store.pool)
        .await
        .unwrap();
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("{path}?invite=inv_fixture&join_organization=true"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
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
    let owner = user(&store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
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
    assert_eq!(orgs[0].organization_id, org.organization_id);
    assert_eq!(orgs[0].role, "member");
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
    let owner = user(&store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    store.oauth = Arc::new(OAuthConfig::new(
        Some(OAuthProviderConfig::github("fixture".into(), "fixture".into()).unwrap()),
        None,
        true,
    ));
    let url = store
        .oauth_authorization_url("github", Some(&token), true, "browser-fixture")
        .await
        .unwrap();
    assert!(!url.as_str().contains(&token));
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let row = sqlx::query("SELECT invite_token,invite_hash,browser_hash,join_organization FROM oauth_states WHERE state=$1").bind(&state).fetch_one(&store.pool).await.unwrap();
    assert!(row.get::<Option<String>, _>("invite_token").is_none());
    assert_eq!(
        row.get::<String, _>("invite_hash"),
        fast_secret_hash(&token)
    );
    assert_eq!(
        row.get::<String, _>("browser_hash"),
        fast_secret_hash("browser-fixture")
    );
    assert!(row.get::<bool, _>("join_organization"));
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
            OAuthProfile {
                provider: "github",
                subject: "new-subject".into(),
                email: "new@example.test".into(),
                preferred_name: "New".into(),
            },
            pending.invite.as_deref(),
            pending.join_organization,
        )
        .await
        .unwrap();
    let orgs = store.list_organizations(&joined.user_id).await.unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0].role, "member");
    assert_eq!(orgs[0].organization_id, org.organization_id);
    let url = store
        .oauth_authorization_url("github", None, false, "browser-fixture")
        .await
        .unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
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
    assert_eq!(
        store
            .preview_invitation("missing-fixture")
            .await
            .unwrap()
            .status,
        "invalid"
    );
    let preview = serde_json::to_string(&store.preview_invitation("").await.unwrap()).unwrap();
    assert_eq!(
        preview,
        serde_json::to_string(&store.preview_invitation("missing-fixture").await.unwrap()).unwrap()
    );
    sqlx::query("UPDATE invitation_attempts SET attempts=600 WHERE bucket='global'")
        .execute(&store.pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invitation_attempts")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert!(other_proxy
        .reserve_invitation_attempt("new-source-after-global-exhaustion")
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM invitation_attempts")
            .fetch_one(&store.pool)
            .await
            .unwrap(),
        count,
        "global budget must reject before allocating another peer bucket"
    );
}

async fn user(store: &AuthStore, email: &str) -> CurrentSession {
    let (token, _) = store.create_personal_invitation().await.unwrap();
    store
        .register(Some(&token), email, "User", "password123")
        .await
        .unwrap()
}

#[tokio::test]
async fn organization_registration_without_consent_does_not_consume_or_join() {
    let mut store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    for _ in 0..2 {
        let preview = store.preview_invitation(&token).await.unwrap();
        assert_eq!(preview.kind.as_deref(), Some("organization"));
        assert_eq!(preview.organization_name.as_deref(), Some("Team"));
    }
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
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM users WHERE email='blocked@example.test'"
        )
        .fetch_one(&store.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        store.preview_invitation(&token).await.unwrap().status,
        "valid"
    );
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
    let organizations = store.list_organizations(&newcomer.user_id).await.unwrap();
    assert_eq!(organizations.len(), 1);
    assert_ne!(organizations[0].organization_id, org.organization_id);
    assert!(store.preview_invitation(&token).await.is_ok());
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
    assert_eq!(accepted.organization_id, org.organization_id);
    assert_eq!(accepted.role, "member");
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
        .list_audit_events(&org.organization_id, &owner.user_id, None, None, 100)
        .await
        .unwrap();
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(serialized.contains("invitation.accepted"));
    assert!(!serialized.contains(&token));
}

#[tokio::test]
async fn oauth_admission_requires_explicit_accept_and_remains_single_use() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    let profile = || OAuthProfile {
        provider: "github",
        subject: "fixture-subject".into(),
        email: "oauth@example.test".into(),
        preferred_name: "OAuth".into(),
    };
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
    assert!(store
        .list_organizations(&newcomer.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != org.organization_id));
    store
        .complete_oauth_login(profile(), Some(&token), true)
        .await
        .unwrap();
    assert!(store
        .list_organizations(&newcomer.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != org.organization_id));
    store
        .accept_invitation(&token, &newcomer.user_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_organizations(&newcomer.user_id)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn pending_storage_failure_never_invalidates_a_completed_oauth_login() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (invite, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    let (personal, _) = store.create_personal_invitation().await.unwrap();
    let session = store
        .complete_oauth_login(
            OAuthProfile {
                provider: "github",
                subject: "fixture".into(),
                email: "oauth@example.test".into(),
                preferred_name: "OAuth".into(),
            },
            Some(&personal),
            false,
        )
        .await
        .unwrap();
    sqlx::query("DROP TABLE pending_organization_invitations")
        .execute(&store.pool)
        .await
        .unwrap();
    store
        .retain_oauth_invitation_best_effort(&session, Some(&invite))
        .await;
    assert_eq!(
        store
            .session(&session.token)
            .await
            .unwrap()
            .unwrap()
            .user_id,
        session.user_id
    );
    let response = oauth_session_redirect(&store, &session);
    assert!(response.status().is_redirection());
    assert!(response.headers().contains_key(header::SET_COOKIE));
    assert!(store
        .list_organizations(&session.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != org.organization_id));
}

#[tokio::test]
async fn invitation_accept_is_atomic_and_never_changes_existing_roles() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let first = user(&store, "first@example.test").await;
    let second = user(&store, "second@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
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
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM organization_members WHERE organization_id=$1")
            .bind(&org.organization_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
    let (owner_invite, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
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
    assert_eq!(
        store
            .preview_invitation(&personal)
            .await
            .unwrap()
            .kind
            .as_deref(),
        Some("personal")
    );
    sqlx::query("DELETE FROM invitations WHERE token=$1")
        .bind(&personal)
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        store.preview_invitation(&personal).await.unwrap().status,
        "invalid"
    );
}

#[tokio::test]
async fn invitation_http_requires_cookie_origin_and_rejects_delegated_tokens() {
    let store = AuthStore::for_test("fixture").await;
    let owner = user(&store, "owner@example.test").await;
    let invited = user(&store, "invited@example.test").await;
    let org = store
        .create_organization(&owner.user_id, "Team")
        .await
        .unwrap();
    let (token, _) = store
        .create_invitation(&org.organization_id, &owner.user_id)
        .await
        .unwrap();
    let app = Router::new()
        .route("/preview", post(preview_invitation))
        .route("/accept", post(accept_invitation))
        .layer(Extension(store.clone()));
    for (cookie, origin, bearer, fetch_site) in [
        (false, Some("https://app.treer.example"), false, "same-site"),
        (true, None, false, "same-site"),
        (true, Some("https://evil.example"), false, "same-site"),
        (true, Some("https://app.treer.example"), true, "same-site"),
        (true, Some("https://app.treer.example"), false, "cross-site"),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/accept")
            .header(header::CONTENT_TYPE, "application/json")
            .header("sec-fetch-site", fetch_site);
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
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(json!({"token":token}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }
    assert!(store
        .list_organizations(&invited.user_id)
        .await
        .unwrap()
        .iter()
        .all(|o| o.organization_id != org.organization_id));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preview")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"token":token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/accept")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(
                    header::COOKIE,
                    format!("{SESSION_COOKIE}={}", invited.token),
                )
                .body(Body::from(json!({"token":token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}
