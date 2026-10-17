use super::*;

pub(in crate::api) async fn agent_network_publication_forbidden() -> ApiFailure {
    ApiFailure::forbidden(
        "managed_app_required",
        "Agents cannot manage services, virtual hosts, or ingresses; deploy an App or use the operator control plane",
    )
}

pub(in crate::api) async fn agent_create_machine_service(
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

pub(super) fn require_service_owner(
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

pub(super) async fn service_subject(
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

pub(in crate::api) async fn agent_update_machine_service(
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

pub(in crate::api) async fn agent_delete_machine_service(
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

pub(in crate::api) async fn agent_create_service_ingress(
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

pub(in crate::api) async fn agent_update_service_ingress(
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
        .resolve_machine_service(&workspace, ingress_service_id(&ingress)?)
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

pub(in crate::api) async fn agent_delete_service_ingress(
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
        .resolve_machine_service(&workspace, ingress_service_id(&ingress)?)
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

/// Agent-managed ingress operations act on service ingresses only; an Agent
/// interface origin (#143 Phase 2) has no machine service behind it.
fn ingress_service_id(ingress: &treer_protocol::ServiceIngress) -> Result<&str, ApiFailure> {
    ingress.service_id.as_deref().ok_or_else(|| ApiFailure {
        status: axum::http::StatusCode::CONFLICT,
        error: treer_protocol::ProtocolError::new(
            "ingress_target_not_switchable",
            "an Agent interface origin has no machine service",
        ),
    })
}
