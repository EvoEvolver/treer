use super::*;

// An in-process capability set only after App UI authentication. It cannot be
// populated by browser headers and is consumed after untrusted headers are removed.
#[derive(Clone)]
pub(in crate::api) struct ForwardedAppIdentity(pub String);

pub(in crate::api) async fn proxy_virtual_network_host_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, String::new(), request).await
}

pub(in crate::api) async fn proxy_virtual_network_host_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, path, request).await
}

pub(in crate::api) async fn proxy_virtual_network_host(
    state: AppState,
    auth: AuthStore,
    workspace_id: String,
    hostname: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let host = auth
        .resolve_virtual_network_host(&workspace_id, &hostname)
        .await?
        .ok_or_else(|| ApiFailure::not_found("virtual_host_not_found", &hostname))?;
    if host.service_protocol != treer_protocol::MachineServiceProtocol::Http {
        return Err(ApiFailure::bad_request(
            "service_protocol_mismatch",
            "browser access requires an HTTP service",
        ));
    }
    let query = request.uri().query();
    let target = if path.is_empty() {
        query.map_or_else(|| "/".to_string(), |query| format!("/?{query}"))
    } else {
        query.map_or_else(|| format!("/{path}"), |query| format!("/{path}?{query}"))
    };
    *request.uri_mut() = target
        .parse::<Uri>()
        .map_err(|error| ApiFailure::bad_gateway("invalid_tunnel_uri", &error.to_string()))?;
    *request.version_mut() = Version::HTTP_11;

    let upgraded = request.headers().contains_key(header::UPGRADE);
    sanitize_tunnel_request_headers(request.headers_mut(), upgraded, &host.hostname)?;
    if let Some(ForwardedAppIdentity(token)) =
        request.extensions_mut().remove::<ForwardedAppIdentity>()
    {
        request.headers_mut().insert(
            TREER_IDENTITY_TOKEN_HEADER,
            HeaderValue::from_str(&token).map_err(|_| {
                ApiFailure::internal("identity_token_failed", "invalid issued App identity")
            })?,
        );
    }
    tunnel_http_request(
        state,
        &workspace_id,
        &host.destination_server_id,
        host.destination_agent_id.as_deref(),
        &host.target_host,
        host.target_port.unwrap_or(80),
        request,
        true,
        "virtual host",
        TrafficClass::VirtualHost,
    )
    .await
}

pub(in crate::api) async fn proxy_agent_interface_ui_root(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, String::new(), request).await
}

pub(in crate::api) async fn proxy_agent_interface_ui_path(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, path, request).await
}

pub(in crate::api) async fn proxy_agent_interface_ui(
    state: AppState,
    workspace_id: String,
    target: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let interface = agent
        .interface
        .as_ref()
        .ok_or_else(|| ApiFailure::not_found("agent_interface_not_found", &agent.agent_id))?;
    let ui_path = interface
        .ui_path
        .as_deref()
        .ok_or_else(|| ApiFailure::not_found("agent_interface_ui_unavailable", &agent.agent_id))?;

    let target_path = if path.is_empty() {
        ui_path.to_string()
    } else if ui_path == "/" {
        format!("/{path}")
    } else {
        format!("{}/{path}", ui_path.trim_end_matches('/'))
    };
    let target = request.uri().query().map_or(target_path.clone(), |query| {
        format!("{target_path}?{query}")
    });
    *request.uri_mut() = target
        .parse::<Uri>()
        .map_err(|error| ApiFailure::bad_gateway("invalid_tunnel_uri", &error.to_string()))?;
    *request.version_mut() = Version::HTTP_11;
    let upgraded = request.headers().contains_key(header::UPGRADE);
    let authority = format!("127.0.0.1:{}", interface.port);
    sanitize_tunnel_request_headers(request.headers_mut(), upgraded, &authority)?;
    tunnel_http_request(
        state,
        &workspace_id,
        &agent.server_id,
        Some(&agent.agent_id),
        "127.0.0.1",
        interface.port,
        request,
        true,
        "Agent Interface UI",
        TrafficClass::AgentInterface,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(in crate::api) async fn tunnel_http_request(
    state: AppState,
    workspace_id: &str,
    server_id: &str,
    target_agent_id: Option<&str>,
    target_host: &str,
    target_port: u16,
    mut request: Request<Body>,
    strip_response_cookies: bool,
    route_kind: &'static str,
    traffic_class: TrafficClass,
) -> Result<Response, ApiFailure> {
    let stream = state
        .open_browser_network_stream(
            workspace_id,
            server_id,
            target_agent_id,
            target_host,
            target_port,
            traffic_class,
        )
        .await?;
    let upgraded = request.headers().contains_key(header::UPGRADE);
    let downstream_upgrade = upgraded.then(|| hyper::upgrade::on(&mut request));
    let io = TokioIo::new(stream);
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<_, Body>(io)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_handshake_failed", &error.to_string()))?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            tracing::debug!(%error, route_kind, "HTTP tunnel connection closed");
        }
    });
    let mut response = sender
        .send_request(request)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_request_failed", &error.to_string()))?;
    let target_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
        .then(|| hyper::upgrade::on(&mut response));
    if strip_response_cookies {
        sanitize_tunnel_response_headers(response.headers_mut(), target_upgrade.is_some());
    } else if target_upgrade.is_none() {
        remove_hop_by_hop_headers(response.headers_mut());
    }
    if let (Some(downstream), Some(target)) = (downstream_upgrade, target_upgrade) {
        tokio::spawn(async move {
            let Ok(downstream) = downstream.await else {
                return;
            };
            let Ok(target) = target.await else { return };
            let mut downstream = TokioIo::new(downstream);
            let mut target = TokioIo::new(target);
            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut target).await;
        });
    }
    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}

pub(in crate::api) fn sanitize_tunnel_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
) -> Result<(), ApiFailure> {
    headers.remove(header::COOKIE);
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(TREER_AUTHORIZATION_HEADER);
    for name in headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-treer-"))
        .cloned()
        .collect::<Vec<_>>()
    {
        headers.remove(name);
    }
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_gateway("invalid_virtual_host", &error.to_string()))?,
    );
    Ok(())
}

pub(in crate::api) fn sanitize_ingress_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
    public_scheme: &str,
    ingress_cookie_name: &str,
    identity_token: Option<&str>,
) -> Result<(), ApiFailure> {
    headers.remove(header::PROXY_AUTHORIZATION);
    headers.remove(TREER_AUTHORIZATION_HEADER);
    for name in headers
        .keys()
        .filter(|name| name.as_str().starts_with("x-treer-"))
        .cloned()
        .collect::<Vec<_>>()
    {
        headers.remove(name);
    }
    if let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        let forwarded = cookie
            .split(';')
            .map(str::trim)
            .filter(|item| {
                item.split_once('=').is_none_or(|(name, _)| {
                    name != ingress_cookie_name
                        && name != auth::SESSION_COOKIE
                        && name != auth::ADMIN_SESSION_COOKIE
                })
            })
            .collect::<Vec<_>>()
            .join("; ");
        if forwarded.is_empty() {
            headers.remove(header::COOKIE);
        } else {
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(&forwarded).map_err(|error| {
                    ApiFailure::bad_request("invalid_cookie", &error.to_string())
                })?,
            );
        }
    }
    for name in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        headers.remove(name);
    }
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_gateway("invalid_ingress_host", &error.to_string()))?,
    );
    headers.insert(
        "x-forwarded-host",
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_request("invalid_host", &error.to_string()))?,
    );
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_str(public_scheme)
            .map_err(|error| ApiFailure::internal("invalid_ingress_scheme", &error.to_string()))?,
    );
    if let Some(token) = identity_token {
        headers.insert(
            TREER_IDENTITY_TOKEN_HEADER,
            HeaderValue::from_str(token).map_err(|error| {
                ApiFailure::bad_request("invalid_identity_token", &error.to_string())
            })?,
        );
    }
    Ok(())
}

pub(in crate::api) fn sanitize_tunnel_response_headers(headers: &mut HeaderMap, upgraded: bool) {
    headers.remove(header::SET_COOKIE);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
}

pub(in crate::api) fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    for name in [
        header::CONNECTION,
        header::UPGRADE,
        header::TRANSFER_ENCODING,
        header::TE,
        header::TRAILER,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}
