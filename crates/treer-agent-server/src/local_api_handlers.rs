use super::*;
pub(super) async fn exec_machine(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<MachineExecRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .post_as(
                &format!("machines/{server_id}/exec"),
                &body,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn upload_machine_file(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<UploadMachineFileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .post_as(
                &format!("machines/{server_id}/files"),
                &body,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn health(State(state): State<LocalApiState>) -> Json<Value> {
    let proxy = state.runtime.proxy_link_status();
    Json(json!({
        "status": "ok",
        "service": "treer-agent-server",
        "workspace_id": state.workspace_id,
        "server_id": state.server_id,
        "controller_epoch": state.controller_epoch,
        "controller_build": BuildInfo {
            version: treer_build_info::VERSION.to_string(),
            git_commit: treer_build_info::GIT_COMMIT.to_string(),
        },
        "host_build": state.host_build,
        "proxy_connected": proxy.connected,
        "proxy_last_error": proxy.last_error,
        "proxy_last_error_code": proxy.last_error_code,
        "connection_state": proxy.connection_state(),
    }))
}

pub(super) async fn discovery(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("snapshot", source_agent.as_ref()).await?))
}

pub(super) async fn issue_identity_token(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<WorkloadIdentityTokenRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .post_as("identity/token", &body, Some(&agent_id))
            .await?,
    ))
}

pub(super) async fn list_humans(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("humans", Some(&agent_id)).await?))
}

pub(super) async fn send_core_message(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<SendMessageRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(state.post_as("messages", &body, Some(&agent)).await?))
}

pub(super) async fn get_core_message(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(message_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(
                &format!("messages/{}", encode_path_segment(&message_id)),
                Some(&agent),
            )
            .await?,
    ))
}

pub(super) async fn list_core_messages(
    State(state): State<LocalApiState>,
    Query(query): Query<ListMessagesQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let suffix = encode_message_list_suffix(&query);
    Ok(Json(state.get_as(&suffix, Some(&agent)).await?))
}

pub(super) fn encode_message_list_suffix(query: &ListMessagesQuery) -> String {
    let mut encoded = url::form_urlencoded::Serializer::new(String::new());
    encoded.append_pair("limit", &query.limit.to_string());
    if let Some(before) = &query.before {
        encoded.append_pair("before", before);
    }
    format!("messages?{}", encoded.finish())
}

pub(super) async fn receive_core_messages(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<ReceiveMessagesRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .post_as("messages/receive", &body, Some(&agent))
            .await?,
    ))
}

pub(super) async fn acknowledge_core_messages(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<AcknowledgeMessagesRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state.post_as("messages/ack", &body, Some(&agent)).await?,
    ))
}

pub(super) async fn import_core_messages(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<ImportMessagesRequest>,
) -> Result<Json<Value>, LocalApiError> {
    if validated_source_agent(&state, &headers)?.is_some() {
        return Err(LocalApiError::unauthorized(ProtocolError::new(
            "message_import_denied",
            "message import requires a local operator",
        )));
    }
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(state.post_as("messages/import", &body, None).await?))
}

pub(super) fn encode_path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub(super) async fn list_agents(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("agents", source_agent.as_ref()).await?))
}

pub(super) async fn list_local_agents(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    authenticate_operator(&state, &headers)?;
    Ok(Json(json!({ "agents": state.runtime.list() })))
}

pub(super) async fn list_machine_services(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("services", Some(&agent_id)).await?))
}

pub(super) async fn create_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "services",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

pub(super) async fn update_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
    Json(request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("services/{service_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

pub(super) async fn delete_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("services/{service_id}"), Some(&agent_id))
            .await?,
    ))
}

pub(super) async fn probe_machine_service(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(service_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("services/{service_id}/probe"),
                &json!({}),
                Some(&agent_id),
            )
            .await?,
    ))
}

pub(super) async fn get_agent_interface(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        serde_json::to_value(
            state
                .runtime
                .interface(&agent.agent_id)
                .map_err(LocalApiError::bad_request_protocol)?,
        )
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
    ))
}

pub(super) async fn register_agent_interface(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<RegisterAgentInterfaceRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let descriptor = state
        .runtime
        .register_interface(&agent.agent_id, request)
        .await
        .map_err(LocalApiError::bad_request_protocol)?;
    Ok(Json(serde_json::to_value(descriptor).map_err(|error| {
        LocalApiError::bad_request(error.to_string())
    })?))
}

pub(super) async fn clear_agent_interface(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let descriptor = state
        .runtime
        .clear_interface(&agent.agent_id)
        .map_err(LocalApiError::bad_request_protocol)?;
    Ok(Json(json!({ "removed": descriptor })))
}

pub(super) async fn list_virtual_network_hosts(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("virtual-hosts", Some(&agent_id)).await?))
}

pub(super) async fn create_virtual_network_host(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "virtual-hosts",
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                Some(&agent_id),
            )
            .await?,
    ))
}

pub(super) async fn delete_virtual_network_host(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(hostname): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent_id = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("virtual-hosts/{hostname}"), Some(&agent_id))
            .await?,
    ))
}

pub(super) async fn list_service_ingresses(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("ingresses", Some(&agent)).await?))
}

pub(super) async fn create_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "ingresses",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent),
            )
            .await?,
    ))
}

pub(super) async fn update_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(ingress_id): Path<String>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("ingresses/{ingress_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                Some(&agent),
            )
            .await?,
    ))
}

pub(super) async fn delete_service_ingress(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(ingress_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("ingresses/{ingress_id}"), Some(&agent))
            .await?,
    ))
}

pub(super) fn source_agent_id(headers: &HeaderMap) -> Result<&str, LocalApiError> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| LocalApiError::bad_request("invalid agent identity".to_string()))
        })
        .transpose()
        .map(|value| value.filter(|value| !value.is_empty()))?;
    agent_id
        .ok_or_else(|| LocalApiError::bad_request("managed agent identity is required".to_string()))
}

pub(super) fn workload_identity(headers: &HeaderMap) -> Result<(&str, &str), LocalApiError> {
    let agent_id = source_agent_id(headers)?;
    let credential = headers
        .get(WORKLOAD_CREDENTIAL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LocalApiError::unauthorized(ProtocolError::new(
                "workload_credential_required",
                "managed agent workload credential is required",
            ))
        })?;
    Ok((agent_id, credential))
}

pub(super) fn optional_workload_identity(
    headers: &HeaderMap,
) -> Result<Option<(&str, &str)>, LocalApiError> {
    let has_agent_id = headers.contains_key(AGENT_ID_HEADER);
    let has_credential = headers.contains_key(WORKLOAD_CREDENTIAL_HEADER);
    match (has_agent_id, has_credential) {
        (false, false) => Ok(None),
        (true, true) => workload_identity(headers).map(Some),
        (true, false) => Err(LocalApiError::unauthorized(ProtocolError::new(
            "workload_credential_required",
            "managed agent workload credential is required",
        ))),
        (false, true) => Err(LocalApiError::unauthorized(ProtocolError::new(
            "agent_identity_required",
            "managed agent identity is required with a workload credential",
        ))),
    }
}

#[derive(Clone, Debug)]
pub(super) struct ValidatedAgent {
    pub(super) agent_id: String,
    pub(super) workload_credential: String,
}

pub(super) fn authenticate_operator(
    state: &LocalApiState,
    headers: &HeaderMap,
) -> Result<(), LocalApiError> {
    let expected = state
        .operator_credential
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(operator_auth_required)?;
    if !operator_credential_matches(expected, headers) {
        return Err(operator_auth_required());
    }
    Ok(())
}

pub(super) fn operator_credential_matches(expected: &str, headers: &HeaderMap) -> bool {
    let supplied = headers
        .get(OPERATOR_CREDENTIAL_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    !expected.is_empty()
        && expected.len() == supplied.len()
        && expected.as_bytes().ct_eq(supplied.as_bytes()).unwrap_u8() == 1
}

pub(super) fn operator_auth_required() -> LocalApiError {
    LocalApiError::unauthorized(ProtocolError::new(
        "operator_authentication_required",
        "local operator credential is required",
    ))
}

pub(super) fn validated_source_agent(
    state: &LocalApiState,
    headers: &HeaderMap,
) -> Result<Option<ValidatedAgent>, LocalApiError> {
    let Some((agent_id, workload_credential)) = optional_workload_identity(headers)? else {
        authenticate_operator(state, headers)?;
        return Ok(None);
    };
    state
        .runtime
        .authenticate_agent(agent_id, workload_credential)
        .map_err(LocalApiError::unauthorized)?;
    Ok(Some(ValidatedAgent {
        agent_id: agent_id.to_string(),
        workload_credential: workload_credential.to_string(),
    }))
}

pub(super) fn required_validated_source_agent(
    state: &LocalApiState,
    headers: &HeaderMap,
) -> Result<ValidatedAgent, LocalApiError> {
    validated_source_agent(state, headers)?.ok_or_else(|| {
        LocalApiError::unauthorized(ProtocolError::new(
            "workload_identity_required",
            "managed agent identity and workload credential are required",
        ))
    })
}

pub(super) async fn get_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(&format!("agents/{agent_id}"), source_agent.as_ref())
            .await?,
    ))
}

pub(super) async fn create_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "agents",
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn get_agent_startup(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(&format!("agents/{}/startup", agent.agent_id), Some(&agent))
            .await?,
    ))
}

pub(super) async fn set_agent_startup(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<SetAgentStartupRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    let body = serde_json::to_value(request)
        .map_err(|error| LocalApiError::bad_request(error.to_string()))?;
    Ok(Json(
        state
            .put_as(
                &format!("agents/{}/startup", agent.agent_id),
                &body,
                Some(&agent),
            )
            .await?,
    ))
}

pub(super) async fn clear_agent_startup(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let agent = required_validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("agents/{}/startup", agent.agent_id), Some(&agent))
            .await?,
    ))
}

pub(super) async fn list_app_deployments(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(state.get_as("apps", source_agent.as_ref()).await?))
}

pub(super) async fn get_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(&format!("apps/{app_id}"), source_agent.as_ref())
            .await?,
    ))
}

pub(super) async fn create_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAppDeploymentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "apps",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn start_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    app_lifecycle_action(state, headers, app_id, "start").await
}

pub(super) async fn stop_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    app_lifecycle_action(state, headers, app_id, "stop").await
}

pub(super) async fn restart_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    app_lifecycle_action(state, headers, app_id, "restart").await
}

pub(super) async fn app_lifecycle_action(
    state: LocalApiState,
    headers: HeaderMap,
    app_id: String,
    action: &str,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("apps/{app_id}/{action}"),
                &json!({}),
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn delete_app_deployment(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("apps/{app_id}"), source_agent.as_ref())
            .await?,
    ))
}

pub(super) async fn list_agent_launch_profiles(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as("launch-profiles", source_agent.as_ref())
            .await?,
    ))
}

pub(super) async fn get_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .get_as(
                &format!("launch-profiles/{profile_id}"),
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn create_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                "launch-profiles",
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn update_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    Json(request): Json<UpdateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("launch-profiles/{profile_id}"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn delete_agent_launch_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(
                &format!("launch-profiles/{profile_id}"),
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn launch_agent_profile(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(profile_id): Path<String>,
    Json(request): Json<LaunchAgentProfileRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("launch-profiles/{profile_id}/launch"),
                &serde_json::to_value(request)
                    .map_err(|error| LocalApiError::bad_request(error.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn rename_machine(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("servers/{server_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn delete_machine(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("servers/{server_id}"), source_agent.as_ref())
            .await?,
    ))
}

pub(super) async fn rename_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .patch_as(
                &format!("agents/{agent_id}"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn delete_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .delete_as(&format!("agents/{agent_id}"), source_agent.as_ref())
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct TerminalQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
    #[serde(default)]
    stream_epoch: Option<String>,
    #[serde(default)]
    since_revision: Option<u64>,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

pub(super) async fn agent_terminal(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Query(query): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let mut upstream = state.proxy_websocket_url(&format!("agents/{agent_id}/terminal"))?;
    upstream
        .query_pairs_mut()
        .append_pair("cols", &query.cols.max(1).to_string())
        .append_pair("rows", &query.rows.max(1).to_string());
    if let Some(stream_epoch) = query
        .stream_epoch
        .as_deref()
        .map(str::trim)
        .filter(|epoch| !epoch.is_empty())
    {
        upstream
            .query_pairs_mut()
            .append_pair("stream_epoch", stream_epoch);
        if let Some(revision) = query.since_revision {
            upstream
                .query_pairs_mut()
                .append_pair("since_revision", &revision.to_string());
        }
    }
    Ok(ws.on_upgrade(move |socket| relay_terminal(socket, state, upstream, source_agent)))
}

pub(super) async fn relay_terminal(
    socket: WebSocket,
    state: LocalApiState,
    upstream: Url,
    source_agent: Option<ValidatedAgent>,
) {
    if let Err(error) = relay_terminal_inner(socket, state, upstream, source_agent).await {
        tracing::warn!(error = %error.message, "local terminal relay closed");
    }
}

pub(super) async fn relay_terminal_inner(
    mut socket: WebSocket,
    state: LocalApiState,
    upstream: Url,
    source_agent: Option<ValidatedAgent>,
) -> Result<(), ProtocolError> {
    let mut request = upstream
        .as_str()
        .into_client_request()
        .map_err(|error| ProtocolError::new("proxy_unavailable", error.to_string()))?;
    if let Some(token) = &state.machine_token {
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| ProtocolError::new("invalid_machine_token", error.to_string()))?,
        );
    }
    if let Some(agent) = source_agent {
        request.headers_mut().insert(
            AGENT_ID_HEADER,
            HeaderValue::from_str(&agent.agent_id)
                .map_err(|error| ProtocolError::new("invalid_agent_identity", error.to_string()))?,
        );
        request.headers_mut().insert(
            WORKLOAD_CREDENTIAL_HEADER,
            HeaderValue::from_str(&agent.workload_credential).map_err(|error| {
                ProtocolError::new("invalid_workload_credential", error.to_string())
            })?,
        );
    }
    let upstream = match tokio_tungstenite::connect_async(request).await {
        Ok((upstream, _)) => upstream,
        Err(error) => {
            send_terminal_error(&mut socket, "proxy_unavailable", error.to_string()).await;
            return Err(ProtocolError::new("proxy_unavailable", error.to_string()));
        }
    };
    let (mut browser_out, mut browser_in) = socket.split();
    let (mut proxy_out, mut proxy_in) = upstream.split();

    loop {
        tokio::select! {
            message = browser_in.next() => {
                let Some(Ok(message)) = message else { break };
                let message = match message {
                    BrowserMessage::Text(text) => ProxyMessage::Text(text.to_string().into()),
                    BrowserMessage::Binary(data) => ProxyMessage::Binary(data),
                    BrowserMessage::Ping(data) => ProxyMessage::Ping(data),
                    BrowserMessage::Pong(data) => ProxyMessage::Pong(data),
                    BrowserMessage::Close(_) => break,
                };
                if proxy_out.send(message).await.is_err() {
                    break;
                }
            }
            message = proxy_in.next() => {
                let Some(Ok(message)) = message else { break };
                let message = match message {
                    ProxyMessage::Text(text) => BrowserMessage::Text(text.to_string().into()),
                    ProxyMessage::Binary(data) => BrowserMessage::Binary(data),
                    ProxyMessage::Ping(data) => BrowserMessage::Ping(data),
                    ProxyMessage::Pong(data) => BrowserMessage::Pong(data),
                    ProxyMessage::Close(_) => break,
                    ProxyMessage::Frame(_) => continue,
                };
                if browser_out.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn send_terminal_error(socket: &mut WebSocket, code: &str, message: String) {
    let message = TerminalServerMessage::Error {
        error: ProtocolError::new(code, message),
    };
    if let Ok(encoded) = serde_json::to_string(&message) {
        let _ = socket.send(BrowserMessage::Text(encoded.into())).await;
    }
}

pub(super) async fn prompt_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/prompt"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn input_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/input"),
                &serde_json::to_value(request)
                    .map_err(|err| LocalApiError::bad_request(err.to_string()))?,
                source_agent.as_ref(),
            )
            .await?,
    ))
}

pub(super) async fn read_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let suffix = query.get("lines").map_or_else(
        || format!("agents/{agent_id}/output"),
        |lines| format!("agents/{agent_id}/output?lines={lines}"),
    );
    Ok(Json(state.get_as(&suffix, source_agent.as_ref()).await?))
}

pub(super) async fn read_agent_transcript(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    let mut suffix = format!("agents/{agent_id}/transcript");
    let parameters = query
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "page" | "cursor" | "limit"))
        .map(|(key, value)| {
            format!(
                "{}={}",
                key,
                percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC)
            )
        })
        .collect::<Vec<_>>();
    if !parameters.is_empty() {
        suffix.push('?');
        suffix.push_str(&parameters.join("&"));
    }
    Ok(Json(state.get_as(&suffix, source_agent.as_ref()).await?))
}

pub(super) async fn stop_agent(
    State(state): State<LocalApiState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, LocalApiError> {
    let source_agent = validated_source_agent(&state, &headers)?;
    Ok(Json(
        state
            .post_as(
                &format!("agents/{agent_id}/stop"),
                &json!({}),
                source_agent.as_ref(),
            )
            .await?,
    ))
}
