use super::*;

pub(in crate::api) async fn agent_list_machine_services(
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

pub(in crate::api) async fn agent_probe_machine_service(
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

pub(in crate::api) async fn agent_list_virtual_network_hosts(
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

pub(in crate::api) async fn agent_list_service_ingresses(
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
