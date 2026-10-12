use super::*;

pub(in crate::api) async fn list_machine_services(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "services": auth.list_machine_services(&workspace_id).await?
    })))
}

pub(in crate::api) async fn create_machine_service(
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

pub(in crate::api) async fn update_machine_service(
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

pub(in crate::api) async fn delete_machine_service(
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

pub(in crate::api) async fn probe_machine_service(
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
