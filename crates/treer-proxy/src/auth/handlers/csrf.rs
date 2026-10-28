use super::*;

/// Browser authority is tied to an exact configured origin, not SameSite or
/// CORS. Keep a single check for invitation, authentication and session writes.
pub(crate) fn require_console_origin(
    auth: &AuthStore,
    headers: &HeaderMap,
) -> Result<(), AuthFailure> {
    validate_browser_origin(
        headers,
        &[&auth.app_public_url.origin().ascii_serialization()],
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CookieOriginClass {
    Console,
    Embedded,
}

// Classify Axum's registered template, never a client-supplied path prefix.
// Missing route metadata defaults to the narrower Console-only authority.
pub(crate) fn cookie_origin_class(template: &str) -> CookieOriginClass {
    match template {
        "/api/workspaces/{workspace_id}/apps/{app_id}/ui/"
        | "/api/workspaces/{workspace_id}/apps/{app_id}/ui/{*path}"
        | "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui"
        | "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/"
        | "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/{*path}"
        | "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy"
        | "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/"
        | "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/{*path}" => {
            CookieOriginClass::Embedded
        }
        _ => CookieOriginClass::Console,
    }
}

pub(crate) fn require_cookie_request_origin(
    auth: &AuthStore,
    request: &Request,
) -> Result<(), AuthFailure> {
    let class = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|path| cookie_origin_class(path.as_str()))
        .unwrap_or(CookieOriginClass::Console);
    let console = auth.app_public_url.origin().ascii_serialization();
    let proxy = auth.proxy_public_url.origin().ascii_serialization();
    match class {
        CookieOriginClass::Console => validate_browser_origin(request.headers(), &[&console]),
        CookieOriginClass::Embedded => {
            validate_browser_origin(request.headers(), &[&console, &proxy])
        }
    }
}

fn validate_browser_origin(headers: &HeaderMap, expected: &[&str]) -> Result<(), AuthFailure> {
    if headers.get_all(header::ORIGIN).iter().count() != 1
        || !headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| expected.contains(&origin))
        || headers.get_all("sec-fetch-site").iter().count() > 1
        || headers
            .get("sec-fetch-site")
            .is_some_and(|v| v != "same-origin" && v != "same-site")
    {
        return Err(AuthFailure::forbidden(
            "csrf_denied",
            "trusted browser origin required",
        ));
    }
    Ok(())
}

pub(crate) fn browser_request_needs_origin(request: &Request) -> bool {
    !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    ) || request.headers().contains_key(header::UPGRADE)
        || request.uri().path() == "/api/admin/update/check"
}

// These endpoints select credentials from their body, never Authorization.
// A mobile protocol marker cannot exempt any authenticated cookie operation.
const PREAUTH_WRITES: &[&str] = &[
    "/api/auth/login",
    "/api/auth/register",
    "/api/auth/request-password-reset",
    "/api/auth/reset-password",
    "/api/admin/login",
    "/api/invitations/preview",
    "/api/invitations/accept",
    "/api/invitations/pending",
];

fn native_preauth(headers: &HeaderMap) -> bool {
    headers.get_all(NATIVE_CLIENT_HEADER).iter().count() == 1
        && native_client_kind(headers).is_some()
        && !headers.contains_key(header::ORIGIN)
        && !headers
            .keys()
            .any(|name| name.as_str().starts_with("sec-fetch-"))
        && cookie_value(headers, SESSION_COOKIE).is_none()
        && cookie_value(headers, ADMIN_SESSION_COOKIE).is_none()
}

pub(crate) async fn require_preauth_origin(
    State(auth): State<AuthStore>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if browser_request_needs_origin(&request)
        && PREAUTH_WRITES.contains(&path)
        && !(path.starts_with("/api/auth/") && native_preauth(request.headers()))
    {
        if let Err(error) = require_console_origin(&auth, request.headers()) {
            return error.into_response();
        }
    }
    next.run(request).await
}

/// OAuth/ingress authorization is a documented top-level navigation exception.
/// It must not be triggered as an image, XHR, or other browser subresource.
pub(crate) fn require_authorization_navigation(headers: &HeaderMap) -> Result<(), AuthFailure> {
    for (name, expected) in [
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-dest", "document"),
    ] {
        if headers.get_all(name).iter().count() > 1
            || headers.get(name).is_some_and(|value| value != expected)
        {
            return Err(AuthFailure::forbidden(
                "csrf_denied",
                "browser navigation required",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, middleware, routing::any, Router};
    use tower::ServiceExt;

    async fn status(app: &Router, request: axum::http::request::Builder) -> StatusCode {
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    async fn user_session(auth: &AuthStore) -> CurrentSession {
        let (invite, _) = auth.create_personal_invitation().await.unwrap();
        auth.register(Some(&invite), "csrf@example.test", "CSRF", "password123")
            .await
            .unwrap()
    }

    #[test]
    fn native_channel_is_not_a_cookie_or_browser_bypass() {
        let mut headers = HeaderMap::new();
        headers.insert(NATIVE_CLIENT_HEADER, HeaderValue::from_static("mobile_ios"));
        assert!(native_preauth(&headers));
        for (name, value) in [
            ("origin", "https://app.treer.example"),
            ("sec-fetch-mode", "cors"),
            ("cookie", "treer_session=test-session"),
            ("cookie", "treer_admin_session=test-admin"),
        ] {
            let mut candidate = headers.clone();
            candidate.insert(
                axum::http::HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
            assert!(!native_preauth(&candidate), "{name}");
        }
        headers.append(NATIVE_CLIENT_HEADER, HeaderValue::from_static("mobile_ios"));
        assert!(!native_preauth(&headers));
    }

    #[tokio::test]
    async fn proxy_origin_is_authorized_only_for_registered_embedded_routes() {
        let auth = AuthStore::for_test("admin-password").await;
        let user = user_session(&auth).await;
        let mut app = Router::new();
        for template in [
            "/api/workspaces/{workspace_id}/apps/{app_id}/ui/{*path}",
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui",
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/{*path}",
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/{*path}",
            "/api/workspaces/{workspace_id}/machines/{machine_id}/exec",
        ] {
            app = app.route(template, any(|| async { StatusCode::NO_CONTENT }));
        }
        let app = app.layer(middleware::from_fn_with_state(auth, require_user));
        let cookie = format!("{SESSION_COOKIE}={}", user.token);
        for path in [
            "/api/workspaces/ws/apps/app/ui/api/write",
            "/api/workspaces/ws/agents/agent/interface/ui",
            "/api/workspaces/ws/agents/agent/interface/ui/api/prompt",
            "/api/workspaces/ws/virtual-hosts/demo/proxy/api/write",
            "/api/workspaces/ws/machines/machine/exec",
        ] {
            for origin in [
                "https://app.treer.example",
                "https://proxy.treer.example",
                "https://evil.example",
            ] {
                for upgrade in [false, true] {
                    let mut request = Request::builder()
                        .method(if upgrade { "GET" } else { "POST" })
                        .uri(path)
                        .header(header::COOKIE, &cookie)
                        .header(header::ORIGIN, origin);
                    if upgrade {
                        request = request.header(header::UPGRADE, "websocket");
                    }
                    let allowed = origin == "https://app.treer.example"
                        || (origin == "https://proxy.treer.example" && !path.ends_with("/exec"));
                    assert_eq!(
                        status(&app, request).await,
                        if allowed {
                            StatusCode::NO_CONTENT
                        } else {
                            StatusCode::FORBIDDEN
                        },
                        "{path} {origin} upgrade={upgrade}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn preauth_guard_has_a_narrow_explicit_native_channel() {
        let auth = AuthStore::for_test("admin-password").await;
        let app = Router::new()
            .route("/{*path}", any(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn_with_state(auth, require_preauth_origin));
        for path in PREAUTH_WRITES {
            let native = || {
                Request::builder()
                    .method("POST")
                    .uri(*path)
                    .header(NATIVE_CLIENT_HEADER, "mobile_ios")
            };
            assert_eq!(
                status(&app, native()).await,
                if path.starts_with("/api/auth/") {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            for (name, value) in [
                ("origin", "https://evil.example"),
                ("sec-fetch-mode", "cors"),
                ("cookie", "treer_session=synthetic"),
            ] {
                assert_eq!(
                    status(&app, native().header(name, value)).await,
                    StatusCode::FORBIDDEN,
                    "{path} {name}"
                );
            }
            let console = Request::builder()
                .method("POST")
                .uri(*path)
                .header(header::ORIGIN, "https://app.treer.example");
            assert_eq!(status(&app, console).await, StatusCode::NO_CONTENT);
        }
    }

    #[tokio::test]
    async fn cookie_guard_uses_selected_authentication_and_prevents_side_effects() {
        let auth = AuthStore::for_test("admin-password").await;
        let user = user_session(&auth).await;
        let admin = auth.admin_login("admin-password").await.unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for is_admin in [false, true] {
            let handler_count = count.clone();
            let app = Router::new().route(
                "/{*path}",
                any(move || {
                    let count = handler_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
            let (app, cookie) = if is_admin {
                (
                    app.layer(middleware::from_fn_with_state(auth.clone(), require_admin)),
                    format!("{ADMIN_SESSION_COOKIE}={}", admin.token),
                )
            } else {
                (
                    app.layer(middleware::from_fn_with_state(auth.clone(), require_user)),
                    format!("{SESSION_COOKIE}={}", user.token),
                )
            };
            let mutation = |method: &str| {
                Request::builder()
                    .method(method)
                    .uri("/mutation")
                    .header(header::COOKIE, &cookie)
            };
            for method in ["POST", "PUT", "PATCH", "DELETE"] {
                for authorization in [None, Some("Basic irrelevant"), Some("Bearer ")] {
                    let before = count.load(Ordering::SeqCst);
                    let mut request = mutation(method);
                    if let Some(value) = authorization {
                        request = request.header(header::AUTHORIZATION, value);
                    }
                    assert_eq!(status(&app, request).await, StatusCode::FORBIDDEN);
                    assert_eq!(count.load(Ordering::SeqCst), before);
                }
                let request = mutation(method).header(header::ORIGIN, "https://app.treer.example");
                assert_eq!(status(&app, request).await, StatusCode::NO_CONTENT);
            }
            let socket = Request::builder()
                .uri("/socket")
                .header(header::UPGRADE, "websocket")
                .header(header::COOKIE, &cookie);
            assert_eq!(status(&app, socket).await, StatusCode::FORBIDDEN);
            // A valid user Bearer is not CSRF-prone; it never unlocks admin routes.
            let request =
                mutation("POST").header(header::AUTHORIZATION, format!("Bearer {}", user.token));
            assert_eq!(
                status(&app, request).await,
                if is_admin {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::NO_CONTENT
                }
            );
            if !is_admin {
                let request = mutation("POST").header(header::AUTHORIZATION, "Bearer invalid");
                assert_eq!(
                    status(&app, request).await,
                    StatusCode::UNAUTHORIZED,
                    "invalid Bearer must not fall back to a valid cookie"
                );
            }
        }
    }

    #[test]
    fn origin_is_exact_single_and_fetch_metadata_cannot_contradict_it() {
        const EXPECTED: &str = "https://console.example.test";
        for origin in [
            None,
            Some("null"),
            Some("https://evil.example.test"),
            Some("https://console.example.test/"),
            Some("https://console.example.test.evil.test"),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(origin) = origin {
                headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
            }
            assert!(validate_browser_origin(&headers, &[EXPECTED]).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static(EXPECTED));
        assert!(validate_browser_origin(&headers, &[EXPECTED]).is_ok());
        for (site, ok) in [
            ("same-origin", true),
            ("same-site", true),
            ("cross-site", false),
            ("none", false),
            ("invalid", false),
        ] {
            headers.insert("sec-fetch-site", HeaderValue::from_static(site));
            assert_eq!(
                validate_browser_origin(&headers, &[EXPECTED]).is_ok(),
                ok,
                "{site}"
            );
        }
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        headers.append("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert!(validate_browser_origin(&headers, &[EXPECTED]).is_err());
        headers.remove("sec-fetch-site");
        headers.append(header::ORIGIN, HeaderValue::from_static(EXPECTED));
        assert!(validate_browser_origin(&headers, &[EXPECTED]).is_err());
    }
}
