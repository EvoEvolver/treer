use super::*;

#[derive(Debug, Deserialize)]
pub(in crate::api) struct IngressAuthorizeQuery {
    hostname: String,
    #[serde(default = "default_ingress_return_path")]
    return_path: String,
}

pub(in crate::api) fn default_ingress_return_path() -> String {
    "/".to_string()
}

pub(in crate::api) async fn authorize_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    Query(query): Query<IngressAuthorizeQuery>,
) -> Result<Response, ApiFailure> {
    auth::require_authorization_navigation(&headers)?;
    let request_host = request_hostname(&headers)?;
    let proxy_host = config
        .proxy_public_url
        .host_str()
        .ok_or_else(|| ApiFailure::internal("proxy_url_error", "proxy public URL has no host"))?;
    if !request_host.eq_ignore_ascii_case(proxy_host) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let resolved = auth
        .resolve_service_ingress_hostname(&query.hostname)
        .await?
        .filter(|resolved| resolved.ingress.enabled)
        .ok_or_else(|| ApiFailure::not_found("ingress_not_found", "service ingress not found"))?;
    if resolved.ingress.access != ServiceIngressAccess::Workspace {
        return Ok(Redirect::to(
            config
                .url_for_hostname(&resolved.ingress.hostname)?
                .as_str(),
        )
        .into_response());
    }
    let session = match auth::authenticate_request(&auth, &headers).await {
        Ok(session) => session,
        Err(error) => {
            let (status, error) = error.into_parts();
            if status != StatusCode::UNAUTHORIZED {
                return Err(ApiFailure { status, error });
            }
            let mut authorize_url = config.proxy_public_url.clone();
            authorize_url.set_path("/.treer/ingress/authorize");
            authorize_url.set_query(None);
            authorize_url
                .query_pairs_mut()
                .append_pair("hostname", &resolved.ingress.hostname)
                .append_pair("return_path", &query.return_path);
            let mut login_url = config.app_public_url.clone();
            login_url.set_query(None);
            login_url
                .query_pairs_mut()
                .append_pair("return_to", authorize_url.as_str());
            return Ok(Redirect::to(login_url.as_str()).into_response());
        }
    };
    let code = auth
        .create_ingress_auth_code(&resolved.ingress, &session.user_id, &query.return_path)
        .await?;
    let mut callback = config.url_for_hostname(&resolved.ingress.hostname)?;
    callback.set_path("/.treer/callback");
    callback.set_query(None);
    callback.query_pairs_mut().append_pair("code", &code);
    Ok(Redirect::to(callback.as_str()).into_response())
}

pub(in crate::api) async fn proxy_service_ingress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(identity): Extension<IdentityIssuer>,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let hostname = request_hostname(request.headers())?;
    if !config.matches_hostname(&hostname) {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let resolved = auth
        .resolve_service_ingress_hostname(&hostname)
        .await?
        .filter(|resolved| resolved.ingress.enabled)
        .ok_or_else(|| ApiFailure::not_found("ingress_not_found", "service ingress not found"))?;
    if request.uri().path() == "/.treer/callback" {
        return complete_ingress_authorization(&auth, &config, &hostname, request.uri()).await;
    }
    if request.uri().path().starts_with("/.treer/") {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }

    let mut identity_token = None;
    if resolved.ingress.access == ServiceIngressAccess::Workspace && !auth.authentication_disabled()
    {
        if let Some(token) = ingress_bearer_token(request.headers())? {
            let verified = identity.verify(token, &resolved.service.service_id);
            let valid = verified.claims.as_ref().is_some_and(|claims| {
                claims.workspace_id == resolved.ingress.workspace_id
                    && claims.service_id == resolved.service.service_id
            });
            if !verified.active || !valid {
                return Err(ApiFailure::unauthorized(
                    "ingress_authentication_required",
                    "valid workspace Agent credentials are required",
                ));
            }
            identity_token = Some(token.to_string());
        } else {
            let session = cookie_value(request.headers(), config.ingress_cookie_name());
            let user_id = match session {
                Some(token) => auth.authenticate_ingress_session(&hostname, &token).await?,
                None => None,
            };
            let Some(user_id) = user_id else {
                return redirect_to_ingress_authorization(&config, &hostname, request.uri());
            };
            identity_token = Some(
                issue_browser_app_identity(
                    &auth,
                    &identity,
                    &resolved.ingress.workspace_id,
                    &user_id,
                    &resolved.service.service_id,
                )
                .await?,
            );
        }
    }

    let upgraded = request.headers().contains_key(header::UPGRADE);
    sanitize_ingress_request_headers(
        request.headers_mut(),
        upgraded,
        &hostname,
        config.public_url.as_ref().map_or("http", url::Url::scheme),
        config.ingress_cookie_name(),
        identity_token.as_deref(),
    )?;
    tunnel_http_request(
        state,
        &resolved.ingress.workspace_id,
        &resolved.service.server_id,
        resolved.service.target_agent_id.as_deref(),
        &resolved.service.target_host,
        resolved.service.target_port,
        request,
        false,
        "service ingress",
        TrafficClass::ServiceIngress,
    )
    .await
}

pub(in crate::api) async fn complete_ingress_authorization(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    uri: &Uri,
) -> Result<Response, ApiFailure> {
    let code = uri
        .query()
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        })
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "invalid_ingress_authorization",
                "authorization code missing",
            )
        })?;
    let authorization = auth.consume_ingress_auth_code(hostname, &code).await?;
    let secure = if config
        .public_url
        .as_ref()
        .is_some_and(|url| url.scheme() == "https")
    {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        config.ingress_cookie_name(),
        authorization.session_token,
        12 * 60 * 60,
        secure,
    );
    let mut response = Redirect::to(&authorization.return_path).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|error| ApiFailure::internal("cookie_error", &error.to_string()))?,
    );
    Ok(response)
}

pub(in crate::api) fn redirect_to_ingress_authorization(
    config: &IngressConfig,
    hostname: &str,
    uri: &Uri,
) -> Result<Response, ApiFailure> {
    let mut url = config.proxy_public_url.clone();
    url.set_path("/.treer/ingress/authorize");
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("hostname", hostname)
        .append_pair("return_path", &uri.to_string());
    Ok(Redirect::to(url.as_str()).into_response())
}

pub(in crate::api) fn request_hostname(headers: &HeaderMap) -> Result<String, ApiFailure> {
    let authority = headers
        .get(header::HOST)
        .ok_or_else(|| ApiFailure::bad_request("host_required", "Host header is required"))?
        .to_str()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?;
    Ok(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

pub(in crate::api) fn ingress_bearer_token(
    headers: &HeaderMap,
) -> Result<Option<&str>, ApiFailure> {
    let Some(value) = headers.get(TREER_AUTHORIZATION_HEADER) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| {
        ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization header is invalid",
        )
    })?;
    let (scheme, token) = value.split_once(' ').ok_or_else(|| {
        ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization must contain a Bearer token",
        )
    })?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(ApiFailure::unauthorized(
            "ingress_authentication_required",
            "Treer-Authorization must contain a Bearer token",
        ));
    }
    Ok(Some(token.trim()))
}

pub(in crate::api) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
}
