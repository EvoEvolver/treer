use super::ingress_body::{GuardedResponseBody, LimitedBody, STOP_TIMEOUT, STOP_TOO_LARGE};
use super::*;
use crate::ingress_guard::{Refusal, Rejection};
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub(in crate::api) struct IngressAuthorizeQuery {
    hostname: String,
    #[serde(default = "default_ingress_return_path")]
    return_path: String,
    browser_nonce_hash: String,
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
    if !auth::valid_ingress_nonce(&query.browser_nonce_hash) {
        return Err(auth::invalid_ingress_authorization().into());
    }
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
                .append_pair("return_path", &query.return_path)
                .append_pair("browser_nonce_hash", &query.browser_nonce_hash);
            let mut login_url = config.app_public_url.clone();
            login_url.set_query(None);
            login_url
                .query_pairs_mut()
                .append_pair("return_to", authorize_url.as_str());
            return Ok(Redirect::to(login_url.as_str()).into_response());
        }
    };
    let code = auth
        .create_ingress_auth_code(
            &resolved.ingress,
            &session.user_id,
            &query.return_path,
            &query.browser_nonce_hash,
        )
        .await?;
    let mut callback = config.url_for_hostname(&resolved.ingress.hostname)?;
    callback.set_path("/.treer/callback");
    callback.set_query(None);
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("browser_nonce_hash", &query.browser_nonce_hash);
    let mut response = Redirect::to(callback.as_str()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

/// Serves one request for a service ingress hostname. Public ingress (#146)
/// is admitted per client and per service before anything is forwarded,
/// capped in body size, bounded in time, and tagged with the verified client
/// address; workspace ingress only gets the time bounds.
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
    let guard = config.guard();
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip());
    let resolved = auth
        .resolve_service_ingress_hostname(&hostname)
        .await?
        .filter(|resolved| resolved.ingress.enabled);
    let Some(resolved) = resolved else {
        // Unknown hostnames are answered from the snapshot, so the only cost
        // left is this response; one client cannot have unbounded many.
        if let Some(peer) = peer {
            if let Err(refusal) = guard.admit_unknown_host(peer, request.headers()) {
                return Ok(refusal_response(refusal));
            }
        }
        return Err(ApiFailure::not_found(
            "ingress_not_found",
            "service ingress not found",
        ));
    };
    if request.uri().path() == "/.treer/callback" {
        require_ingress_navigation(request.headers())?;
        return complete_ingress_authorization(
            &auth,
            &config,
            &hostname,
            request.uri(),
            request.headers(),
        )
        .await;
    }
    if request.uri().path().starts_with("/.treer/") {
        return Err(ApiFailure::not_found("route_not_found", "route not found"));
    }
    let public = resolved.ingress.access == ServiceIngressAccess::Public;
    let limits = resolved.ingress.limits.effective();
    let ingress_id = resolved.ingress.ingress_id.clone();
    // Ambiguous framing is refused for every ingress: it is how request
    // smuggling starts.
    if request.headers().contains_key(header::CONTENT_LENGTH)
        && request.headers().contains_key(header::TRANSFER_ENCODING)
    {
        guard.record(&ingress_id, Rejection::RequestRejected);
        return Err(ApiFailure::bad_request(
            "ingress_request_rejected",
            "request carries both Content-Length and Transfer-Encoding",
        ));
    }
    let upgraded = request.headers().contains_key(header::UPGRADE);
    let mut admission = None;
    if public {
        // No peer address means no socket (tests): treat it as a local peer.
        let peer = peer.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        match guard.admit(&ingress_id, &limits, peer, request.headers()) {
            Ok(admitted) => {
                tracing::trace!(
                    ingress_id,
                    per_ip_enforced = admitted.per_ip_enforced,
                    "public ingress request admitted"
                );
                admission = Some(admitted);
            }
            Err(refusal) => return Ok(refusal_response(refusal)),
        }
        if let Some(length) = declared_content_length(request.headers()) {
            if length > limits.max_body_bytes {
                guard.record(&ingress_id, Rejection::BodyTooLarge);
                return Err(body_too_large());
            }
        }
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
                require_ingress_navigation(request.headers())?;
                return redirect_to_ingress_authorization(
                    &config,
                    &hostname,
                    request.uri(),
                    request.headers(),
                );
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

    sanitize_ingress_request_headers(
        request.headers_mut(),
        upgraded,
        &hostname,
        config.public_url.as_ref().map_or("http", url::Url::scheme),
        config.ingress_cookie_name(),
        identity_token.as_deref(),
    )?;
    // The front's own header never reaches the App: only the verified header
    // below is meaningful there.
    if let Some(name) = guard.client_ip_header() {
        request.headers_mut().remove(name);
    }
    // After the strip of every client x-treer-* header: the verified address.
    if let Some(admitted) = &admission {
        request.headers_mut().insert(
            treer_protocol::TREER_CLIENT_IP_HEADER,
            HeaderValue::from_str(&admitted.client.address.to_string())
                .map_err(|error| ApiFailure::internal("invalid_client_ip", &error.to_string()))?,
        );
    }
    // Request body: capped for public ingress, idle-bounded for every ingress
    // (upgraded connections are governed by their permit instead).
    let body_outcome = if upgraded {
        None
    } else {
        let cap = if public {
            limits.max_body_bytes
        } else {
            u64::MAX
        };
        let body = std::mem::replace(request.body_mut(), Body::empty());
        let (limited, outcome) = LimitedBody::new(
            body,
            cap,
            BODY_IDLE_TIMEOUT,
            Duration::from_secs(u64::from(limits.body_timeout_secs)),
        );
        *request.body_mut() = Body::new(limited);
        Some(outcome)
    };
    let permit = admission.map(|admitted| std::sync::Arc::new(admitted.permit));
    let tunnel = tunnel_http_request(
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
        permit.clone(),
    );
    let response = match tokio::time::timeout(
        Duration::from_secs(u64::from(limits.response_timeout_secs)),
        tunnel,
    )
    .await
    {
        Err(_) => {
            guard.record(&ingress_id, Rejection::UpstreamTimeout);
            return Err(ApiFailure {
                status: StatusCode::GATEWAY_TIMEOUT,
                error: ProtocolError::new(
                    "ingress_upstream_timeout",
                    "the service did not answer in time",
                ),
            });
        }
        Ok(Err(error)) => {
            if let Some(outcome) = &body_outcome {
                match outcome.stopped_by() {
                    STOP_TOO_LARGE => {
                        guard.record(&ingress_id, Rejection::BodyTooLarge);
                        return Err(body_too_large());
                    }
                    STOP_TIMEOUT => {
                        guard.record(&ingress_id, Rejection::BodyTimeout);
                        return Err(ApiFailure {
                            status: StatusCode::REQUEST_TIMEOUT,
                            error: ProtocolError::new(
                                "ingress_body_timeout",
                                "the request body stopped arriving",
                            ),
                        });
                    }
                    _ => {}
                }
            }
            return Err(error);
        }
        Ok(Ok(response)) => response,
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        // The upgrade task owns the permit for the connection's lifetime.
        return Ok(response);
    }
    let (parts, body) = response.into_parts();
    let guarded = GuardedResponseBody::new(body, RESPONSE_IDLE_TIMEOUT, PERMIT_LEASE, permit);
    Ok(Response::from_parts(parts, Body::new(guarded)))
}

/// Limits that are not per-App configurable.
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// A non-upgraded response holds its in-flight permit at most this long,
/// even when the client never reads it; the stream itself may continue.
const PERMIT_LEASE: Duration = Duration::from_secs(300);

fn declared_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn body_too_large() -> ApiFailure {
    ApiFailure {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        error: ProtocolError::new(
            "ingress_body_too_large",
            "request body exceeds this service's limit",
        ),
    }
}

/// 429 with Retry-After and a fixed JSON body; nothing about the client.
fn refusal_response(refusal: Refusal) -> Response {
    let (code, message) = match refusal.rejection {
        Rejection::IpRateLimited => (
            "ingress_rate_limited",
            "too many requests from this client; retry later",
        ),
        Rejection::ConcurrencyLimited => (
            "ingress_concurrency_limited",
            "the service is handling its maximum number of requests; retry later",
        ),
        _ => (
            "ingress_rate_limited",
            "too many requests for this service; retry later",
        ),
    };
    let mut response = ApiFailure {
        status: StatusCode::TOO_MANY_REQUESTS,
        error: ProtocolError::new(code, message),
    }
    .into_response();
    if let Some(seconds) = refusal.retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(in crate::api) async fn complete_ingress_authorization(
    auth: &AuthStore,
    config: &IngressConfig,
    hostname: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<Response, ApiFailure> {
    let pairs: Vec<_> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).collect();
    let single = |name: &str| {
        let mut values = pairs.iter().filter(|(key, _)| key == name);
        let value = values.next()?.1.as_ref();
        values.next().is_none().then_some(value)
    };
    let code = single("code").ok_or_else(auth::invalid_ingress_authorization)?;
    let challenge = single("browser_nonce_hash").filter(|value| auth::valid_ingress_nonce(value));
    let nonce_cookie = challenge.map(|hash| ingress_nonce_cookie_name(config, hash));
    let nonce = nonce_cookie
        .as_ref()
        .and_then(|name| single_cookie_value(headers, name));
    let result = auth
        .consume_ingress_auth_code(hostname, code, nonce.as_deref())
        .await;
    let mut response = match result {
        Ok(authorization) => ingress_authorized_response(config, authorization)?,
        Err(error) => error.into_response(),
    };
    if let Some(name) = nonce_cookie {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
                ingress_secure_cookie(config)
            ))
            .unwrap(),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

fn ingress_secure_cookie(config: &IngressConfig) -> &'static str {
    if config
        .public_url
        .as_ref()
        .is_some_and(|url| url.scheme() == "https")
    {
        "; Secure"
    } else {
        ""
    }
}

fn ingress_nonce_cookie_name(config: &IngressConfig, challenge: &str) -> String {
    // __Host prevents a sibling ingress setting a Domain cookie with a known
    // verifier. It requires Path=/; strip the nonce before forwarding to Apps.
    let prefix = if ingress_secure_cookie(config).is_empty() {
        "treer_ingress_nonce_"
    } else {
        "__Host-treer_ingress_nonce_"
    };
    format!("{prefix}{challenge}")
}

fn single_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut result = None;
    for header in headers.get_all(header::COOKIE) {
        for item in header.to_str().ok()?.split(';') {
            let Some((key, value)) = item.trim().split_once('=') else {
                continue;
            };
            if key == name {
                if result.is_some() {
                    return None;
                }
                result = Some(value.to_owned());
            }
        }
    }
    result
}

fn require_ingress_navigation(headers: &HeaderMap) -> Result<(), ApiFailure> {
    auth::require_authorization_navigation(headers)
        .map_err(|_| auth::invalid_ingress_authorization().into())
}

fn ingress_authorized_response(
    config: &IngressConfig,
    authorization: auth::ConsumedIngressAuthorization,
) -> Result<Response, ApiFailure> {
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
    headers: &HeaderMap,
) -> Result<Response, ApiFailure> {
    // Three OS-random UUIDs supply >256 bits of entropy; hash to a fixed,
    // header-safe 256-bit verifier rather than relying on UUID formatting bits.
    let nonce = auth::ingress_nonce_hash(&format!(
        "{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ));
    let challenge = auth::ingress_nonce_hash(&nonce);
    let mut url = config.proxy_public_url.clone();
    url.set_path("/.treer/ingress/authorize");
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("hostname", hostname)
        .append_pair("return_path", &uri.to_string())
        .append_pair("browser_nonce_hash", &challenge);
    let mut response = Redirect::to(url.as_str()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={nonce}; Path=/; HttpOnly; SameSite=Lax; Max-Age=300{}",
            ingress_nonce_cookie_name(config, &challenge),
            ingress_secure_cookie(config)
        ))
        .unwrap(),
    );
    // Browser cookie order is creation order for equal paths. Retain at most
    // seven prior flows plus the new one; duplicate/forged names grant nothing.
    let prefix = ingress_nonce_cookie_name(config, "");
    let mut names = Vec::new();
    for cookie in headers.get_all(header::COOKIE) {
        for item in cookie.to_str().unwrap_or("").split(';') {
            if let Some((name, _)) = item.trim().split_once('=') {
                if name
                    .strip_prefix(&prefix)
                    .is_some_and(auth::valid_ingress_nonce)
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
        }
    }
    for name in names.iter().take(names.len().saturating_sub(7)) {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
                ingress_secure_cookie(config)
            ))
            .unwrap(),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
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

#[cfg(test)]
mod browser_binding_tests {
    use super::*;

    #[test]
    fn nonce_cookies_and_fetch_metadata_reject_ambiguity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("nonce=one; unrelated=kept"),
        );
        assert_eq!(
            single_cookie_value(&headers, "nonce").as_deref(),
            Some("one")
        );
        headers.append(header::COOKIE, HeaderValue::from_static("nonce=two"));
        assert_eq!(single_cookie_value(&headers, "nonce"), None);
        for (name, value) in [("sec-fetch-mode", "cors"), ("sec-fetch-dest", "image")] {
            let mut headers = HeaderMap::new();
            headers.insert(name, HeaderValue::from_static(value));
            assert!(require_ingress_navigation(&headers).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.append("sec-fetch-mode", HeaderValue::from_static("navigate"));
        headers.append("sec-fetch-mode", HeaderValue::from_static("navigate"));
        assert!(require_ingress_navigation(&headers).is_err());
    }

    #[test]
    fn browser_flows_are_host_prefixed_bounded_and_not_forwarded() {
        let config = IngressConfig::new(
            Some(Url::parse("https://apps.example.test").unwrap()),
            &Url::parse("https://proxy.example.test").unwrap(),
            &Url::parse("https://console.example.test").unwrap(),
        )
        .unwrap();
        let cookies = (0..10)
            .map(|i| {
                format!(
                    "{}={}",
                    ingress_nonce_cookie_name(&config, &format!("{i:064x}")),
                    "a".repeat(64)
                )
            })
            .collect::<Vec<_>>();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&cookies.join("; ")).unwrap(),
        );
        let response = redirect_to_ingress_authorization(
            &config,
            "svc.apps.example.test",
            &"/".parse().unwrap(),
            &headers,
        )
        .unwrap();
        let emitted = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|h| h.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            emitted.len(),
            4,
            "one new flow and three oldest flows evicted"
        );
        for cookie in emitted {
            assert!(cookie.starts_with("__Host-treer_ingress_nonce_"));
            assert!(cookie.contains("; Path=/; HttpOnly; SameSite=Lax;"));
            assert!(cookie.ends_with("; Secure"));
            assert!(
                !cookie.contains("Domain="),
                "__Host with Domain must be rejected by browsers"
            );
        }
        let forwarded = format!(
            "{}; treer_ingress_nonce_untrusted=private; app_owned=retained",
            cookies.join("; ")
        );
        headers.insert(header::COOKIE, HeaderValue::from_str(&forwarded).unwrap());
        sanitize_ingress_request_headers(
            &mut headers,
            false,
            "svc.apps.example.test",
            "https",
            config.ingress_cookie_name(),
            None,
        )
        .unwrap();
        assert_eq!(headers[header::COOKIE], "app_owned=retained");
    }
}
