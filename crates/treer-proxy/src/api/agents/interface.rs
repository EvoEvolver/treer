use super::*;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AgentOutputQuery {
    pub lines: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AgentTranscriptQuery {
    pub page: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

pub(crate) async fn prompt_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context.authorize_agent(&agent, ACTION_AGENT_PROMPT).await?;
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
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
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context.authorize_agent(&agent, ACTION_AGENT_INPUT).await?;
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
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
    Query(query): Query<AgentOutputQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context
        .authorize_agent(&agent, ACTION_AGENT_OUTPUT_READ)
        .await?;
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
            &agent.server_id,
            AgentCommand::Read {
                agent_id: agent.agent_id,
                lines: query.lines,
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
    Query(query): Query<AgentTranscriptQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context
        .authorize_agent(&agent, ACTION_AGENT_OUTPUT_READ)
        .await?;
    let cursor = query.page.or(query.cursor);
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
            &agent.server_id,
            AgentCommand::Transcript {
                agent_id: agent.agent_id,
                cursor,
                limit: query.limit,
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
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context.authorize_agent(&agent, ACTION_AGENT_STOP).await?;
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
            &agent.server_id,
            AgentCommand::Stop {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), context.policy_subject());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: context.workspace_id(),
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
        tracing::warn!(?error, workspace_id = %context.workspace_id(), agent_id = %agent.agent_id, "failed to record runtime audit event");
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
    let context = AgentRequestContext::new(
        &state,
        &policy,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context.authorize_agent(&agent, ACTION_AGENT_ABORT).await?;
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
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
            &agent.server_id,
            AgentCommand::Abort {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), context.policy_subject());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: context.workspace_id(),
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
        tracing::warn!(?error, workspace_id = %context.workspace_id(), agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(data))
}
