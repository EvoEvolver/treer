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
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(mut request): Json<treer_protocol::SubmitAgentPromptRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context.authorize_agent(&agent, ACTION_AGENT_PROMPT).await?;
    let sender = match context.policy_subject() {
        Some(PolicySubject::Agent { agent_id, .. }) => {
            let caller = context.resolve_agent(agent_id).await?;
            Some(treer_protocol::PromptSender {
                agent_id: caller.agent_id,
                path: caller.path.unwrap_or_else(|| format!("/{}", caller.name)),
                name: caller.name,
            })
        }
        _ => None,
    };
    // Agent-to-Agent tasks report completion without a polling loop. An explicit
    // empty `on` list remains the opt-out for API clients.
    if sender.is_some()
        && request.delivery == treer_protocol::PromptDelivery::Queue
        && request.notify.is_none()
    {
        request.notify = Some(treer_protocol::PromptNotify::default());
    }
    if let Some(notify) = request.notify.as_mut() {
        if notify.to.is_none() {
            notify.to = sender.as_ref().map(|sender| sender.agent_id.clone());
        }
        if notify.to.is_none() {
            return Err(ProtocolError::new(
                "invalid_request",
                "Human/operator notifications require an explicit notify.to recipient",
            )
            .into());
        }
    }
    let data = context
        .state()
        .send_command(
            context.workspace_id(),
            &agent.server_id,
            AgentCommand::SubmitPrompt {
                agent_id: agent.agent_id,
                request,
                sender,
            },
        )
        .await?;
    Ok(Json(data))
}

pub(crate) async fn read_prompt_queue(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
        machine.as_ref().map(|m| &m.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    context
        .authorize_agent(&agent, crate::policy::ACTION_AGENT_QUEUE_READ)
        .await?;
    Ok(Json(
        state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::QueueRead {
                    agent_id: agent.agent_id,
                },
            )
            .await?,
    ))
}

pub(crate) async fn read_prompt_submission(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target, id)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    prompt_submission_access(
        state,
        auth,
        policy,
        machine.map(|m| m.0),
        headers,
        workspace_id,
        target,
        id,
        false,
    )
    .await
    .map(Json)
}

pub(crate) async fn cancel_prompt_submission(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target, id)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    prompt_submission_access(
        state,
        auth,
        policy,
        machine.map(|m| m.0),
        headers,
        workspace_id,
        target,
        id,
        true,
    )
    .await
    .map(Json)
}

#[allow(clippy::too_many_arguments)]
async fn prompt_submission_access(
    state: AppState,
    auth: AuthStore,
    policy: PolicyEngine,
    machine: Option<MachineSession>,
    headers: HeaderMap,
    workspace_id: String,
    target: String,
    id: String,
    cancel: bool,
) -> Result<Value, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
        machine.as_ref(),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = context.resolve_agent(&target).await?;
    require_machine_target(context.policy_subject(), &agent.server_id)?;
    let result = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::SubmissionRead {
                agent_id: agent.agent_id.clone(),
                submission_id: id.clone(),
            },
        )
        .await;
    let owned = match (context.policy_subject(), result.as_ref().ok()) {
        (Some(PolicySubject::Agent { agent_id, .. }), Some(data)) => {
            data["submission"]["sender"]["agent_id"].as_str() == Some(agent_id.as_str())
        }
        _ => false,
    };
    if !owned {
        context
            .authorize_agent(
                &agent,
                if cancel {
                    crate::policy::ACTION_AGENT_QUEUE_CANCEL
                } else {
                    crate::policy::ACTION_AGENT_QUEUE_READ
                },
            )
            .await?;
    }
    let submission = result?;
    if !cancel {
        return Ok(submission);
    }
    Ok(state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::QueueCancel {
                agent_id: agent.agent_id,
                submission_id: id,
            },
        )
        .await?)
}

pub(crate) async fn input_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
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
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<AgentOutputQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
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
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<AgentTranscriptQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let context = AgentRequestContext::new(
        &state,
        &policy,
        &auth,
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
        &auth,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let agent = resolve_workload_at(
        &state,
        &auth,
        &workspace_id,
        headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok()),
        &target,
    )
    .await?;
    context.authorize_agent(&agent, ACTION_AGENT_STOP).await?;
    let data = apply_workload_action(
        &state,
        &auth,
        &workspace_id,
        &agent,
        WorkloadAction::Stop,
        app_mutation_actor(session.as_deref(), context.policy_subject()),
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
        &auth,
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
