use super::*;
pub(crate) async fn list_agents(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let mut agents = Vec::new();
    let tree = auth.workload_tree(&workspace_id).await?;
    for mut agent in snapshot.agents {
        agent.path = Some(tree.workload_path(&agent.agent_id, &agent.name)?);
        if matches!(subject.as_ref(), Some(PolicySubject::Machine { server_id }) if server_id != &agent.server_id)
        {
            continue;
        }
        match authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_DISCOVER,
            agent_policy_resource(&agent),
        )
        .await
        {
            Ok(()) => agents.push(agent),
            Err(error) if error.error.code == "policy_denied" => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Json(json!({ "agents": agents })))
}

pub(crate) async fn get_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = resolve_workload_at(
        &state,
        &auth,
        &workspace_id,
        headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok()),
        &target,
    )
    .await?;
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
        ACTION_AGENT_METADATA_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    let mut value = serde_json::to_value(&agent)?;
    if agent
        .interface
        .as_ref()
        .is_some_and(|interface| interface.supports("state.observe"))
    {
        match state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::InterfaceStatus {
                    agent_id: agent.agent_id.clone(),
                },
            )
            .await
        {
            Ok(status) => {
                value["busy"] = status["busy"].clone();
                value["queue_length"] = status["queue_length"].clone();
            }
            Err(error) => {
                value["activity_error"] = serde_json::to_value(error)?;
            }
        }
    }
    Ok(Json(value))
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn rename_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = resolve_workload_at(
        &state,
        &auth,
        &workspace_id,
        headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok()),
        &target,
    )
    .await?;
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
        ACTION_AGENT_UPDATE,
        agent_policy_resource(&agent),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_agent_name(&workspace_id, &agent.agent_id, &name)
        .await?;
    let renamed = match state
        .rename_agent(&workspace_id, &agent.agent_id, name.clone())
        .await
    {
        Ok(agent) => agent,
        Err(error) if error.code == "agent_not_found" => {
            let mut agent = agent.clone();
            agent.name = name.clone();
            agent
        }
        Err(error) => return Err(error.into()),
    };
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.renamed",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(renamed)?))
}

pub(crate) async fn delete_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = resolve_workload_at(
        &state,
        &auth,
        &workspace_id,
        headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok()),
        &target,
    )
    .await?;
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
        ACTION_AGENT_DELETE,
        agent_policy_resource(&agent),
    )
    .await?;
    if let Some(app) = auth
        .resolve_app_deployment_by_runtime(&workspace_id, &agent.agent_id)
        .await?
    {
        remove_app_workload(&state, &auth, &app).await?;
        publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
        record_app_audit(
            &auth,
            session.as_deref(),
            subject.as_ref(),
            "app.deleted",
            &app,
        )
        .await;
        return Ok(Json(serde_json::to_value(agent)?));
    }
    let guard = auth
        .workload_lifecycle_guard(&workspace_id, &agent.agent_id)
        .await?;
    auth.set_agent_recovery_enabled(&workspace_id, &agent.agent_id, false)
        .await?;
    match state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Stop {
                agent_id: agent.agent_id.clone(),
            },
        )
        .await
    {
        Ok(_) => {}
        Err(error) if stop_target_is_absent(&error) => {}
        Err(error) => return Err(error.into()),
    }
    // Credential revocation below is authoritative for recovery. Do not send a
    // startup command here: older Controllers reject unknown command variants
    // and would disconnect the entire machine during a rolling upgrade.
    auth.delete_agent(&workspace_id, &agent.agent_id).await?;
    auth.refresh_virtual_network_hosts()
        .await
        .map_err(|error| {
            ApiFailure::internal("virtual_host_refresh_failed", &format!("{error:#}"))
        })?;
    refresh_service_ingress_routes(&auth).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    let deleted = state.delete_agent(&workspace_id, &agent.agent_id).await?;
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "agent.deleted",
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({ "server_id": &agent.server_id }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, agent_id = %agent.agent_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(deleted)?))
}

pub(crate) fn normalize_display_name(name: String) -> Result<String, ProtocolError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ProtocolError::new(
            "invalid_name",
            "name must contain 1-80 visible characters",
        ));
    }
    Ok(name.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let data = execute_agent_create(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        subject.as_ref(),
        &workspace_id,
        request,
        None,
    )
    .await?;
    Ok(Json(data))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_agent_create(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
    workspace_id: &str,
    request: CreateAgentRequest,
    launch_profile_id: Option<&str>,
) -> Result<Value, ApiFailure> {
    if request
        .recipe
        .as_deref()
        .is_some_and(|recipe| !recipe.trim().is_empty())
    {
        return Err(ApiFailure::bad_request("recipe_runtime_retired", "launch your installer through an explicit saved profile and provide its recipe through the Agent Interface; Core no longer installs or prompts provider runtimes"));
    }
    let server_id = state
        .select_server(workspace_id, request.server_id.as_deref())
        .await?;
    require_machine_target(subject, &server_id)?;
    authorize_control(
        policy,
        workspace_id,
        subject,
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    if request.folder_id.is_some() {
        authorize_control(
            policy,
            workspace_id,
            subject,
            crate::policy::ACTION_AGENT_TREE_MANAGE,
            PolicyResource::new(crate::policy::RESOURCE_AGENT_TREE, workspace_id),
        )
        .await?;
    }
    let agent_id = format!("ag_{}", Uuid::new_v4().simple());
    let agent_name = request.name.clone();
    let workload_credential = auth
        .create_named_agent_credential(
            workspace_id,
            &server_id,
            &agent_id,
            &request.name,
            request.folder_id.as_deref(),
        )
        .await?;
    let mut data = state
        .send_command(
            workspace_id,
            &server_id,
            AgentCommand::Create {
                agent_id: agent_id.clone(),
                workload_credential,
                request,
            },
        )
        .await.map_err(|mut error| {
            error.message = format!("{}; Agent identity {} was reserved before dispatch; inspect it before retrying creation", error.message, agent_id);
            error.hint = Some(vec![agent_id.clone()]);
            ApiFailure::from(error)
        })?;
    let tree = auth.workload_tree(workspace_id).await?;
    data["path"] = json!(tree.workload_path(&agent_id, &agent_name)?);
    let (actor_kind, actor_id) = control_audit_actor(session, subject);
    let payload = launch_profile_id.map_or_else(
        || json!({ "server_id": &server_id }),
        |profile_id| json!({ "server_id": &server_id, "launch_profile_id": profile_id }),
    );
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id,
            actor_kind,
            actor_id,
            action: "agent.created",
            resource_kind: "agent",
            resource_id: &agent_id,
            resource_name: Some(&agent_name),
            payload,
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %agent_id, "failed to record runtime audit event");
    }
    Ok(data)
}
