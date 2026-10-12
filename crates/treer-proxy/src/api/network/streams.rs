use super::*;

pub(in crate::api) async fn connect_machine_service(
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

pub(in crate::api) async fn agent_connect_machine_service(
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

pub(super) async fn upgrade_service_stream(
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

pub(super) async fn relay_service_websocket(socket: WebSocket, stream: tokio::io::DuplexStream) {
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

pub(super) async fn monitor_service_transfer<F: std::future::Future>(
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
