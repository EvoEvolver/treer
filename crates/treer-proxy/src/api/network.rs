use super::*;

pub(super) async fn list_machine_services(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "services": auth.list_machine_services(&workspace_id).await?
    })))
}

pub(super) async fn create_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if let Some(target) = request.target_agent_id.as_deref() {
        let agent = state.resolve_agent(&workspace_id, target).await?;
        request.target_agent_id = Some(agent.agent_id);
        request.server_id = agent.server_id;
    } else {
        request.server_id = state
            .resolve_server(&workspace_id, &request.server_id)
            .await?
            .server_id;
    }
    let service = auth
        .create_machine_service(&workspace_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({ "service": service })))
}

pub(super) async fn update_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(mut request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if let Some(server_id) = request.server_id.as_deref() {
        request.server_id = Some(
            state
                .resolve_server(&workspace_id, server_id)
                .await?
                .server_id,
        );
    }
    let service = auth
        .update_machine_service(&workspace_id, &service_id, &session.user_id, request)
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "service": service })))
}

pub(super) async fn delete_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    let service = auth
        .delete_machine_service(&workspace_id, &current.service_id)
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({
        "deleted": true,
        "service_id": service.service_id,
        "name": service.name,
    })))
}

pub(super) async fn probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
                target_agent_id: service.target_agent_id.clone(),
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

pub(super) async fn list_virtual_network_hosts(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "hosts": auth.list_virtual_network_hosts(&workspace_id).await?
    })))
}

pub(super) async fn create_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let host = auth
        .create_virtual_network_host(&workspace_id, &session.user_id, request)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "host": host })))
}

pub(super) async fn delete_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.delete_virtual_network_host(&workspace_id, &hostname)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "deleted": true })))
}

pub(super) async fn list_service_ingresses(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let ingresses = auth
        .list_service_ingresses(&workspace_id)
        .await?
        .into_iter()
        .map(|ingress| service_ingress_json(&config, ingress))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({ "ingresses": ingresses })))
}

pub(super) async fn create_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(provider_store): Extension<crate::policy_provider_store::PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if request.access == ServiceIngressAccess::Public
        && provider_store
            .get(&workspace_id)
            .await
            .map_err(|error| {
                ApiFailure::internal("policy_provider_store_failed", &error.to_string())
            })?
            .is_some_and(|provider| provider.service_id == request.service_id)
    {
        return Err(ApiFailure::bad_request(
            "policy_provider_must_be_private",
            "the active Policy Provider service cannot have public ingress",
        ));
    }
    let ingress = auth
        .create_service_ingress(
            &workspace_id,
            &session.user_id,
            config.base_domain()?,
            request,
        )
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

pub(super) async fn update_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(provider_store): Extension<crate::policy_provider_store::PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if request.access == Some(ServiceIngressAccess::Public) {
        let current = auth
            .resolve_service_ingress(&workspace_id, &ingress_id)
            .await?;
        if provider_store
            .get(&workspace_id)
            .await
            .map_err(|error| {
                ApiFailure::internal("policy_provider_store_failed", &error.to_string())
            })?
            .is_some_and(|provider| provider.service_id == current.ingress.service_id)
        {
            return Err(ApiFailure::bad_request(
                "policy_provider_must_be_private",
                "the active Policy Provider service cannot have public ingress",
            ));
        }
    }
    let ingress = auth
        .update_service_ingress(&workspace_id, &ingress_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

pub(super) async fn delete_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress = auth
        .delete_service_ingress(&workspace_id, &ingress_id)
        .await?;
    Ok(Json(json!({
        "deleted": true,
        "ingress_id": ingress.ingress_id,
        "hostname": ingress.hostname,
    })))
}

pub(super) fn service_ingress_json(
    config: &IngressConfig,
    ingress: ServiceIngress,
) -> Result<Value, ApiFailure> {
    let public_url = config.url_for_hostname(&ingress.hostname)?.to_string();
    let mut value = serde_json::to_value(ingress)
        .map_err(|error| ApiFailure::internal("serialization_error", &error.to_string()))?;
    value
        .as_object_mut()
        .expect("service ingress serializes as an object")
        .insert("url".to_string(), Value::String(public_url));
    Ok(value)
}

#[derive(Debug, Deserialize)]
pub(super) struct IngressAuthorizeQuery {
    hostname: String,
    #[serde(default = "default_ingress_return_path")]
    return_path: String,
}

pub(super) fn default_ingress_return_path() -> String {
    "/".to_string()
}

pub(super) async fn authorize_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    Query(query): Query<IngressAuthorizeQuery>,
) -> Result<Response, ApiFailure> {
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

pub(super) async fn proxy_service_ingress(
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
            let authenticated = match session {
                Some(token) => auth
                    .authenticate_ingress_session(&hostname, &token)
                    .await?
                    .is_some(),
                None => false,
            };
            if !authenticated {
                return redirect_to_ingress_authorization(&config, &hostname, request.uri());
            }
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

pub(super) async fn complete_ingress_authorization(
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

pub(super) fn redirect_to_ingress_authorization(
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

pub(super) fn request_hostname(headers: &HeaderMap) -> Result<String, ApiFailure> {
    let authority = headers
        .get(header::HOST)
        .ok_or_else(|| ApiFailure::bad_request("host_required", "Host header is required"))?
        .to_str()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| ApiFailure::bad_request("invalid_host", "Host header is invalid"))?;
    Ok(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

pub(super) fn ingress_bearer_token(headers: &HeaderMap) -> Result<Option<&str>, ApiFailure> {
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

pub(super) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_string()))
}

pub(super) async fn proxy_virtual_network_host_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, String::new(), request).await
}

pub(super) async fn proxy_virtual_network_host_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, path, request).await
}

pub(super) async fn proxy_virtual_network_host(
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

pub(super) async fn proxy_agent_interface_ui_root(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, String::new(), request).await
}

pub(super) async fn proxy_agent_interface_ui_path(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_tunnel_if_present(request.headers())?;
    proxy_agent_interface_ui(state, workspace_id, agent_id, path, request).await
}

pub(super) async fn proxy_agent_interface_ui(
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
pub(super) async fn tunnel_http_request(
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

pub(super) fn sanitize_tunnel_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
) -> Result<(), ApiFailure> {
    headers.remove(header::COOKIE);
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::PROXY_AUTHORIZATION);
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

pub(super) fn sanitize_ingress_request_headers(
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
                item.split_once('=')
                    .is_none_or(|(name, _)| name != ingress_cookie_name)
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

pub(super) fn sanitize_tunnel_response_headers(headers: &mut HeaderMap, upgraded: bool) {
    headers.remove(header::SET_COOKIE);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
}

pub(super) fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
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

pub(super) async fn agent_network_publication_forbidden() -> ApiFailure {
    ApiFailure::forbidden(
        "managed_app_required",
        "Agents cannot manage services, virtual hosts, or ingresses; deploy an App or use the operator control plane",
    )
}

pub(super) async fn agent_list_machine_services(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_LIST,
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, "*"),
        ))
        .await?;
    list_machine_services(Extension(auth), Path(workspace_id)).await
}

pub(super) async fn agent_probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    require_agent_can_probe_service(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_PROBE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                service.target_agent_id.as_deref(),
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
                target_agent_id: service.target_agent_id.clone(),
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

pub(super) async fn agent_list_virtual_network_hosts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_VIRTUAL_HOST_LIST,
            PolicyResource::new(RESOURCE_VIRTUAL_HOST, "*"),
        ))
        .await?;
    list_virtual_network_hosts(Extension(auth), Path(workspace_id)).await
}

pub(super) async fn agent_list_service_ingresses(
    State(state): State<AppState>,
    Extension(api): Extension<ServiceIngressApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    api.policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_INGRESS_LIST,
            PolicyResource::new(RESOURCE_SERVICE_INGRESS, "*"),
        ))
        .await?;
    list_service_ingresses(
        Extension(api.auth),
        Extension(api.config),
        Path(workspace_id),
    )
    .await
}

pub(super) async fn publish_virtual_network_hosts(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<(), ApiFailure> {
    let snapshot = virtual_network_hosts_snapshot(auth, workspace_id).await?;
    state
        .broadcast_proxy_message(
            workspace_id,
            &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
        )
        .await;
    Ok(())
}

pub(super) async fn refresh_service_ingress_routes(auth: &AuthStore) -> Result<(), ApiFailure> {
    auth.refresh_service_ingresses()
        .await
        .map_err(|error| ApiFailure::internal("ingress_refresh_failed", &format!("{error:#}")))
}

pub fn spawn_network_metadata_refresh(state: AppState, auth: AuthStore) {
    tokio::spawn(async move {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(5));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh.tick().await;
        loop {
            refresh.tick().await;
            if let Err(error) = auth.refresh_virtual_network_hosts().await {
                tracing::warn!(%error, "failed to reload virtual hosts");
                continue;
            }
            if let Err(error) = auth.refresh_service_ingresses().await {
                tracing::warn!(%error, "failed to reload service ingresses");
            }
            let Ok(workspaces) = auth.all_workspaces().await else {
                tracing::warn!("failed to list workspaces for virtual-host refresh");
                continue;
            };
            for workspace in workspaces {
                match virtual_network_hosts_snapshot(&auth, &workspace.workspace_id).await {
                    Ok(snapshot) => {
                        state
                            .broadcast_proxy_message(
                                &workspace.workspace_id,
                                &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
                            )
                            .await;
                    }
                    Err(error) => {
                        let (_, error) = error.into_parts();
                        tracing::warn!(
                            workspace = %workspace.workspace_id,
                            message = %error.message,
                            "failed to refresh virtual hosts"
                        );
                    }
                }
            }
        }
    });
}

pub(crate) async fn virtual_network_hosts_snapshot(
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<VirtualNetworkHostsSnapshot, auth::AuthFailure> {
    auth.virtual_network_hosts_snapshot(workspace_id).await
}
