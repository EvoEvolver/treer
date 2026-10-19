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
        // Virtual-host tunnels carry no browser session of their own (#194).
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
/// per (workspace, member, Agent, Policy revision) is remembered for a short
/// window, so Console refetch churn (an interface re-registering in a loop
/// makes every member refetch the snapshot) costs at most one Policy
/// evaluation pair per member per Agent per window on this Proxy instance.
/// The revision in the key is the one the evaluators would apply now, so a
/// published Policy is re-evaluated as soon as the engine itself sees it;
/// allowed and denied results share the window, a failed evaluation is never
/// remembered, an unavailable revision (Policy document not loadable) bypasses
/// the cache entirely, and the authorize step and every request on the origin
/// evaluate the gate uncached.
const ORIGIN_GATE_WINDOW: Duration = Duration::from_secs(5);
const ORIGIN_GATE_CACHE_CAP: usize = 10_000;
static ORIGIN_GATE_WINDOW_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(ORIGIN_GATE_WINDOW.as_millis() as u64);
type OriginGateKey = (String, String, String, Option<u64>);
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
    let key = match policy.policy_revision(workspace_id).await {
        crate::policy::PolicyRevision::Known(revision) => Some((
            workspace_id.to_owned(),
            user_id.clone(),
            agent.agent_id.clone(),
            Some(revision),
        )),
        crate::policy::PolicyRevision::Unrevisioned => Some((
            workspace_id.to_owned(),
            user_id.clone(),
            agent.agent_id.clone(),
            None,
        )),
        crate::policy::PolicyRevision::Unavailable => None,
    };
    let cached = key.as_ref().and_then(origin_gate_cached);
    let allowed = match cached {
        Some(allowed) => allowed,
        None => {
            let allowed =
                match require_agent_interface_access(policy, workspace_id, subject, agent).await {
                    Ok(()) => true,
                    Err(error) if error.error.code == "policy_denied" => false,
                    Err(error) => return Err(error),
                };
            if let Some(key) = key {
                origin_gate_remember(key, allowed);
            }
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
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    Extension(ingress): Extension<IngressConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
    require_tunnel_fallback_allowed(&ingress)?;
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
    let revalidation = console_upgrade_revalidation(
        &auth,
        &policy,
        session.as_deref(),
        &subject,
        &workspace_id,
        &agent,
    );
    let mut response = proxy_agent_interface_ui(
        state,
        workspace_id,
        agent_id,
        String::new(),
        request,
        revalidation,
    )
    .await?;
    crate::api::app_ui::allow_control_plane_frame(response.headers_mut(), &browser)?;
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::api) async fn proxy_agent_interface_ui_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(bootstrap): Extension<BootstrapConfig>,
    Extension(ingress): Extension<IngressConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    // App content must never run on the Console's origin (#132).
    bootstrap.app_dist().require_embedding_allowed()?;
    require_tunnel_fallback_allowed(&ingress)?;
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
    let revalidation = console_upgrade_revalidation(
        &auth,
        &policy,
        session.as_deref(),
        &subject,
        &workspace_id,
        &agent,
    );
    let mut response =
        proxy_agent_interface_ui(state, workspace_id, agent_id, path, request, revalidation)
            .await?;
    crate::api::app_ui::allow_control_plane_frame(response.headers_mut(), &browser)?;
    Ok(response)
}

pub(in crate::api) async fn proxy_agent_interface_ui(
    state: AppState,
    workspace_id: String,
    target: String,
    path: String,
    mut request: Request<Body>,
    revalidation: Option<UpgradeRevalidation>,
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
        revalidation,
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
    revalidation: Option<UpgradeRevalidation>,
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
            match revalidation {
                None => {
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut target).await;
                }
                Some(revalidation) => {
                    relay_with_revalidation(&mut downstream, &mut target, revalidation).await;
                }
            }
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

/// For a human Console session on the Proxy-origin interface tunnel: the
/// session, membership and the interface gate are re-checked while an
/// upgraded socket lives (#194). Machine subjects keep their credential
/// semantics and are not re-checked here.
fn console_upgrade_revalidation(
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    subject: &PolicySubject,
    workspace_id: &str,
    agent: &AgentInfo,
) -> Option<UpgradeRevalidation> {
    let (PolicySubject::Human { user_id }, Some(session)) = (subject, session) else {
        return None;
    };
    Some(
        UpgradeRevalidation::console(auth.clone(), &session.token, workspace_id, user_id)
            .with_agent_gate(policy.clone(), agent.clone()),
    )
}

/// How often an upgraded tunnel re-checks the browser authority behind it
/// (#194), plus a per-socket jitter so N sockets spread their lookups.
pub(in crate::api) const TUNNEL_REVALIDATE_INTERVAL: Duration = Duration::from_secs(30);
pub(in crate::api) const TUNNEL_REVALIDATE_JITTER: Duration = Duration::from_secs(5);
/// Consecutive checks that may fail on database errors before the socket is
/// closed anyway (about 90 s of unverified authority).
pub(in crate::api) const TUNNEL_UNVERIFIED_MAX_CHECKS: u32 = 3;
/// Close codes: the same as the events stream for session and membership.
pub(in crate::api) const TUNNEL_CLOSE_SESSION_ENDED: u16 = 4401;
pub(in crate::api) const TUNNEL_CLOSE_ACCESS_REVOKED: u16 = 4403;
pub(in crate::api) const TUNNEL_CLOSE_UNVERIFIED: u16 = 1011;
/// Writing the close frame and shutting a half down never blocks the task
/// longer than this.
const TUNNEL_CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

static TUNNEL_REVALIDATE_INTERVAL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(TUNNEL_REVALIDATE_INTERVAL.as_millis() as u64);
static TUNNEL_REVALIDATE_JITTER_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(TUNNEL_REVALIDATE_JITTER.as_millis() as u64);

fn tunnel_revalidate_interval() -> Duration {
    Duration::from_millis(TUNNEL_REVALIDATE_INTERVAL_MS.load(std::sync::atomic::Ordering::Relaxed))
}

fn tunnel_revalidate_jitter() -> Duration {
    let max = TUNNEL_REVALIDATE_JITTER_MS.load(std::sync::atomic::Ordering::Relaxed);
    if max == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(rand::Rng::gen_range(&mut rand::thread_rng(), 0..=max))
}

/// Tests shrink the window; production never changes it.
#[cfg(test)]
pub(in crate::api) fn set_tunnel_revalidation_for_test(interval: Duration, jitter: Duration) {
    TUNNEL_REVALIDATE_INTERVAL_MS.store(
        interval.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    TUNNEL_REVALIDATE_JITTER_MS.store(
        jitter.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The browser session that authorized an upgraded tunnel, re-checked while
/// the socket lives (#194): the same lookups as the HTTP path (ingress or
/// embed session by hostname, token and kind; or the Console session), then
/// current workspace membership, then the interface gate without recording
/// a Policy decision. Holds the session token for the life of the socket, so
/// it is never printed: `Debug` shows the kind and the ids only.
pub(in crate::api) struct UpgradeRevalidation {
    auth: AuthStore,
    session: RevalidatedSession,
    workspace_id: String,
    user_id: String,
    agent_gate: Option<(PolicyEngine, AgentInfo)>,
}

enum RevalidatedSession {
    Ingress {
        hostname: String,
        token: String,
        embed: bool,
    },
    Console {
        token: String,
    },
}

impl std::fmt::Debug for UpgradeRevalidation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.session {
            RevalidatedSession::Ingress { embed: true, .. } => "embed",
            RevalidatedSession::Ingress { embed: false, .. } => "ingress",
            RevalidatedSession::Console { .. } => "console",
        };
        f.debug_struct("UpgradeRevalidation")
            .field("session", &kind)
            .field("workspace_id", &self.workspace_id)
            .field("user_id", &self.user_id)
            .field(
                "agent_id",
                &self
                    .agent_gate
                    .as_ref()
                    .map(|(_, agent)| agent.agent_id.as_str()),
            )
            .finish_non_exhaustive()
    }
}

/// Result of one re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::api) enum RevalidationOutcome {
    /// Everything the HTTP path would require still holds.
    Verified,
    /// The session is gone, expired, or belongs to someone else.
    SessionEnded,
    /// Membership or the interface gate no longer allows this user.
    AccessRevoked,
    /// A database or Policy-store error: nothing is granted, retried next tick.
    Unavailable,
}

impl UpgradeRevalidation {
    pub(in crate::api) fn ingress(
        auth: AuthStore,
        hostname: &str,
        token: &str,
        embed: bool,
        workspace_id: &str,
        user_id: &str,
    ) -> Self {
        Self {
            auth,
            session: RevalidatedSession::Ingress {
                hostname: hostname.to_owned(),
                token: token.to_owned(),
                embed,
            },
            workspace_id: workspace_id.to_owned(),
            user_id: user_id.to_owned(),
            agent_gate: None,
        }
    }

    pub(in crate::api) fn console(
        auth: AuthStore,
        token: &str,
        workspace_id: &str,
        user_id: &str,
    ) -> Self {
        Self {
            auth,
            session: RevalidatedSession::Console {
                token: token.to_owned(),
            },
            workspace_id: workspace_id.to_owned(),
            user_id: user_id.to_owned(),
            agent_gate: None,
        }
    }

    /// Also re-evaluate the Agent interface gate (`agent.output.read` and
    /// `agent.prompt`) for this user, without recording a Policy decision.
    pub(in crate::api) fn with_agent_gate(
        mut self,
        policy: PolicyEngine,
        agent: AgentInfo,
    ) -> Self {
        self.agent_gate = Some((policy, agent));
        self
    }

    pub(in crate::api) async fn check(&self) -> RevalidationOutcome {
        let session = match &self.session {
            RevalidatedSession::Ingress {
                hostname,
                token,
                embed,
            } => {
                self.auth
                    .authenticate_ingress_session_kind(hostname, token, *embed)
                    .await
            }
            RevalidatedSession::Console { token } => self
                .auth
                .revalidate_session(token)
                .await
                .map(|current| current.map(|current| current.user_id)),
        };
        let membership = match self
            .auth
            .require_workspace_member(&self.workspace_id, &self.user_id)
            .await
        {
            Ok(()) => RevalidationOutcome::Verified,
            Err(error) if error.code() == "database_error" => RevalidationOutcome::Unavailable,
            Err(_) => RevalidationOutcome::AccessRevoked,
        };
        match session {
            Ok(Some(user_id)) if user_id == self.user_id => {}
            // The ingress lookup already folds membership in and answers
            // "no session" for a former member; name the real reason.
            Ok(_) if membership == RevalidationOutcome::AccessRevoked => {
                return RevalidationOutcome::AccessRevoked
            }
            Ok(_) => return RevalidationOutcome::SessionEnded,
            Err(error) if error.code() == "database_error" => {
                return RevalidationOutcome::Unavailable
            }
            Err(_) => return RevalidationOutcome::SessionEnded,
        }
        if membership != RevalidationOutcome::Verified {
            return membership;
        }
        if let Some((policy, agent)) = &self.agent_gate {
            for action in [ACTION_AGENT_OUTPUT_READ, ACTION_AGENT_PROMPT] {
                let request = PolicyRequest::new(
                    &self.workspace_id,
                    PolicySubject::Human {
                        user_id: self.user_id.clone(),
                    },
                    action,
                    agent_policy_resource(agent),
                );
                match policy.explain_request(&request).await {
                    Ok(explanation)
                        if explanation.effective_decision
                            == treer_protocol::PolicyEffect::Allow => {}
                    Ok(_) => return RevalidationOutcome::AccessRevoked,
                    Err(_) => return RevalidationOutcome::Unavailable,
                }
            }
        }
        RevalidationOutcome::Verified
    }
}

/// An unmasked server close frame: FIN + opcode 8, two-byte code, short
/// ASCII reason.
pub(in crate::api) fn websocket_close_frame(code: u16, reason: &str) -> Vec<u8> {
    let reason = &reason.as_bytes()[..reason.len().min(123)];
    let mut frame = Vec::with_capacity(4 + reason.len());
    frame.push(0x88);
    frame.push((2 + reason.len()) as u8);
    frame.extend_from_slice(&code.to_be_bytes());
    frame.extend_from_slice(reason);
    frame
}

/// One re-check may take at most this long before it counts as unavailable,
/// so a stalled database never holds a verdict (or a socket) open.
pub(in crate::api) const TUNNEL_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
static TUNNEL_CHECK_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(TUNNEL_CHECK_TIMEOUT.as_millis() as u64);

fn tunnel_check_timeout() -> Duration {
    Duration::from_millis(TUNNEL_CHECK_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
pub(in crate::api) fn set_tunnel_check_timeout_for_test(timeout: Duration) {
    TUNNEL_CHECK_TIMEOUT_MS.store(
        timeout.as_millis() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Runs the periodic re-checks on their own task so a pending lookup never
/// stops bytes from moving; resolves with the close code once the socket
/// must go. Each check is bounded by [`TUNNEL_CHECK_TIMEOUT`] and a timed-out
/// check counts as unavailable.
async fn revalidation_verdict(revalidation: UpgradeRevalidation) -> (u16, UpgradeRevalidation) {
    let mut unverified = 0_u32;
    loop {
        tokio::time::sleep(tunnel_revalidate_interval() + tunnel_revalidate_jitter()).await;
        let outcome = tokio::time::timeout(tunnel_check_timeout(), revalidation.check())
            .await
            .unwrap_or(RevalidationOutcome::Unavailable);
        match outcome {
            RevalidationOutcome::Verified => unverified = 0,
            RevalidationOutcome::Unavailable => {
                unverified += 1;
                tracing::warn!(
                    revalidation = ?revalidation,
                    unverified,
                    "tunnel session re-check failed; keeping the socket until the next check"
                );
                if unverified >= TUNNEL_UNVERIFIED_MAX_CHECKS {
                    return (TUNNEL_CLOSE_UNVERIFIED, revalidation);
                }
            }
            RevalidationOutcome::SessionEnded => return (TUNNEL_CLOSE_SESSION_ENDED, revalidation),
            RevalidationOutcome::AccessRevoked => {
                return (TUNNEL_CLOSE_ACCESS_REVOKED, revalidation)
            }
        }
    }
}

/// Relays the upgraded bytes until either side closes or a re-check fails;
/// then sends a close frame to the browser and shuts both halves down, so the
/// Agent or App stops receiving as well. The relay is byte-level: when the
/// target was mid-frame the browser sees a protocol error instead of the code.
/// Either way the socket is gone within one interval of the revocation. The
/// checks run concurrently with the relay (see [`revalidation_verdict`]), so
/// a slow lookup never pauses the socket.
async fn relay_with_revalidation<D, T>(
    downstream: &mut D,
    target: &mut T,
    revalidation: UpgradeRevalidation,
) where
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let verdict = tokio::spawn(revalidation_verdict(revalidation));
    let verdict_handle = verdict.abort_handle();
    let (code, revalidation) = tokio::select! {
        _ = tokio::io::copy_bidirectional(downstream, target) => {
            // The socket ended on its own; the token goes with the checker.
            verdict_handle.abort();
            return;
        }
        verdict = verdict => match verdict {
            Ok(verdict) => verdict,
            Err(_) => return,
        },
    };
    let reason = match code {
        TUNNEL_CLOSE_SESSION_ENDED => "session ended",
        TUNNEL_CLOSE_ACCESS_REVOKED => "access revoked",
        _ => "unverified",
    };
    tracing::info!(revalidation = ?revalidation, code, "closing tunnelled socket");
    drop(revalidation);
    let frame = websocket_close_frame(code, reason);
    let _ = tokio::time::timeout(TUNNEL_CLOSE_WRITE_TIMEOUT, downstream.write_all(&frame)).await;
    let _ = tokio::time::timeout(TUNNEL_CLOSE_WRITE_TIMEOUT, downstream.shutdown()).await;
    let _ = tokio::time::timeout(TUNNEL_CLOSE_WRITE_TIMEOUT, target.shutdown()).await;
}
