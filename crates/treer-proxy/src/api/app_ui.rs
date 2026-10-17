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
    let mut response =
        proxy_virtual_network_host(state, auth, workspace_id, app.hostname, path, request).await?;
    allow_control_plane_frame(response.headers_mut(), &browser)?;
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
}
