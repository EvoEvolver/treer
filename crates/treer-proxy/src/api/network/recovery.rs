use super::*;

pub(in crate::api) async fn machine_restore_runtime(
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

pub(in crate::api) async fn restore_machine_runtime(
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
