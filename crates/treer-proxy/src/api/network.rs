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

pub(super) async fn machine_restore_runtime(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    if headers.contains_key(AGENT_ID_HEADER) {
        return Err(ApiFailure::forbidden(
            "operator_required",
            "runtime restore requires the machine operator identity",
        ));
    }
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
    let target = state.resolve_server(&workspace_id, &server_id).await?;
    require_machine_target(Some(&subject), &target.server_id).map_err(|_| {
        ApiFailure::forbidden(
            "machine_restore_scope",
            "machine operator may restore only its authenticated machine",
        )
    })?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            "runtime.restore",
            PolicyResource::new(RESOURCE_MACHINE, &target.server_id),
        ))
        .await
        .map_err(|error| ApiFailure::forbidden(&error.code, &error.message))?;
    restore_machine_runtime(
        State(state),
        Extension(auth),
        Path((workspace_id, target.server_id)),
    )
    .await
}

pub(super) async fn restore_machine_runtime(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let server = state.resolve_server(&workspace_id, &server_id).await?;
    // Import legacy App declarations before the one Controller restore pass.
    // This never creates a second runtime identity or a second recovery owner.
    synchronize_app_lifecycles_for_server(
        state.clone(),
        auth.clone(),
        workspace_id.clone(),
        server.server_id.clone(),
    )
    .await;
    let mut report = state
        .send_command(
            &workspace_id,
            &server.server_id,
            AgentCommand::RestoreRuntime {},
        )
        .await?;
    let previous = auth
        .list_app_deployments_for_server(&workspace_id, &server.server_id)
        .await?;
    let items = report
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            ApiFailure::internal(
                "invalid_restore_report",
                "Controller returned an invalid restoration report",
            )
        })?;
    for service in auth
        .list_machine_services(&workspace_id)
        .await?
        .into_iter()
        .filter(|service| service.server_id == server.server_id)
    {
        let stopped = previous.iter().any(|app| {
            app.service_id == service.service_id
                && app.desired_state == treer_protocol::AppDesiredState::Stopped
        });
        if stopped {
            items.push(json!({"workload_id":service.service_id,"kind":"service","state":"skipped_stopped","message":"owning App is intentionally stopped"}));
            continue;
        }
        let probe = state.send_command(
            &workspace_id,
            &server.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                target_agent_id: service.target_agent_id.clone(),
                timeout_ms: 1000,
            },
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), probe).await;
        let (status,message)=match result {
            Ok(Ok(value)) if value.get("healthy").and_then(Value::as_bool) == Some(true) => ("attached", "registered endpoint accepts TCP connections".to_string()),
            Ok(Ok(value)) => ("waiting_endpoint",value.get("error").and_then(Value::as_str).unwrap_or("registered endpoint is not reachable; external processes are not supervised").to_string()),
            Ok(Err(error)) => ("waiting_endpoint",error.message),
            Err(_) => ("waiting_endpoint","endpoint probe timed out; retry restore after the endpoint starts".to_string()),
        };
        items.push(json!({"workload_id":service.service_id,"kind":"service","state":status,"message":message}));
    }
    Ok(Json(report))
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
    let _ = (&state, &auth, &session, &workspace_id, &request);
    Err(ApiFailure::bad_request(
        "virtual_hosts_retired",
        "register services by stable ID; platform assigns public domains",
    ))
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

pub(super) async fn agent_create_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if !headers.contains_key(AGENT_ID_HEADER) {
        let server_id = machine.server_id.as_deref().ok_or_else(|| {
            ApiFailure::forbidden(
                "machine_identity_required",
                "bound machine identity required",
            )
        })?;
        if (!request.server_id.is_empty() && request.server_id != server_id)
            || request.target_agent_id.is_some()
        {
            return Err(ApiFailure::forbidden(
                "service_not_owned",
                "machine may register only its own external endpoints",
            ));
        }
        request.server_id = server_id.to_owned();
        let subject = PolicySubject::Machine {
            server_id: server_id.to_owned(),
        };
        policy
            .authorize(&PolicyRequest::new(
                &workspace_id,
                subject,
                "service.create",
                PolicyResource::new(RESOURCE_MACHINE_SERVICE, "new")
                    .with_attribute("server_id", server_id),
            ))
            .await?;
        let service = auth
            .create_machine_service(&workspace_id, server_id, request)
            .await?;
        return Ok(Json(json!({"service":service})));
    }
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let PolicySubject::Agent {
        server_id,
        agent_id,
    } = &subject
    else {
        unreachable!()
    };
    if (!request.server_id.is_empty() && request.server_id != *server_id)
        || request
            .target_agent_id
            .as_ref()
            .is_some_and(|id| id != agent_id)
    {
        return Err(ApiFailure::forbidden(
            "service_not_owned",
            "service registration must use the authenticated Agent and machine",
        ));
    }
    request.server_id = server_id.clone();
    request.target_agent_id = Some(agent_id.clone());
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            "service.create",
            machine_service_policy_resource(
                "new",
                &request.name,
                server_id,
                Some(agent_id),
                &request.target_host,
                request.target_port,
            ),
        ))
        .await?;
    let service = auth
        .create_machine_service(&workspace_id, agent_id, request)
        .await?;
    Ok(Json(json!({"service": service})))
}

fn require_service_owner(
    subject: &PolicySubject,
    service: &MachineService,
) -> Result<(), ApiFailure> {
    if let PolicySubject::Machine { server_id } = subject {
        if service.server_id == *server_id {
            return Ok(());
        }
    }
    if let PolicySubject::Agent {
        server_id,
        agent_id,
    } = subject
    {
        if service.server_id == *server_id && service.target_agent_id.as_ref() == Some(agent_id) {
            return Ok(());
        }
    }
    Err(ApiFailure::forbidden(
        "service_not_owned",
        "only the owning Agent can modify this service",
    ))
}

async fn service_subject(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace: &str,
) -> Result<PolicySubject, ApiFailure> {
    control_policy_subject(state, Some(machine), headers, workspace)
        .await?
        .ok_or_else(|| {
            ApiFailure::forbidden(
                "service_identity_required",
                "authenticated identity required",
            )
        })
}

pub(super) async fn agent_update_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    if request
        .server_id
        .as_ref()
        .is_some_and(|id| id != &service.server_id)
    {
        return Err(ApiFailure::forbidden(
            "service_not_owned",
            "Agent cannot transfer a service to another machine",
        ));
    }
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            "service.update",
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service.service_id),
        ))
        .await?;
    let updated = auth
        .update_machine_service(
            &workspace_id,
            &service.service_id,
            service
                .target_agent_id
                .as_deref()
                .unwrap_or(&service.server_id),
            request,
        )
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    Ok(Json(json!({"service": updated})))
}

pub(super) async fn agent_delete_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            "service.delete",
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service.service_id),
        ))
        .await?;
    auth.delete_machine_service(&workspace_id, &service.service_id)
        .await?;
    refresh_service_ingress_routes(&auth).await?;
    Ok(Json(
        json!({"deleted": true, "service_id": service.service_id}),
    ))
}

pub(super) async fn agent_create_service_ingress(
    State(state): State<AppState>,
    Extension(api): Extension<ServiceIngressApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let ServiceIngressApi {
        auth,
        policy,
        config,
    } = api;
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &request.service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            "ingress.create",
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service.service_id),
        ))
        .await?;
    let ingress = auth
        .create_service_ingress(
            &workspace_id,
            service
                .target_agent_id
                .as_deref()
                .unwrap_or(&service.server_id),
            config.base_domain()?,
            request,
        )
        .await?;
    Ok(Json(
        json!({"ingress": service_ingress_json(&config, ingress)?}),
    ))
}

pub(super) async fn agent_update_service_ingress(
    State(state): State<AppState>,
    Extension(api): Extension<ServiceIngressApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace, ingress_id)): Path<(String, String)>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let ServiceIngressApi {
        auth,
        policy,
        config,
    } = api;
    let subject = service_subject(&state, &machine, &headers, &workspace).await?;
    let ingress = auth
        .list_service_ingresses(&workspace)
        .await?
        .into_iter()
        .find(|i| i.ingress_id == ingress_id)
        .ok_or_else(|| ApiFailure::bad_request("ingress_not_found", "ingress not found"))?;
    let service = auth
        .resolve_machine_service(&workspace, &ingress.service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace,
            subject,
            "ingress.update",
            PolicyResource::new(RESOURCE_SERVICE_INGRESS, &ingress_id),
        ))
        .await?;
    let updated = auth
        .update_service_ingress(&workspace, &ingress_id, &service.server_id, request)
        .await?;
    Ok(Json(
        json!({"ingress":service_ingress_json(&config,updated)?}),
    ))
}

pub(super) async fn agent_delete_service_ingress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace, ingress_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = service_subject(&state, &machine, &headers, &workspace).await?;
    let ingress = auth
        .list_service_ingresses(&workspace)
        .await?
        .into_iter()
        .find(|i| i.ingress_id == ingress_id)
        .ok_or_else(|| ApiFailure::bad_request("ingress_not_found", "ingress not found"))?;
    let service = auth
        .resolve_machine_service(&workspace, &ingress.service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace,
            subject,
            "ingress.delete",
            PolicyResource::new(RESOURCE_SERVICE_INGRESS, &ingress_id),
        ))
        .await?;
    auth.delete_service_ingress(&workspace, &ingress_id).await?;
    Ok(Json(json!({"deleted":true})))
}

pub(super) async fn connect_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    if service.service_id != service_id {
        return Err(ApiFailure::bad_request(
            "service_id_required",
            "forwarding requires the stable service ID",
        ));
    }
    upgrade_service_stream(state, auth, None, service, request).await
}

pub(super) async fn agent_connect_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let subject = service_subject(&state, &machine, request.headers(), &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    // Names are convenience selectors only. Policy and the live stream retain
    // the resolved stable service ID, just as for an explicit-ID request.
    let authorization = PolicyRequest::new(
        &workspace_id,
        subject,
        "service.connect",
        machine_service_policy_resource(
            &service.service_id,
            &service.name,
            &service.server_id,
            service.target_agent_id.as_deref(),
            &service.target_host,
            service.target_port,
        ),
    );
    policy.authorize(&authorization).await?;
    upgrade_service_stream(state, auth, Some((policy, authorization)), service, request).await
}

async fn upgrade_service_stream(
    state: AppState,
    auth: AuthStore,
    policy: Option<(PolicyEngine, PolicyRequest)>,
    service: MachineService,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    static CONNECTIONS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    if !service_destination_identity_active(&auth, &service).await {
        return Err(ApiFailure::forbidden(
            "service_identity_revoked",
            "service destination identity is not active",
        ));
    }
    let permit = CONNECTIONS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1024)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiFailure::service_unavailable(
                "service_connection_limit",
                "service forwarding connection limit reached",
            )
        })?;
    if service.protocol == treer_protocol::MachineServiceProtocol::Udp {
        return Err(ApiFailure::bad_request(
            "udp_not_supported",
            "explicit forwarding supports TCP and HTTP byte streams",
        ));
    }
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let headers = request.headers().clone();
    let ws = if websocket {
        use axum::extract::FromRequestParts;
        let (mut parts, body) = request.into_parts();
        let ws = WebSocketUpgrade::from_request_parts(&mut parts, &state)
            .await
            .map_err(|_| {
                ApiFailure::bad_request("invalid_websocket", "invalid service WebSocket handshake")
            })?;
        request = Request::from_parts(parts, body);
        Some(ws.max_message_size(64 * 1024).max_frame_size(64 * 1024))
    } else if headers.get(header::UPGRADE).and_then(|v| v.to_str().ok()) == Some("treer-tcp") {
        None
    } else {
        return Err(ApiFailure::bad_request(
            "upgrade_required",
            "use a service WebSocket or send Upgrade: treer-tcp",
        ));
    };
    let (source_server_id, source_agent_id) = match policy
        .as_ref()
        .map(|(_, authorization)| &authorization.subject)
    {
        Some(PolicySubject::Agent {
            server_id,
            agent_id,
        }) => (server_id.clone(), Some(agent_id.clone())),
        Some(PolicySubject::Machine { server_id }) => (server_id.clone(), None),
        _ => ("browser".into(), None),
    };
    let (mut stream, cancel) = state
        .open_client_network_stream_cancellable(
            &service.workspace_id,
            &service.server_id,
            treer_protocol::NetworkConnectRequest {
                source_server_id,
                source_agent_id,
                destination_agent_id: service.target_agent_id.clone(),
                host: service.target_host.clone(),
                port: service.target_port,
            },
            TrafficClass::ServiceIngress,
        )
        .await?;
    if let Some(ws) = ws {
        return Ok(ws.on_upgrade(move |socket| async move {
            let _permit = permit;
            let _cancel = ServiceStreamCancellation(Some(cancel));
            monitor_service_transfer(
                relay_service_websocket(socket, stream),
                auth,
                policy,
                service,
                headers,
            )
            .await;
        }));
    }
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let _permit = permit;
        let _cancel = ServiceStreamCancellation(Some(cancel));
        if let Ok(client) = upgrade.await {
            let mut client = TokioIo::new(client);
            monitor_service_transfer(
                tokio::io::copy_bidirectional(&mut client, &mut stream),
                auth,
                policy,
                service,
                headers,
            )
            .await;
        }
    });
    Ok(Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "treer-tcp")
        .body(Body::empty())
        .expect("static response"))
}

async fn relay_service_websocket(socket: WebSocket, stream: tokio::io::DuplexStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut outgoing, mut incoming) = socket.split();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let send = async {
        let mut buffer = vec![0; 32 * 1024];
        loop {
            let count = reader.read(&mut buffer).await.map_err(|_| ())?;
            if count == 0 {
                outgoing
                    .send(Message::Text(treer_protocol::SERVICE_STREAM_EOF.into()))
                    .await
                    .map_err(|_| ())?;
                return Ok::<(), ()>(());
            }
            outgoing
                .send(Message::Binary(buffer[..count].to_vec().into()))
                .await
                .map_err(|_| ())?;
        }
    };
    let receive = async {
        while let Some(message) = incoming.next().await {
            match message.map_err(|_| ())? {
                Message::Binary(bytes) => writer.write_all(&bytes).await.map_err(|_| ())?,
                Message::Text(text) if text == treer_protocol::SERVICE_STREAM_EOF => {
                    writer.shutdown().await.map_err(|_| ())?;
                    return Ok(());
                }
                Message::Ping(_) | Message::Pong(_) => {}
                _ => return Err(()),
            }
        }
        Err(())
    };
    let _ = tokio::try_join!(send, receive);
}

async fn monitor_service_transfer<F: std::future::Future>(
    transfer: F,
    auth: AuthStore,
    policy: Option<(PolicyEngine, PolicyRequest)>,
    service: MachineService,
    headers: HeaderMap,
) {
    tokio::pin!(transfer);
    let mut check = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = &mut transfer => break,
            _ = check.tick() => {
                let valid = async {
                    let current = auth.resolve_machine_service(&service.workspace_id,&service.service_id).await?;
                    if current != service || !service_destination_identity_active(&auth,&service).await { return Err(ApiFailure::forbidden("service_changed","service endpoint or destination identity changed")); }
                    if let Some((engine,authorization)) = &policy {
                        let machine = auth.authenticate_machine(&headers).await?;
                        auth.authenticate_agent(&machine,&headers).await?;
                        engine.authorize(authorization).await?;
                    } else {
                        let session = auth::authenticate_request(&auth,&headers).await?;
                        auth.require_workspace_member(&service.workspace_id,&session.user_id).await?;
                    }
                    Ok::<(),ApiFailure>(())
                };
                if !matches!(tokio::time::timeout(std::time::Duration::from_secs(5),valid).await,Ok(Ok(()))) { break; }
            }
        }
    }
}

pub(super) async fn agent_list_machine_services(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
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
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    if matches!(subject, PolicySubject::Agent { .. }) {
        require_agent_can_probe_service(&subject, &service)?;
    } else {
        require_machine_target(Some(&subject), &service.server_id)?;
    }
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
    let subject = service_subject(&state, &machine, &headers, &workspace_id).await?;
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
    spawn_public_tcp_listeners(state.clone(), auth.clone());
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

fn public_tcp_ready() -> &'static std::sync::RwLock<std::collections::HashSet<u16>> {
    static READY: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<u16>>> =
        std::sync::OnceLock::new();
    READY.get_or_init(Default::default)
}

struct PublicTcpReadyPort(u16);
impl Drop for PublicTcpReadyPort {
    fn drop(&mut self) {
        public_tcp_ready()
            .write()
            .expect("TCP ready lock")
            .remove(&self.0);
    }
}

struct ServiceStreamCancellation(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for ServiceStreamCancellation {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

async fn service_destination_identity_active(auth: &AuthStore, service: &MachineService) -> bool {
    if auth.authentication_disabled() {
        return true;
    }
    let active=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM machines m JOIN workspaces w ON w.workspace_id=m.workspace_id WHERE m.server_id=$1 AND m.workspace_id=$2 AND m.revoked_at IS NULL AND w.deleted_at IS NULL AND ($3::text IS NULL OR EXISTS(SELECT 1 FROM agent_credentials a WHERE a.agent_id=$3 AND a.server_id=m.server_id AND a.workspace_id=m.workspace_id AND a.revoked_at IS NULL)))")
        .bind(&service.server_id).bind(&service.workspace_id).bind(&service.target_agent_id).fetch_one(&auth.pool()).await;
    matches!(active, Ok(true))
}

fn public_tcp_range() -> Result<(u16, u16), ApiFailure> {
    let range = std::env::var("TREER_PUBLIC_TCP_PORT_RANGE").map_err(|_| {
        ApiFailure::service_unavailable(
            "public_tcp_not_configured",
            "configure TREER_PUBLIC_TCP_PORT_RANGE=start-end to enable public TCP",
        )
    })?;
    let parsed = range
        .split_once('-')
        .and_then(|(a, b)| Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()?)));
    match parsed {
        Some((start, end)) if start > 0 && end >= start && end - start < 1024 => Ok((start, end)),
        _ => Err(ApiFailure::service_unavailable(
            "invalid_public_tcp_range",
            "TCP range must contain 1 to 1024 nonzero ports",
        )),
    }
}

pub(super) async fn publish_public_tcp_service(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiFailure> {
    let (start, end) = public_tcp_range()?;
    if request.get("access").and_then(Value::as_str) != Some("public") {
        return Err(ApiFailure::bad_request("public_access_required", "raw public TCP requires explicit access=public; use authenticated forwarding for workspace access"));
    }
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    if service.protocol == treer_protocol::MachineServiceProtocol::Udp {
        return Err(ApiFailure::bad_request(
            "udp_not_supported",
            "UDP publication is not supported",
        ));
    }
    let pool = auth.pool();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock(74839201)")
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    let existing: Option<i32> =
        sqlx::query_scalar("SELECT public_port FROM public_tcp_services WHERE service_id=$1")
            .bind(&service.service_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    let port = if let Some(port) = existing {
        port
    } else {
        let available: Vec<i32> = public_tcp_ready()
            .read()
            .expect("TCP ready lock")
            .iter()
            .copied()
            .filter(|port| *port >= start && *port <= end)
            .map(i32::from)
            .collect();
        sqlx::query_scalar::<_, i32>("SELECT candidate FROM generate_series($1::integer,$2::integer) candidate WHERE candidate=ANY($3) AND NOT EXISTS (SELECT 1 FROM public_tcp_services WHERE public_port=candidate) ORDER BY candidate LIMIT 1")
            .bind(i32::from(start)).bind(i32::from(end)).bind(available).fetch_optional(&mut *tx).await.map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?
            .ok_or_else(|| ApiFailure::service_unavailable("public_tcp_exhausted", "configured public TCP port range is exhausted"))?
    };
    if port < i32::from(start) || port > i32::from(end) {
        return Err(ApiFailure::service_unavailable(
            "public_tcp_range_changed",
            "the service's stable port is outside the configured range",
        ));
    }
    if !public_tcp_ready()
        .read()
        .expect("TCP ready lock")
        .contains(&(port as u16))
    {
        return Err(ApiFailure::service_unavailable(
            "public_tcp_listener_unavailable",
            "the assigned platform TCP listener is not available",
        ));
    }
    let host = config
        .proxy_public_url
        .host_str()
        .unwrap_or("localhost")
        .to_owned();
    sqlx::query("INSERT INTO public_tcp_services(service_id,public_port,enabled) VALUES($1,$2,TRUE) ON CONFLICT(service_id) DO UPDATE SET enabled=TRUE").bind(&service.service_id).bind(port).execute(&mut *tx).await.map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    Ok(Json(
        json!({"service_id": service.service_id, "access": "public", "hostname": host, "port": port}),
    ))
}

pub(super) async fn agent_publish_public_tcp_service(
    State(state): State<AppState>,
    Extension(api): Extension<ServiceIngressApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace, service_id)): Path<(String, String)>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiFailure> {
    let ServiceIngressApi {
        auth,
        policy,
        config,
    } = api;
    let subject = service_subject(&state, &machine, &headers, &workspace).await?;
    let service = auth
        .resolve_machine_service(&workspace, &service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace,
            subject,
            "ingress.create",
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service.service_id),
        ))
        .await?;
    publish_public_tcp_service(
        Extension(auth),
        Extension(config),
        Path((workspace, service_id)),
        Json(request),
    )
    .await
}

pub(super) async fn agent_unpublish_public_tcp_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = service_subject(&state, &machine, &headers, &workspace).await?;
    let service = auth
        .resolve_machine_service(&workspace, &service_id)
        .await?;
    require_service_owner(&subject, &service)?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace,
            subject,
            "ingress.delete",
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service_id),
        ))
        .await?;
    unpublish_public_tcp_service(Extension(auth), Path((workspace, service_id))).await
}

pub(super) async fn unpublish_public_tcp_service(
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    sqlx::query("UPDATE public_tcp_services SET enabled=FALSE WHERE service_id=$1")
        .bind(&service.service_id)
        .execute(&auth.pool())
        .await
        .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?;
    Ok(Json(
        json!({"service_id":service.service_id,"enabled":false}),
    ))
}

fn spawn_public_tcp_listeners(state: AppState, auth: AuthStore) {
    let Ok((start, end)) = public_tcp_range() else {
        return;
    };
    let bind_host =
        std::env::var("TREER_PUBLIC_TCP_BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0".into());
    for port in start..=end {
        let state = state.clone();
        let auth = auth.clone();
        let bind_host = bind_host.clone();
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind((bind_host.as_str(), port)).await {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::error!(%error,port,"public TCP listener unavailable");
                    return;
                }
            };
            let limit = Arc::new(tokio::sync::Semaphore::new(64));
            public_tcp_ready()
                .write()
                .expect("TCP ready lock")
                .insert(port);
            let _ready = PublicTcpReadyPort(port);
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    break;
                };
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    continue;
                };
                let state = state.clone();
                let auth = auth.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let row: Result<Option<(String,String)>,_> = sqlx::query_as("SELECT s.workspace_id,s.service_id FROM public_tcp_services p JOIN machine_services s USING(service_id) JOIN workspaces w ON w.workspace_id=s.workspace_id WHERE p.public_port=$1 AND p.enabled=TRUE AND w.deleted_at IS NULL").bind(i32::from(port)).fetch_optional(&auth.pool()).await;
                    let Ok(Some((workspace, id))) = row else {
                        return;
                    };
                    let Ok(service) = auth.resolve_machine_service(&workspace, &id).await else {
                        return;
                    };
                    if !service_destination_identity_active(&auth, &service).await {
                        return;
                    }
                    let Ok((mut stream, cancel)) = state
                        .open_browser_network_stream_cancellable(
                            &workspace,
                            &service.server_id,
                            service.target_agent_id.as_deref(),
                            &service.target_host,
                            service.target_port,
                            TrafficClass::ServiceIngress,
                        )
                        .await
                    else {
                        return;
                    };
                    let _cancel = ServiceStreamCancellation(Some(cancel));
                    let transfer = tokio::io::copy_bidirectional(&mut client, &mut stream);
                    tokio::pin!(transfer);
                    let mut check = tokio::time::interval(std::time::Duration::from_secs(1));
                    loop {
                        tokio::select! {
                            _ = &mut transfer => break,
                            _ = check.tick() => {
                                let valid = async {
                                    let current = auth.resolve_machine_service(&workspace,&id).await;
                                    let enabled = sqlx::query_scalar::<_,bool>("SELECT p.enabled FROM public_tcp_services p JOIN machine_services s USING(service_id) JOIN workspaces w ON w.workspace_id=s.workspace_id WHERE p.service_id=$1 AND w.deleted_at IS NULL").bind(&id).fetch_optional(&auth.pool()).await;
                                    matches!(enabled,Ok(Some(true))) && matches!(current,Ok(ref value) if value == &service) && service_destination_identity_active(&auth,&service).await
                                };
                                if !matches!(tokio::time::timeout(std::time::Duration::from_secs(5),valid).await,Ok(true)) { break; }
                            }
                        }
                    }
                });
            }
        });
    }
}

pub(crate) async fn virtual_network_hosts_snapshot(
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<VirtualNetworkHostsSnapshot, auth::AuthFailure> {
    auth.virtual_network_hosts_snapshot(workspace_id).await
}

#[cfg(test)]
mod explicit_service_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use treer_protocol::{NetworkBinaryFrame, NetworkBinaryKind};

    async fn relay_fixture(
        workspace: &str,
    ) -> (
        AuthStore,
        AppState,
        MachineService,
        String,
        MachineSession,
        tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
        uuid::Uuid,
    ) {
        let auth = AuthStore::for_test("owner-password").await;
        auth.seed_test_workspace(workspace).await;
        let enrollment = auth
            .create_machine_enrollment(workspace, "owner")
            .await
            .unwrap();
        let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
        let state = AppState::new();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let connection = uuid::Uuid::new_v4();
        let build = treer_protocol::BuildInfo {
            version: "test".into(),
            git_commit: "test".into(),
        };
        state
            .register_server(
                treer_protocol::ServerInfo {
                    server_id: machine.server_id.clone(),
                    workspace_id: workspace.into(),
                    name: "test".into(),
                    hostname: "test".into(),
                    root: "/tmp".into(),
                    controller_build: build.clone(),
                    host_build: build,
                    supervision: None,
                    labels: Default::default(),
                    available_agents: None,
                    status: treer_protocol::ServerStatus::Online,
                    connected_at: Utc::now(),
                    last_seen_at: Utc::now(),
                },
                connection,
                sender,
            )
            .await
            .unwrap();
        let service = auth
            .create_machine_service(
                workspace,
                "owner",
                CreateMachineServiceRequest {
                    name: "echo".into(),
                    server_id: machine.server_id.clone(),
                    target_agent_id: None,
                    primary: false,
                    target_host: "127.0.0.1".into(),
                    target_port: 3000,
                    protocol: treer_protocol::MachineServiceProtocol::Tcp,
                },
            )
            .await
            .unwrap();
        let session = MachineSession {
            server_id: Some(machine.server_id),
            workspace_id: Some(workspace.into()),
        };
        (
            auth,
            state,
            service,
            machine.machine_token,
            session,
            receiver,
            connection,
        )
    }

    async fn next_frame(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    ) -> NetworkBinaryFrame {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap()
            {
                SocketFrame::Binary(data) => return NetworkBinaryFrame::decode(&data).unwrap(),
                _ => continue,
            }
        }
    }

    async fn relay(
        state: &AppState,
        service: &MachineService,
        connection: uuid::Uuid,
        id: &str,
        kind: NetworkBinaryKind,
        payload: &[u8],
    ) {
        state
            .relay_network_frame(
                &service.workspace_id,
                &service.server_id,
                connection,
                NetworkBinaryFrame {
                    kind,
                    stream_id: id.into(),
                    payload: payload.to_vec(),
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn machine_restore_route_authenticates_scopes_and_reports_waiting_endpoints() {
        use crate::policy::{PolicyDecision, PolicyDenial};
        use tower::ServiceExt;
        let (auth, state, service, token, _session, mut receiver, _connection) =
            relay_fixture("restore-route").await;
        auth.seed_test_workspace("other-workspace").await;
        let mut other = state
            .resolve_server(&service.workspace_id, &service.server_id)
            .await
            .unwrap();
        other.server_id = "different-machine".into();
        other.name = "different-machine".into();
        let (other_sender, _other_receiver) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(other, uuid::Uuid::new_v4(), other_sender)
            .await
            .unwrap();
        let url = Url::parse("https://proxy.example/").unwrap();
        let browser_url = Url::parse("https://app.example/").unwrap();
        let bootstrap = BootstrapConfig::new(
            url.clone(),
            PathBuf::from("dist"),
            Url::parse("https://example.test/releases/").unwrap(),
        );
        let identity = IdentityIssuer::load(&auth, &url).await.unwrap();
        let messages = MessageStore::open(auth.pool()).await.unwrap();
        let browser = BrowserAccess::new(&browser_url, &url).unwrap();
        let ingress = IngressConfig::new(None, &url, &browser_url).unwrap();
        let make_router = |policy| {
            router(
                state.clone(),
                bootstrap.clone(),
                auth.clone(),
                policy,
                identity.clone(),
                browser.clone(),
                ingress.clone(),
                messages.clone(),
                CapabilityRollout::all_enabled(),
                crate::updater::UpdaterClient::disabled(),
                crate::voice::VoiceServices::disabled(),
            )
        };
        let app = make_router(PolicyEngine::allow_all());
        let path = format!(
            "/agent/workspaces/{}/machines/{}/restore",
            service.workspace_id, service.server_id
        );
        let request = |path: &str, authenticated: bool| {
            let mut builder = Request::builder().method("POST").uri(path);
            if authenticated {
                builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request(&path, false))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong_workspace = format!(
            "/agent/workspaces/other-workspace/machines/{}/restore",
            service.server_id
        );
        assert_eq!(
            app.clone()
                .oneshot(request(&wrong_workspace, true))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let wrong_machine = format!(
            "/agent/workspaces/{}/machines/different-machine/restore",
            service.workspace_id
        );
        assert_eq!(
            app.clone()
                .oneshot(request(&wrong_machine, true))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let credential = auth
            .create_agent_credential(&service.workspace_id, &service.server_id, "agent-restore")
            .await
            .unwrap();
        let agent_request = Request::builder()
            .method("POST")
            .uri(&path)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(AGENT_ID_HEADER, "agent-restore")
            .header(treer_protocol::WORKLOAD_CREDENTIAL_HEADER, credential)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(agent_request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let denied = make_router(PolicyEngine::new(
            PolicyDecision::Deny(PolicyDenial::new("restore_denied", "restore denied")),
            vec![],
        ));
        assert_eq!(
            denied.oneshot(request(&path, true)).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert!(
            receiver.try_recv().is_err(),
            "denied requests must not send Controller commands"
        );
        let responder_state = state.clone();
        let workspace = service.workspace_id.clone();
        let server_id = service.server_id.clone();
        let responder = tokio::spawn(async move {
            let mut actions = Vec::new();
            while actions.len() < 2 {
                let frame =
                    tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
                        .await
                        .unwrap()
                        .unwrap();
                let SocketFrame::Text(encoded) = frame else {
                    continue;
                };
                let treer_protocol::ProxyMessage::Command { envelope } =
                    serde_json::from_str(&encoded).unwrap()
                else {
                    continue;
                };
                let value = match envelope.command {
                    AgentCommand::RestoreRuntime {} => {
                        actions.push("restore");
                        json!({"workspace_id":workspace,"server_id":server_id,"items":[]})
                    }
                    AgentCommand::ProbeNetwork { .. } => {
                        actions.push("probe");
                        json!({"healthy":false,"error":"external process has not started"})
                    }
                    command => panic!("unexpected command {command:?}"),
                };
                responder_state
                    .complete_command(treer_protocol::CommandResult::success(
                        envelope.command_id,
                        value,
                    ))
                    .await;
            }
            actions
        });
        let response = app.oneshot(request(&path, true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let report: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(report["items"][0]["workload_id"], service.service_id);
        assert_eq!(report["items"][0]["state"], "waiting_endpoint");
        assert_eq!(responder.await.unwrap(), vec!["restore", "probe"]);
    }

    #[tokio::test]
    async fn private_service_socket_by_name_uses_stable_policy_id_and_revocation() {
        private_service_transport_revocation(false).await;
    }

    #[tokio::test]
    async fn private_service_websocket_preserves_half_close_and_policy_revocation() {
        private_service_transport_revocation(true).await;
    }

    async fn private_service_transport_revocation(websocket: bool) {
        use crate::policy::{
            PolicyDecision, PolicyDenial, PolicyEvaluation, PolicyEvaluator, PolicyFuture,
        };
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Revocable(Arc<AtomicBool>, String);
        impl PolicyEvaluator for Revocable {
            fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a> {
                Box::pin(async move {
                    assert_eq!(request.resource.id, self.1);
                    Ok(PolicyEvaluation::Decide(if self.0.load(Ordering::SeqCst) {
                        PolicyDecision::Deny(PolicyDenial::new("revoked", "revoked"))
                    } else {
                        PolicyDecision::Allow
                    }))
                })
            }
        }
        let (auth, state, service, token, session, mut receiver, connection) =
            relay_fixture("private-socket").await;
        let revoked = Arc::new(AtomicBool::new(false));
        let policy = PolicyEngine::new(
            PolicyDecision::Allow,
            vec![Arc::new(Revocable(
                revoked.clone(),
                service.service_id.clone(),
            ))],
        );
        let app = Router::new()
            .route(
                "/workspaces/{workspace_id}/services/{service_id}/connect",
                get(agent_connect_machine_service).post(agent_connect_machine_service),
            )
            .layer(Extension(auth))
            .layer(Extension(policy))
            .layer(Extension(session))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let url = format!(
            "http://{address}/workspaces/{}/services/{}/connect",
            service.workspace_id,
            service.name.to_uppercase()
        );
        let request = tokio::spawn(async move {
            if websocket {
                use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                let mut request = url
                    .replacen("http://", "ws://", 1)
                    .into_client_request()
                    .unwrap();
                request
                    .headers_mut()
                    .insert("authorization", format!("Bearer {token}").parse().unwrap());
                Ok(tokio_tungstenite::connect_async(request).await.unwrap().0)
            } else {
                Err(reqwest::Client::new()
                    .post(url)
                    .bearer_auth(token)
                    .header("connection", "upgrade")
                    .header("upgrade", "treer-tcp")
                    .send()
                    .await
                    .unwrap())
            }
        });
        let open = next_frame(&mut receiver).await;
        assert_eq!(open.kind, NetworkBinaryKind::Open);
        let connect: treer_protocol::NetworkConnectRequest =
            serde_json::from_slice(&open.payload).unwrap();
        assert_eq!(connect.source_server_id, service.server_id);
        relay(
            &state,
            &service,
            connection,
            &open.stream_id,
            NetworkBinaryKind::Opened,
            &[],
        )
        .await;
        let response = match request.await.unwrap() {
            Ok(mut client) => {
                use tokio_tungstenite::tungstenite::Message as WireMessage;
                client
                    .send(WireMessage::Binary(b"ping".to_vec().into()))
                    .await
                    .unwrap();
                assert_eq!(next_frame(&mut receiver).await.payload, b"ping");
                client
                    .send(WireMessage::Text(treer_protocol::SERVICE_STREAM_EOF.into()))
                    .await
                    .unwrap();
                let half_close = next_frame(&mut receiver).await;
                assert_eq!(half_close.kind, NetworkBinaryKind::HalfClose);
                relay(
                    &state,
                    &service,
                    connection,
                    &open.stream_id,
                    NetworkBinaryKind::Data,
                    b"pong",
                )
                .await;
                assert_eq!(
                    client.next().await.unwrap().unwrap(),
                    WireMessage::Binary(b"pong".to_vec().into())
                );
                revoked.store(true, Ordering::SeqCst);
                let closed = tokio::time::timeout(std::time::Duration::from_secs(3), client.next())
                    .await
                    .unwrap();
                assert!(matches!(
                    closed,
                    None | Some(Err(_)) | Some(Ok(WireMessage::Close(_)))
                ));
                let reset = loop {
                    let frame = next_frame(&mut receiver).await;
                    if frame.kind == NetworkBinaryKind::Reset {
                        break frame;
                    }
                };
                assert_eq!(reset.stream_id, open.stream_id);
                server.abort();
                return;
            }
            Err(response) => response,
        };
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let mut client = response.upgrade().await.unwrap();
        client.write_all(b"ping").await.unwrap();
        assert_eq!(next_frame(&mut receiver).await.payload, b"ping");
        revoked.store(true, Ordering::SeqCst);
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), client.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        let reset = loop {
            let frame = next_frame(&mut receiver).await;
            if frame.kind == NetworkBinaryKind::Reset {
                break frame;
            }
        };
        assert_eq!(reset.stream_id, open.stream_id);
        server.abort();
    }

    #[test]
    fn agent_service_mutations_require_both_bound_machine_and_agent() {
        let service = MachineService {
            service_id: "svc_test".into(),
            workspace_id: "workspace".into(),
            name: "preview".into(),
            server_id: "machine-a".into(),
            target_agent_id: Some("agent-a".into()),
            primary: false,
            target_host: "127.0.0.1".into(),
            target_port: 3000,
            protocol: treer_protocol::MachineServiceProtocol::Http,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: "agent-a".into(),
            updated_by: "agent-a".into(),
        };
        assert!(require_service_owner(
            &PolicySubject::Agent {
                server_id: "machine-a".into(),
                agent_id: "agent-a".into()
            },
            &service
        )
        .is_ok());
        for (machine, agent) in [("machine-a", "agent-b"), ("machine-b", "agent-a")] {
            assert!(require_service_owner(
                &PolicySubject::Agent {
                    server_id: machine.into(),
                    agent_id: agent.into()
                },
                &service
            )
            .is_err());
        }
        assert!(require_service_owner(
            &PolicySubject::Machine {
                server_id: "machine-b".into()
            },
            &service
        )
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires isolated database and an explicitly selected public TCP range"]
    async fn public_tcp_allocation_is_stable_and_explicitly_revocable() {
        let (auth, state, service, _token, _session, mut receiver, connection) =
            relay_fixture("tcp-test").await;
        spawn_public_tcp_listeners(state.clone(), auth.clone());
        let (start, _) = public_tcp_range().unwrap();
        for _ in 0..20 {
            if public_tcp_ready().read().unwrap().contains(&start) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let config = IngressConfig::new(
            None,
            &Url::parse("https://proxy.example/").unwrap(),
            &Url::parse("https://app.example/").unwrap(),
        )
        .unwrap();
        let publish = || {
            publish_public_tcp_service(
                Extension(auth.clone()),
                Extension(config.clone()),
                Path(("tcp-test".into(), service.service_id.clone())),
                Json(json!({"access":"public"})),
            )
        };
        let first = publish().await.unwrap().0;
        let second = publish().await.unwrap().0;
        assert_eq!(first["port"], second["port"]);
        assert_eq!(first["hostname"], "proxy.example");
        let _ = unpublish_public_tcp_service(
            Extension(auth.clone()),
            Path(("tcp-test".into(), service.service_id.clone())),
        )
        .await
        .unwrap();
        let enabled: bool =
            sqlx::query_scalar("SELECT enabled FROM public_tcp_services WHERE service_id=$1")
                .bind(&service.service_id)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
        assert!(!enabled);
        assert_eq!(publish().await.unwrap().0["port"], first["port"]);
        let port = first["port"].as_u64().unwrap() as u16;
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let open = next_frame(&mut receiver).await;
        relay(
            &state,
            &service,
            connection,
            &open.stream_id,
            NetworkBinaryKind::Opened,
            &[],
        )
        .await;
        client.write_all(b"request").await.unwrap();
        assert_eq!(next_frame(&mut receiver).await.payload, b"request");
        client.shutdown().await.unwrap();
        assert_eq!(
            next_frame(&mut receiver).await.kind,
            NetworkBinaryKind::HalfClose
        );
        relay(
            &state,
            &service,
            connection,
            &open.stream_id,
            NetworkBinaryKind::Data,
            b"response after EOF",
        )
        .await;
        relay(
            &state,
            &service,
            connection,
            &open.stream_id,
            NetworkBinaryKind::HalfClose,
            &[],
        )
        .await;
        let mut data = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.read_to_end(&mut data),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(data, b"response after EOF");
        // Drain the completed stream's acknowledgements before opening another.
        while let Ok(SocketFrame::Binary(_)) = receiver.try_recv() {}
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let open = loop {
            let frame = next_frame(&mut receiver).await;
            if frame.kind == NetworkBinaryKind::Open {
                break frame;
            }
        };
        relay(
            &state,
            &service,
            connection,
            &open.stream_id,
            NetworkBinaryKind::Opened,
            &[],
        )
        .await;
        let _ = unpublish_public_tcp_service(
            Extension(auth.clone()),
            Path(("tcp-test".into(), service.service_id.clone())),
        )
        .await
        .unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), client.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
