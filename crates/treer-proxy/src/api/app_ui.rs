use super::*;

pub(super) async fn proxy_app_ui_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(identity): Extension<IdentityIssuer>,
    Path((workspace_id, app_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    proxy_app_ui(
        state,
        auth,
        browser,
        identity,
        workspace_id,
        app_id,
        String::new(),
        request,
    )
    .await
}

pub(super) async fn proxy_app_ui_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(identity): Extension<IdentityIssuer>,
    Path((workspace_id, app_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    proxy_app_ui(
        state,
        auth,
        browser,
        identity,
        workspace_id,
        app_id,
        path,
        request,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn proxy_app_ui(
    state: AppState,
    auth: AuthStore,
    browser: BrowserAccess,
    identity: IdentityIssuer,
    workspace_id: String,
    app_id: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    let navigation = path.is_empty() && is_iframe_html_navigation(&request);
    let app = auth.resolve_app_deployment(&workspace_id, &app_id).await?;
    if !auth.authentication_disabled() {
        let session = request
            .extensions()
            .get::<CurrentSession>()
            .ok_or_else(|| {
                ApiFailure::unauthorized("authentication_required", "human session required")
            })?;
        let token = issue_browser_app_identity(
            &auth,
            &identity,
            &workspace_id,
            &session.user_id,
            &app.service_id,
        )
        .await?;
        request.extensions_mut().insert(ForwardedAppIdentity(token));
    }
    let mut response = match proxy_virtual_network_host(
        state,
        auth,
        workspace_id.clone(),
        app.hostname,
        path,
        request,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            return app_navigation_failure(error, navigation, &browser, &workspace_id, &app_id)
        }
    };
    allow_control_plane_frame(response.headers_mut(), &browser)?;
    Ok(response)
}

fn is_iframe_html_navigation(request: &Request<Body>) -> bool {
    request.method() == Method::GET
        && request
            .headers()
            .get("sec-fetch-dest")
            .is_some_and(|v| v == "iframe")
        && request
            .headers()
            .get("sec-fetch-mode")
            .is_some_and(|v| v == "navigate")
        && request
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',').any(|part| {
                    part.split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .eq_ignore_ascii_case("text/html")
                })
            })
}

// Only Proxy-generated transient transport failures get a navigation bridge.
// Upstream App responses, auth failures, HEAD probes and API calls are untouched.
fn app_navigation_failure(
    error: ApiFailure,
    navigation: bool,
    browser: &BrowserAccess,
    workspace: &str,
    app: &str,
) -> Result<Response, ApiFailure> {
    if !navigation
        || !matches!(
            error.status,
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
        )
        || !matches!(
            error.error.code.as_str(),
            "stream_failed"
                | "server_offline"
                | "no_online_server"
                | "network_open_timeout"
                | "network_stream_closed"
                | "stream_closed"
                | "tunnel_handshake_failed"
                | "tunnel_request_failed"
        )
    {
        return Err(error);
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    // Escape HTML-significant bytes even though the values are JSON strings.
    let payload =
        serde_json::json!({"type":"treer.app-ui.unavailable","workspaceId":workspace,"appId":app})
            .to_string()
            .replace('<', "\\u003c")
            .replace('&', "\\u0026");
    let origin = serde_json::json!(&*browser.origin_text)
        .to_string()
        .replace('<', "\\u003c")
        .replace('&', "\\u0026");
    let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>App unavailable</title></head><body><p>App connection unavailable.</p><script nonce=\"{nonce}\">parent.postMessage({payload},{origin});</script></body></html>");
    let mut response = (error.status, axum::response::Html(html)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; base-uri 'none'; form-action 'none'"
        ))
        .map_err(|_| {
            ApiFailure::internal("app_error_bridge_failed", "invalid error page policy")
        })?,
    );
    allow_control_plane_frame(response.headers_mut(), browser)?;
    Ok(response)
}

pub(super) fn allow_control_plane_frame(
    headers: &mut HeaderMap,
    browser: &BrowserAccess,
) -> Result<(), ApiFailure> {
    // Only explicit, authenticated App/Agent UI tunnels override embedding
    // policy. Keep all other CSP directives and each separately enforced policy.
    let ancestors = format!("frame-ancestors 'self' {}", browser.origin_text);
    let mut policies = Vec::new();
    for value in headers.get_all(header::CONTENT_SECURITY_POLICY) {
        let policy = value
            .to_str()
            .map_err(|error| ApiFailure::bad_gateway("invalid_app_csp", &error.to_string()))?;
        let mut directives: Vec<&str> = policy
            .split(';')
            .map(str::trim)
            .filter(|directive| {
                !directive.is_empty()
                    && !directive
                        .split_whitespace()
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors"))
            })
            .collect();
        directives.push(&ancestors);
        policies.push(directives.join("; "));
    }
    if policies.is_empty() {
        policies.push(ancestors);
    }
    headers.remove(header::CONTENT_SECURITY_POLICY);
    for policy in policies {
        headers.append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&policy)
                .map_err(|error| ApiFailure::bad_gateway("invalid_app_csp", &error.to_string()))?,
        );
    }
    headers.remove(header::X_FRAME_OPTIONS);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser() -> BrowserAccess {
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap()
    }

    // Export the production renderer's actual response for the isolated Web
    // delayed-listener fixture. No test-only endpoint or production flag.
    #[tokio::test]
    #[ignore = "explicit browser fixture export; requires Console origin"]
    async fn export_navigation_error_browser_fixture() {
        let origin = std::env::var("TREER_APP_UI_FIXTURE_CONSOLE_ORIGIN")
            .expect("set the isolated fixture Console origin");
        let browser = BrowserAccess::new(
            &Url::parse(&origin).unwrap(),
            &Url::parse("http://127.0.0.1/").unwrap(),
        )
        .unwrap();
        let response = app_navigation_failure(
            ApiFailure::bad_gateway("stream_failed", "isolated listener unavailable"),
            true,
            &browser,
            "workspace",
            "app",
        )
        .unwrap();
        let status = response.status().as_u16();
        let headers: std::collections::BTreeMap<_, _> = response
            .headers()
            .iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect();
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        println!(
            "TREER_APP_UI_ERROR_FIXTURE {}",
            serde_json::json!({
                "status":status,"headers":headers,"body":std::str::from_utf8(&body).unwrap(),
                "console_origin":&*browser.origin_text,
            })
        );
    }

    #[test]
    fn error_bridge_requires_an_explicit_get_iframe_html_navigation() {
        let request = |method: Method, dest: &str, mode: &str, accept: &str| {
            Request::builder()
                .method(method)
                .header("sec-fetch-dest", dest)
                .header("sec-fetch-mode", mode)
                .header(header::ACCEPT, accept)
                .body(Body::empty())
                .unwrap()
        };
        assert!(is_iframe_html_navigation(&request(
            Method::GET,
            "iframe",
            "navigate",
            "text/html,application/xhtml+xml"
        )));
        for (method, dest, mode, accept) in [
            (Method::HEAD, "iframe", "navigate", "text/html"),
            (Method::POST, "iframe", "navigate", "text/html"),
            (Method::GET, "document", "navigate", "text/html"),
            (Method::GET, "iframe", "cors", "text/html"),
            (Method::GET, "iframe", "navigate", "application/json"),
        ] {
            assert!(!is_iframe_html_navigation(&request(
                method, dest, mode, accept
            )));
        }
    }

    #[tokio::test]
    async fn error_bridge_preserves_failure_and_scopes_signal_without_error_details() {
        let response = app_navigation_failure(
            ApiFailure::bad_gateway("stream_failed", "private upstream diagnostic"),
            true,
            &browser(),
            "ws_test",
            "app_</script><script>bad()",
        )
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        let policy = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(policy.contains("default-src 'none'; script-src 'nonce-"));
        assert!(policy.contains("frame-ancestors 'self' https://app.treer.example"));
        assert!(!policy.contains("unsafe-inline"));
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("parent.postMessage("));
        assert!(body.contains("\"type\":\"treer.app-ui.unavailable\""));
        assert!(body.contains("\"workspaceId\":\"ws_test\""));
        assert!(body.contains(",\"https://app.treer.example\");"));
        assert!(!body.contains("private upstream diagnostic"));
        assert!(!body.contains("</script><script>bad()"));
        for error in [
            ApiFailure::unauthorized("authentication_required", "session required"),
            ApiFailure::forbidden("csrf_denied", "origin required"),
            ApiFailure::bad_gateway("invalid_app_csp", "invalid policy"),
            ApiFailure::bad_gateway("unrecognized_failure", "unknown"),
        ] {
            assert!(app_navigation_failure(error, true, &browser(), "ws", "app").is_err());
        }
        assert!(app_navigation_failure(
            ApiFailure::bad_gateway("stream_failed", "offline"),
            false,
            &browser(),
            "ws",
            "app"
        )
        .is_err());
    }

    #[test]
    fn app_embedding_preserves_every_csp_policy_and_allows_only_configured_ui() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; frame-ancestors 'none'; form-action 'none'",
            ),
        );
        headers.append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("script-src 'self'; FRAME-ANCESTORS 'self'"),
        );
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(header::SET_COOKIE, HeaderValue::from_static("app=value"));
        allow_control_plane_frame(&mut headers, &browser()).unwrap();
        let policies: Vec<_> = headers
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(
            policies,
            vec![
                "default-src 'self'; form-action 'none'; frame-ancestors 'self' https://app.treer.example",
                "script-src 'self'; frame-ancestors 'self' https://app.treer.example",
            ]
        );
        assert!(!headers.contains_key(header::X_FRAME_OPTIONS));
        assert_eq!(headers[header::SET_COOKIE], "app=value");
    }

    #[test]
    fn app_without_csp_still_limits_embedding_to_control_plane() {
        let mut headers = HeaderMap::new();
        allow_control_plane_frame(&mut headers, &browser()).unwrap();
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors 'self' https://app.treer.example"
        );
    }

    #[test]
    fn embedding_preserves_configured_console_origin_port() {
        let browser = BrowserAccess::new(
            &Url::parse("http://console.treer.localhost:8080/").unwrap(),
            &Url::parse("http://proxy.treer.localhost:8080/").unwrap(),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        allow_control_plane_frame(&mut headers, &browser).unwrap();
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "frame-ancestors 'self' http://console.treer.localhost:8080"
        );
    }
}
