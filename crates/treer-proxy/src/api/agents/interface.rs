use super::*;
pub(crate) async fn prompt_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_PROMPT,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Prompt {
                agent_id: agent.agent_id,
                text: request.text,
            },
        )
        .await?;
    Ok(Json(data))
}

pub(crate) async fn input_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_INPUT,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Input {
                agent_id: agent.agent_id,
                data: request.data,
            },
        )
        .await?;
    Ok(Json(data))
}

pub(crate) async fn read_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_OUTPUT_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    let lines = query.get("lines").and_then(|value| value.parse().ok());
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Read {
                agent_id: agent.agent_id,
                lines,
            },
        )
        .await?;
    Ok(Json(data))
}

pub(crate) async fn read_agent_transcript(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_OUTPUT_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    let cursor = query
        .get("page")
        .cloned()
        .or_else(|| query.get("cursor").cloned());
    let limit = query.get("limit").and_then(|value| value.parse().ok());
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Transcript {
                agent_id: agent.agent_id,
                cursor,
                limit,
            },
        )
        .await?;
    Ok(Json(data))
}

pub(crate) async fn stop_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        agent_policy_resource(&agent),
    )
    .await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Stop {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.stopped",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(data))
}

pub(crate) async fn abort_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_ABORT,
        agent_policy_resource(&agent),
    )
    .await?;
    if !agent
        .interface
        .as_ref()
        .is_some_and(|interface| interface.supports("abort"))
    {
        return Err(ApiFailure::bad_request(
            "agent_interface_capability_unavailable",
            "Agent does not expose abort",
        ));
    }
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Abort {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.aborted",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(data))
}
