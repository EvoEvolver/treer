use super::*;

// An in-process capability set only after App UI authentication. It cannot be
// populated by browser headers and is consumed after untrusted headers are removed.
#[derive(Clone)]
pub(in crate::api) struct ForwardedAppIdentity(pub String);

pub(in crate::api) async fn proxy_virtual_network_host_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    Path((workspace_id, hostname)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, String::new(), request).await
}

pub(in crate::api) async fn proxy_virtual_network_host_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    Path((workspace_id, hostname, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
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
        None,
    )
    .await
}

/// The interface UI shows the transcript and can prompt the Agent, so every
/// human or machine caller must hold both `agent.output.read` and
/// `agent.prompt` on the Agent (#143 Phase 2); evaluated per request.
pub(in crate::api) async fn require_agent_interface_access(
    policy: &PolicyEngine,
    workspace_id: &str,
    subject: &PolicySubject,
    agent: &AgentInfo,
) -> Result<(), ApiFailure> {
    for action in [ACTION_AGENT_OUTPUT_READ, ACTION_AGENT_PROMPT] {
        policy
            .authorize(&PolicyRequest::new(
                workspace_id,
                subject.clone(),
                action,
                agent_policy_resource(agent),
            ))
            .await?;
    }
    Ok(())
}

/// Bounded `origin_url` evaluation (#143 Phase 2): the read-model gate result
/// per (workspace, member, Agent) is remembered for a short window, so Console
/// refetch churn (an interface re-registering in a loop makes every member
/// refetch the snapshot) costs at most one Policy evaluation pair per member
/// per Agent per window on this Proxy instance. Only the link in the read
/// model can lag a Policy change by that window; the authorize step and every
/// request on the origin evaluate the gate uncached.
const ORIGIN_GATE_WINDOW: Duration = Duration::from_secs(5);
const ORIGIN_GATE_CACHE_CAP: usize = 10_000;
static ORIGIN_GATE_WINDOW_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(ORIGIN_GATE_WINDOW.as_millis() as u64);
type OriginGateKey = (String, String, String);
static ORIGIN_GATE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<OriginGateKey, (bool, std::time::Instant)>>,
> = std::sync::OnceLock::new();

fn origin_gate_window() -> Duration {
    Duration::from_millis(ORIGIN_GATE_WINDOW_MS.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
pub(in crate::api) fn set_origin_gate_window_for_test(window: Duration) {
    ORIGIN_GATE_WINDOW_MS.store(
        window.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn origin_gate_cached(key: &OriginGateKey) -> Option<bool> {
    let window = origin_gate_window();
    if window.is_zero() {
        return None;
    }
    let cache = ORIGIN_GATE_CACHE.get_or_init(Default::default);
    let cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(key)
        .filter(|(_, at)| at.elapsed() < window)
        .map(|(allowed, _)| *allowed)
}

fn origin_gate_remember(key: OriginGateKey, allowed: bool) {
    let window = origin_gate_window();
    if window.is_zero() {
        return;
    }
    let cache = ORIGIN_GATE_CACHE.get_or_init(Default::default);
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= ORIGIN_GATE_CACHE_CAP {
        cache.retain(|_, (_, at)| at.elapsed() < window);
        if cache.len() >= ORIGIN_GATE_CACHE_CAP {
            cache.clear();
        }
    }
    cache.insert(key, (allowed, std::time::Instant::now()));
}

/// Fills `interface.origin_url` on a read model for a human member who passes
/// the interface gate (#143 Phase 2); other subjects and denied members see no
/// origin, and a Policy denial is not an error here.
pub(in crate::api) async fn hydrate_agent_interface_origin(
    policy: &PolicyEngine,
    config: &crate::api::IngressConfig,
    workspace_id: &str,
    subject: Option<&PolicySubject>,
    agent: &mut AgentInfo,
) -> Result<(), ApiFailure> {
    let Some(subject @ PolicySubject::Human { user_id }) = subject else {
        return Ok(());
    };
    if agent.interface.is_none() {
        return Ok(());
    }
    let Some(origin_url) = config.agent_interface_origin_url(&agent.agent_id) else {
        return Ok(());
    };
    let key = (
        workspace_id.to_owned(),
        user_id.clone(),
        agent.agent_id.clone(),
    );
    let allowed = match origin_gate_cached(&key) {
        Some(allowed) => allowed,
        None => {
            let allowed =
                match require_agent_interface_access(policy, workspace_id, subject, agent).await {
                    Ok(()) => true,
                    Err(error) if error.error.code == "policy_denied" => false,
                    Err(error) => return Err(error),
                };
            origin_gate_remember(key, allowed);
            allowed
        }
    };
    if !allowed {
        return Ok(());
    }
    if let Some(interface) = agent.interface.as_mut() {
        interface.origin_url = Some(origin_url);
    }
    Ok(())
}

/// The Policy subject behind a Proxy-origin interface UI request: the human
/// session, else the authenticated machine or Agent; never anonymous.
async fn interface_ui_subject(
    state: &AppState,
    session: Option<&CurrentSession>,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<PolicySubject, ApiFailure> {
    if let Some(session) = session {
        return Ok(PolicySubject::Human {
            user_id: session.user_id.clone(),
        });
    }
    control_policy_subject(state, machine, headers, workspace_id)
        .await?
        .ok_or_else(|| {
            ApiFailure::forbidden("identity_required", "authenticated identity is required")
        })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::api) async fn proxy_agent_interface_ui_root(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
    browser.validate_tunnel_if_present(request.headers())?;
    let subject = interface_ui_subject(
        &state,
        session.as_deref(),
        machine.as_deref(),
        request.headers(),
        &workspace_id,
    )
    .await?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    require_agent_interface_access(&policy, &workspace_id, &subject, &agent).await?;
    let mut response =
        proxy_agent_interface_ui(state, workspace_id, agent_id, String::new(), request).await?;
    crate::api::app_ui::allow_control_plane_frame(response.headers_mut(), &browser)?;
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::api) async fn proxy_agent_interface_ui_path(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
    browser.validate_tunnel_if_present(request.headers())?;
    let subject = interface_ui_subject(
        &state,
        session.as_deref(),
        machine.as_deref(),
        request.headers(),
        &workspace_id,
    )
    .await?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    require_agent_interface_access(&policy, &workspace_id, &subject, &agent).await?;
    let mut response =
        proxy_agent_interface_ui(state, workspace_id, agent_id, path, request).await?;
    crate::api::app_ui::allow_control_plane_frame(response.headers_mut(), &browser)?;
    Ok(response)
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
    sanitize_agent_interface_headers(request.headers_mut(), upgraded, &agent.agent_id, interface)?;
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
        None,
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
    upgrade_permit: Option<std::sync::Arc<crate::ingress_guard::Permit>>,
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
            // The in-flight permit (public ingress) lives as long as the
            // upgraded connection does.
            let _permit = upgrade_permit;
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

pub(in crate::api) fn sanitize_agent_interface_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    agent_id: &str,
    interface: &treer_protocol::AgentInterfaceDescriptor,
) -> Result<(), ApiFailure> {
    // Only this authenticated Agent UI route injects provenance. Generic service
    // and public ingress routes strip these headers without restoring them.
    sanitize_tunnel_request_headers(headers, upgraded, &format!("127.0.0.1:{}", interface.port))?;
    for (name, value) in [
        (
            treer_protocol::AGENT_INTERFACE_SOURCE_HEADER,
            treer_protocol::AGENT_INTERFACE_SOURCE_UI,
        ),
        (treer_protocol::AGENT_ID_HEADER, agent_id),
        (
            treer_protocol::AGENT_INTERFACE_INSTANCE_HEADER,
            interface.instance_id.as_str(),
        ),
    ] {
        headers.insert(
            name,
            HeaderValue::from_str(value).map_err(|_| {
                ApiFailure::bad_gateway(
                    "invalid_agent_interface",
                    "invalid registered interface identity",
                )
            })?,
        );
    }
    Ok(())
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
        .filter(|name| treer_protocol::is_treer_header_name(name.as_str()))
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
        .filter(|name| treer_protocol::is_treer_header_name(name.as_str()))
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
                        && !name.starts_with("__Host-treer_ingress_nonce_")
                        && !name.starts_with("treer_ingress_nonce_")
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

#[cfg(test)]
mod provenance_tests {
    use super::*;

    #[test]
    fn agent_ui_replaces_spoofed_provenance_for_http_and_websocket() {
        let interface = treer_protocol::AgentInterfaceDescriptor {
            origin_url: None,
            protocol: treer_protocol::AGENT_INTERFACE_PROTOCOL_V1.into(),
            instance_id: "registered-instance".into(),
            port: 12345,
            capabilities: vec![],
            ui_path: Some("/".into()),
            registered_at: Utc::now(),
        };
        for upgraded in [false, true] {
            let mut headers = HeaderMap::new();
            for (name, value) in [
                ("x-treer-interface-source", "controller-api"),
                ("x-treer-interface-instance", "forged"),
                ("x-treer-agent-id", "victim"),
                ("host", "evil.example"),
                ("origin", "https://proxy.example.test"),
                ("cookie", "synthetic-session"),
                ("authorization", "Bearer synthetic"),
                ("upgrade", "websocket"),
                ("connection", "upgrade"),
            ] {
                headers.insert(name, HeaderValue::from_static(value));
            }
            sanitize_agent_interface_headers(&mut headers, upgraded, "target", &interface).unwrap();
            assert_eq!(headers["x-treer-interface-source"], "proxy-ui");
            assert_eq!(headers["x-treer-interface-instance"], "registered-instance");
            assert_eq!(headers["x-treer-agent-id"], "target");
            assert_eq!(headers["host"], "127.0.0.1:12345");
            assert_eq!(headers["origin"], "https://proxy.example.test");
            assert!(!headers.contains_key("cookie"));
            assert!(!headers.contains_key("authorization"));
            assert_eq!(headers.contains_key("upgrade"), upgraded);
        }
    }

    #[test]
    fn ordinary_and_public_tunnels_never_preserve_interface_provenance() {
        for public in [false, true] {
            let mut headers = HeaderMap::new();
            for name in [
                "x-treer-interface-source",
                "x-treer-interface-instance",
                "x-treer-agent-id",
                "x_treer_identity_token",
                "X_TREER_AGENT_ID",
            ] {
                headers.insert(name, HeaderValue::from_static("forged"));
            }
            if public {
                sanitize_ingress_request_headers(
                    &mut headers,
                    true,
                    "public.example.test",
                    "https",
                    "ingress",
                    None,
                )
                .unwrap();
            } else {
                sanitize_tunnel_request_headers(&mut headers, true, "service.example.test")
                    .unwrap();
            }
            assert!(!headers
                .keys()
                .any(|name| treer_protocol::is_treer_header_name(name.as_str())));
        }
    }
}
