use super::*;

pub(super) fn public_tcp_ready() -> &'static std::sync::RwLock<std::collections::HashSet<u16>> {
    static READY: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<u16>>> =
        std::sync::OnceLock::new();
    READY.get_or_init(Default::default)
}

pub(super) struct PublicTcpReadyPort(u16);
impl Drop for PublicTcpReadyPort {
    fn drop(&mut self) {
        public_tcp_ready()
            .write()
            .expect("TCP ready lock")
            .remove(&self.0);
    }
}

pub(super) struct ServiceStreamCancellation(pub(super) Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for ServiceStreamCancellation {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

pub(super) async fn service_destination_identity_active(
    auth: &AuthStore,
    service: &MachineService,
) -> bool {
    if auth.authentication_disabled() {
        return true;
    }
    let active=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM machines m JOIN workspaces w ON w.workspace_id=m.workspace_id WHERE m.server_id=$1 AND m.workspace_id=$2 AND m.revoked_at IS NULL AND w.deleted_at IS NULL AND ($3::text IS NULL OR EXISTS(SELECT 1 FROM agent_credentials a WHERE a.agent_id=$3 AND a.server_id=m.server_id AND a.workspace_id=m.workspace_id AND a.revoked_at IS NULL)))")
        .bind(&service.server_id).bind(&service.workspace_id).bind(&service.target_agent_id).fetch_one(&auth.pool()).await;
    matches!(active, Ok(true))
}

pub(super) fn public_tcp_range() -> Result<(u16, u16), ApiFailure> {
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

/// The selected Policy Provider must stay behind workspace authentication; raw TCP
/// publication has no login, so it is refused at publish time, at provider
/// selection, and again at every accepted connection to close races.
pub(in crate::api) async fn is_active_policy_provider_service(
    pool: &sqlx::PgPool,
    workspace_id: &str,
    service_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_policy_providers WHERE workspace_id=$1 AND service_id=$2)")
        .bind(workspace_id)
        .bind(service_id)
        .fetch_one(pool)
        .await
}

pub(in crate::api) async fn publish_public_tcp_service(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiFailure> {
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
    if is_active_policy_provider_service(&pool, &workspace_id, &service.service_id)
        .await
        .map_err(|e| ApiFailure::internal("database_error", &e.to_string()))?
    {
        return Err(ApiFailure::bad_request(
            "policy_provider_must_be_private",
            "the active Policy Provider service cannot have public ingress",
        ));
    }
    let (start, end) = public_tcp_range()?;
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

pub(in crate::api) async fn agent_publish_public_tcp_service(
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

pub(in crate::api) async fn agent_unpublish_public_tcp_service(
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

pub(in crate::api) async fn unpublish_public_tcp_service(
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

pub(super) fn spawn_public_tcp_listeners(state: AppState, auth: AuthStore) {
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
                    if !matches!(
                        is_active_policy_provider_service(&auth.pool(), &workspace, &id).await,
                        Ok(false)
                    ) {
                        return;
                    }
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
                                    matches!(enabled,Ok(Some(true))) && matches!(current,Ok(ref value) if value == &service) && matches!(is_active_policy_provider_service(&auth.pool(),&workspace,&id).await, Ok(false)) && service_destination_identity_active(&auth,&service).await
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
