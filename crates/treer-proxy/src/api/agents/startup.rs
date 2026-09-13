use super::*;
pub(crate) fn require_self_agent(
    subject: &PolicySubject,
    agent_id: &str,
) -> Result<(), ApiFailure> {
    match subject {
        PolicySubject::Agent {
            agent_id: source, ..
        } if source == agent_id => Ok(()),
        _ => Err(ProtocolError::new(
            "agent_identity_mismatch",
            "an Agent may manage only its own startup command",
        )
        .into()),
    }
}

pub(crate) async fn get_agent_startup(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    require_self_agent(&subject, &agent_id)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    authorize_control(
        &policy,
        &workspace_id,
        Some(&subject),
        ACTION_AGENT_STARTUP_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    Ok(Json(
        state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::StartupGet { agent_id },
            )
            .await?,
    ))
}

pub(crate) async fn set_agent_startup(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Json(request): Json<SetAgentStartupRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    require_self_agent(&subject, &agent_id)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    authorize_control(
        &policy,
        &workspace_id,
        Some(&subject),
        ACTION_AGENT_STARTUP_MANAGE,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::StartupSet {
                agent_id: agent_id.clone(),
                request,
            },
        )
        .await?;
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind: "agent",
            actor_id: Some(&agent_id),
            action: "agent.startup.set",
            resource_kind: "agent",
            resource_id: &agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %agent_id, "failed to record Agent startup audit event");
    }
    Ok(Json(data))
}

pub(crate) async fn clear_agent_startup(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    require_self_agent(&subject, &agent_id)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
    authorize_control(
        &policy,
        &workspace_id,
        Some(&subject),
        ACTION_AGENT_STARTUP_MANAGE,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::StartupClear {
                agent_id: agent_id.clone(),
            },
        )
        .await?;
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind: "agent",
            actor_id: Some(&agent_id),
            action: "agent.startup.cleared",
            resource_kind: "agent",
            resource_id: &agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %agent_id, "failed to record Agent startup audit event");
    }
    Ok(Json(data))
}

pub(crate) async fn validate_agent_startup(
    Extension(auth): Extension<AuthStore>,
    Extension(machine): Extension<MachineSession>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<ValidateAgentStartupRequest>,
) -> Result<Json<ValidateAgentStartupResponse>, ApiFailure> {
    if !machine.allows_server(&workspace_id, &server_id) {
        return Err(ProtocolError::new(
            "machine_identity_mismatch",
            "startup validation targets another machine",
        )
        .into());
    }
    if request.agent_ids.len() > 256 {
        return Err(ApiFailure::bad_request(
            "invalid_agent_startup_validation",
            "at most 256 Agent IDs may be validated at once",
        ));
    }
    Ok(Json(ValidateAgentStartupResponse {
        active_agent_ids: auth
            .active_agent_ids(&workspace_id, &server_id, &request.agent_ids)
            .await?,
    }))
}
