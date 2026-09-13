use super::*;

pub(super) async fn agent_policy_subject(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<PolicySubject, ApiFailure> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| ProtocolError::new("invalid_agent_identity", "agent ID is invalid"))
        })
        .transpose()?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ProtocolError::new(
                "invalid_agent_identity",
                "managed agent identity is required",
            )
        })?;
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    if machine
        .server_id
        .as_ref()
        .is_some_and(|server_id| server_id != &agent.server_id)
    {
        return Err(ProtocolError::new(
            "policy_subject_mismatch",
            "agent does not belong to the authenticated machine",
        )
        .into());
    }
    Ok(PolicySubject::Agent {
        server_id: agent.server_id,
        agent_id: agent.agent_id,
    })
}

pub(super) async fn control_policy_subject(
    state: &AppState,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<Option<PolicySubject>, ApiFailure> {
    let Some(machine) = machine else {
        return Ok(None);
    };
    if headers.contains_key(AGENT_ID_HEADER) {
        agent_policy_subject(state, machine, headers, workspace_id)
            .await
            .map(Some)
    } else {
        Ok(Some(PolicySubject::Machine {
            server_id: machine.server_id.clone().ok_or_else(|| {
                ProtocolError::new("machine_identity_required", "machine identity is required")
            })?,
        }))
    }
}

pub(super) fn require_machine_target(
    subject: Option<&PolicySubject>,
    target_server_id: &str,
) -> Result<(), ApiFailure> {
    if let Some(PolicySubject::Machine { server_id }) = subject {
        if server_id != target_server_id {
            return Err(ProtocolError::new(
                "agent_identity_required",
                "cross-machine operations require an authenticated Agent workload credential",
            )
            .into());
        }
    }
    Ok(())
}

pub(super) async fn authorize_control(
    policy: &PolicyEngine,
    workspace_id: &str,
    subject: Option<&PolicySubject>,
    action: &str,
    resource: PolicyResource,
) -> Result<(), ApiFailure> {
    let Some(subject) = subject else {
        return Ok(());
    };
    policy
        .authorize(&PolicyRequest::new(
            workspace_id,
            subject.clone(),
            action,
            resource,
        ))
        .await?;
    Ok(())
}

pub(super) fn agent_policy_resource(agent: &AgentInfo) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT, &agent.agent_id)
        .with_attribute("server_id", &agent.server_id)
}

pub(super) fn policy_actor_name(subject: &PolicySubject) -> String {
    match subject {
        PolicySubject::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        PolicySubject::Machine { server_id } => format!("machine:{server_id}"),
        PolicySubject::Human { user_id } => format!("human:{user_id}"),
        PolicySubject::Service { service_id } => format!("service:{service_id}"),
    }
}

pub(super) fn launch_profile_policy_resource(profile_id: &str, name: &str) -> PolicyResource {
    PolicyResource::new(RESOURCE_AGENT_LAUNCH_PROFILE, profile_id).with_attribute("name", name)
}

pub(super) fn profile_actor_label(
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
) -> String {
    session.map_or_else(
        || {
            subject
                .map(policy_actor_name)
                .unwrap_or_else(|| "system".to_string())
        },
        |session| session.user_id.clone(),
    )
}

pub(super) async fn prompt_installer_recipe(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
    agent_id: &str,
    recipe: &str,
) -> Result<(), ProtocolError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let output = state
            .send_command(
                workspace_id,
                server_id,
                AgentCommand::Read {
                    agent_id: agent_id.to_string(),
                    lines: Some(80),
                },
            )
            .await?;
        let text = output
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.contains("Do you trust") || text.contains("Press enter to continue") {
            state
                .send_command(
                    workspace_id,
                    server_id,
                    AgentCommand::Input {
                        agent_id: agent_id.to_string(),
                        data: vec![b'\r'],
                    },
                )
                .await?;
        } else if installer_composer_ready(text) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    state
        .send_command(
            workspace_id,
            server_id,
            AgentCommand::Prompt {
                agent_id: agent_id.to_string(),
                text: installer_base_prompt(recipe),
            },
        )
        .await?;
    Ok(())
}

pub(super) fn queue_installer_recipe_prompt(
    state: AppState,
    workspace_id: String,
    server_id: String,
    agent_id: String,
    recipe: String,
) {
    tokio::spawn(async move {
        if let Err(error) =
            prompt_installer_recipe(&state, &workspace_id, &server_id, &agent_id, &recipe).await
        {
            tracing::warn!(
                ?error,
                %workspace_id,
                %agent_id,
                "failed to prompt installer with bundled skill"
            );
        }
    });
}

pub(super) fn require_agent_can_probe_service(
    subject: &PolicySubject,
    service: &MachineService,
) -> Result<(), ApiFailure> {
    match subject {
        PolicySubject::Agent { server_id, .. } if server_id == &service.server_id => Ok(()),
        PolicySubject::Agent { .. } => Err(ApiFailure::forbidden(
            "service_not_owned",
            "agents may probe only services on their own machine",
        )),
        PolicySubject::Machine { .. }
        | PolicySubject::Human { .. }
        | PolicySubject::Service { .. } => Err(ApiFailure::forbidden(
            "ingress_agent_required",
            "a managed agent identity is required to probe a service",
        )),
    }
}

pub(super) fn machine_service_policy_resource(
    service_id: &str,
    name: &str,
    server_id: &str,
    target_agent_id: Option<&str>,
    target_host: &str,
    target_port: u16,
) -> PolicyResource {
    let resource = PolicyResource::new(RESOURCE_MACHINE_SERVICE, service_id)
        .with_attribute("name", name)
        .with_attribute("server_id", server_id)
        .with_attribute("target_host", target_host)
        .with_attribute("target_port", target_port.to_string());
    if let Some(agent_id) = target_agent_id {
        resource.with_attribute("target_agent_id", agent_id)
    } else {
        resource
    }
}

pub(super) async fn list_agent_launch_profiles(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_LIST,
        PolicyResource::new(RESOURCE_AGENT_LAUNCH_PROFILE, "*"),
    )
    .await?;
    Ok(Json(json!({
        "profiles": auth.list_agent_launch_profiles(&workspace_id).await?
    })))
}

pub(super) async fn get_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_READ,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn create_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_CREATE,
        launch_profile_policy_resource("new", request.name.trim()),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .create_agent_launch_profile(
            &workspace_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
            request,
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn update_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<UpdateAgentLaunchProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_UPDATE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .update_agent_launch_profile(
            &workspace_id,
            &profile.profile_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
            request,
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn delete_agent_launch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_DELETE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let actor_label = profile_actor_label(session.as_deref(), subject.as_ref());
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    let profile = auth
        .delete_agent_launch_profile(
            &workspace_id,
            &profile.profile_id,
            ProfileMutationActor {
                kind: actor_kind,
                id: actor_id,
                label: &actor_label,
            },
        )
        .await?;
    Ok(Json(serde_json::to_value(profile)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn launch_agent_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<LaunchAgentProfileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let profile = auth
        .resolve_agent_launch_profile(&workspace_id, &target)
        .await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_LAUNCH_PROFILE_USE,
        launch_profile_policy_resource(&profile.profile_id, &profile.name),
    )
    .await?;
    let profile_id = profile.profile_id.clone();
    let agent_request = agent_request_from_launch_profile(&profile, request)?;
    let data = execute_agent_create(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        subject.as_ref(),
        &workspace_id,
        agent_request,
        Some(&profile_id),
    )
    .await?;
    Ok(Json(data))
}

pub(super) fn agent_request_from_launch_profile(
    profile: &AgentLaunchProfile,
    request: LaunchAgentProfileRequest,
) -> Result<CreateAgentRequest, ProtocolError> {
    let agent_name = request
        .agent_name
        .map(normalize_display_name)
        .transpose()?
        .unwrap_or_else(|| profile.name.clone());
    let mut args = Vec::with_capacity(profile.args.len() + 1);
    args.push(profile.command.clone());
    args.extend(profile.args.clone());
    Ok(CreateAgentRequest {
        server_id: request.server_id,
        kind: "shell".to_string(),
        name: agent_name,
        cwd: request.cwd.unwrap_or_else(|| profile.cwd.clone()),
        args,
        cols: request.cols,
        rows: request.rows,
        publish_ports: Vec::new(),
        recipe: None,
    })
}

pub(super) async fn list_agents(
    State(state): State<AppState>,
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
    for agent in snapshot.agents {
        if agent.kind == "app" {
            continue;
        }
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

pub(super) async fn get_agent(
    State(state): State<AppState>,
    Extension(policy): Extension<PolicyEngine>,
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
        ACTION_AGENT_METADATA_READ,
        agent_policy_resource(&agent),
    )
    .await?;
    Ok(Json(serde_json::to_value(agent)?))
}

pub(super) fn require_self_agent(
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

pub(super) async fn get_agent_startup(
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

pub(super) async fn set_agent_startup(
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

pub(super) async fn clear_agent_startup(
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

pub(super) async fn validate_agent_startup(
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn exec_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<MachineExecRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_EXEC,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let audit_cwd = request.cwd.clone();
    let audit_timeout_ms = request.timeout_ms;
    let data = state
        .send_command(&workspace_id, &server_id, AgentCommand::Exec { request })
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.exec",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({
                "cwd": audit_cwd,
                "timeout_ms": audit_timeout_ms,
                "exit_code": data.get("exit_code"),
                "timed_out": data.get("timed_out"),
                "truncated": data.get("truncated"),
            }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record machine exec audit event");
    }
    Ok(Json(data))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn upload_machine_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<UploadMachineFileRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_FILE_WRITE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    if request.content_base64.len() > MAX_MACHINE_FILE_BYTES.div_ceil(3) * 4 + 4 {
        return Err(ApiFailure::bad_request(
            "machine_file_too_large",
            &format!("uploaded files may contain at most {MAX_MACHINE_FILE_BYTES} bytes"),
        ));
    }
    let contents = base64::engine::general_purpose::STANDARD
        .decode(&request.content_base64)
        .map_err(|_| {
            ApiFailure::bad_request("invalid_machine_file", "file content is not valid base64")
        })?;
    if contents.len() > MAX_MACHINE_FILE_BYTES {
        return Err(ApiFailure::bad_request(
            "machine_file_too_large",
            &format!("uploaded files may contain at most {MAX_MACHINE_FILE_BYTES} bytes"),
        ));
    }
    let upload_id = format!("upl_{}", Uuid::new_v4().simple());
    state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::UploadBegin {
                upload_id: upload_id.clone(),
                directory: request.directory.clone(),
                file_name: request.file_name.clone(),
                overwrite: request.overwrite,
            },
        )
        .await?;
    let upload_result = async {
        for chunk in contents.chunks(MACHINE_FILE_CHUNK_BYTES) {
            state
                .send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::UploadChunk {
                        upload_id: upload_id.clone(),
                        content_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
                    },
                )
                .await?;
        }
        let value = state
            .send_command(
                &workspace_id,
                &server_id,
                AgentCommand::UploadCommit {
                    upload_id: upload_id.clone(),
                },
            )
            .await?;
        serde_json::from_value::<UploadMachineFileResponse>(value).map_err(|error| {
            ProtocolError::new(
                "invalid_machine_upload_response",
                format!("Controller returned an invalid upload response: {error}"),
            )
        })
    }
    .await;
    let uploaded = match upload_result {
        Ok(uploaded) => uploaded,
        Err(error) => {
            let _ = state
                .send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::UploadAbort {
                        upload_id: upload_id.clone(),
                    },
                )
                .await;
            return Err(error.into());
        }
    };
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.file.uploaded",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: None,
            payload: json!({
                "path": uploaded.path,
                "bytes_written": uploaded.bytes_written,
                "overwrite": request.overwrite,
            }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record machine upload audit event");
    }
    Ok(Json(serde_json::to_value(uploaded)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn rename_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_UPDATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_machine_name(&workspace_id, &server_id, &name)
        .await?;
    let renamed = state
        .rename_server(&workspace_id, &server_id, name.clone())
        .await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.renamed",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&name),
            payload: json!({}),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(serde_json::to_value(renamed)?))
}

pub(super) async fn delete_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let server = state.resolve_server(&workspace_id, &server_id).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_MACHINE_DELETE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let agents = state
        .snapshot(&workspace_id)
        .await?
        .agents
        .into_iter()
        .filter(|agent| agent.server_id == server_id)
        .collect::<Vec<_>>();
    let stops = agents
        .iter()
        .filter(|agent| !agent.status.is_terminal())
        .map(|agent| async {
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::Stop {
                        agent_id: agent.agent_id.clone(),
                    },
                ),
            )
            .await
        });
    let _ = futures_util::future::join_all(stops).await;
    let shutdown_requested = if server.labels.get("treer.shutdown").map(String::as_str) == Some("1")
    {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(&workspace_id, &server_id, AgentCommand::ShutdownMachine),
            )
            .await,
            Ok(Ok(_))
        )
    } else {
        false
    };
    let agent_ids = agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    auth.delete_machine(&workspace_id, &server_id, &agent_ids)
        .await?;
    let (server, deleted_agents) = state.delete_server(&workspace_id, &server_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: "machine.deleted",
            resource_kind: "machine",
            resource_id: &server_id,
            resource_name: Some(&server.name),
            payload: json!({ "deleted_agent_count": deleted_agents.len(), "shutdown_requested": shutdown_requested }),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, %server_id, "failed to record runtime audit event");
    }
    Ok(Json(json!({
        "server": server,
        "deleted_agents": deleted_agents,
        "shutdown_requested": shutdown_requested,
    })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn rename_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
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
        ACTION_AGENT_UPDATE,
        agent_policy_resource(&agent),
    )
    .await?;
    let name = normalize_display_name(request.name)?;
    auth.set_agent_name(&workspace_id, &agent.agent_id, &name)
        .await?;
    let renamed = state
        .rename_agent(&workspace_id, &agent.agent_id, name.clone())
        .await?;
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

pub(super) async fn delete_agent(
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
        ACTION_AGENT_DELETE,
        agent_policy_resource(&agent),
    )
    .await?;
    if !agent.status.is_terminal() {
        state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::Stop {
                    agent_id: agent.agent_id.clone(),
                },
            )
            .await?;
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

pub(super) fn normalize_display_name(name: String) -> Result<String, ProtocolError> {
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
pub(super) async fn create_agent(
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
pub(super) async fn execute_agent_create(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
    workspace_id: &str,
    request: CreateAgentRequest,
    launch_profile_id: Option<&str>,
) -> Result<Value, ApiFailure> {
    if let Some(recipe) = recipe_url(&request) {
        if let Err(message) = validate_recipe_url(recipe) {
            return Err(ApiFailure::bad_request("invalid_recipe", &message));
        }
        if !recipe_installer_kind_allowed(&request.kind) {
            return Err(ApiFailure::bad_request(
                "recipe_requires_interactive_agent",
                "recipe install requires an interactive agent kind or auto",
            ));
        }
    }
    let recipe = recipe_url(&request).map(str::to_string);
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
    if let Some(recipe) = recipe.as_deref() {
        if let Ok(snapshot) = state.snapshot(workspace_id).await {
            let filter = if request.kind == "auto" {
                None
            } else {
                Some(request.kind.as_str())
            };
            if let Some(existing) =
                pick_existing_installer_agent(&snapshot.agents, &server_id, filter)
            {
                let agent_id = existing.agent_id.clone();
                let mut data = serde_json::to_value(existing).unwrap_or_else(|_| json!({}));
                queue_installer_recipe_prompt(
                    state.clone(),
                    workspace_id.to_string(),
                    server_id.clone(),
                    agent_id,
                    recipe.to_string(),
                );
                if let Some(object) = data.as_object_mut() {
                    object.insert("installer_reused".into(), json!(true));
                    object.insert("installer_prompted".into(), json!("queued"));
                }
                return Ok(data);
            }
        }
    }
    let agent_id = format!("ag_{}", Uuid::new_v4().simple());
    let agent_name = request.name.clone();
    let workload_credential = auth
        .create_agent_credential(workspace_id, &server_id, &agent_id)
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
        .await?;
    if let Some(recipe) = recipe.as_deref() {
        queue_installer_recipe_prompt(
            state.clone(),
            workspace_id.to_string(),
            server_id.clone(),
            agent_id.clone(),
            recipe.to_string(),
        );
        if let Some(object) = data.as_object_mut() {
            object.insert("installer_prompted".into(), json!("queued"));
        }
    }
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

pub(super) async fn prompt_agent(
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

pub(super) async fn input_agent(
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

pub(super) async fn read_agent(
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

pub(super) async fn read_agent_transcript(
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

pub(super) async fn stop_agent(
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

pub(super) async fn abort_agent(
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
    #[serde(default)]
    flow_control: bool,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

impl TerminalQuery {
    fn cursor(&self) -> Option<TerminalCursor> {
        let stream_epoch = self.stream_epoch.as_deref()?.trim();
        if stream_epoch.is_empty() {
            return None;
        }
        Some(TerminalCursor {
            stream_epoch: stream_epoch.to_string(),
            revision: self.since_revision.unwrap_or(0),
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn agent_terminal(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Extension(policy): Extension<PolicyEngine>,
    machine: Option<Extension<MachineSession>>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    let agent = state.resolve_agent(&workspace_id, &agent_id).await?;
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
    Ok(ws.on_upgrade(move |socket| stream_terminal(socket, state, workspace_id, agent_id, query)))
}

pub(super) async fn stream_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    agent_id: String,
    query: TerminalQuery,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) =
        tokio::sync::mpsc::channel::<SocketFrame>(TERMINAL_BROWSER_QUEUE_CAPACITY);
    let cursor = query.cursor();
    let attached = state
        .attach_terminal(
            &workspace_id,
            &agent_id,
            query.cols,
            query.rows,
            cursor,
            terminal_tx,
        )
        .await;
    let session_id = match attached {
        Ok(session_id) => session_id,
        Err(error) => {
            let message = TerminalServerMessage::Error { error };
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = outgoing.send(Message::Text(encoded.into())).await;
            }
            return;
        }
    };

    let flow_window_bytes = query.flow_control.then_some(TERMINAL_FLOW_WINDOW_BYTES);
    let mut in_flight_bytes = 0usize;
    loop {
        tokio::select! {
            message = incoming.next() => {
                let Some(Ok(message)) = message else { break };
                let result = match message {
                    Message::Binary(data) => state.terminal_input(&session_id, data.to_vec()).await,
                    Message::Text(text) => match serde_json::from_str::<TerminalClientMessage>(&text) {
                        Ok(TerminalClientMessage::Resize { cols, rows }) => {
                            state.terminal_resize(&session_id, cols, rows).await
                        }
                        Ok(TerminalClientMessage::Ack { bytes }) => {
                            let Some(_) = flow_window_bytes else {
                                continue;
                            };
                            let bytes = bytes as usize;
                            if bytes > in_flight_bytes {
                                Err(ProtocolError::new(
                                    "invalid_terminal_ack",
                                    "terminal acknowledgement exceeds outstanding output",
                                ))
                            } else {
                                in_flight_bytes -= bytes;
                                Ok(())
                            }
                        }
                        Err(error) => Err(ProtocolError::new("invalid_terminal_message", error.to_string())),
                    },
                    Message::Close(_) => break,
                    _ => continue,
                };
                if let Err(error) = result {
                    let message = TerminalServerMessage::Error { error };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        if outgoing.send(Message::Text(encoded.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            frame = terminal_rx.recv(), if flow_window_bytes.is_none_or(|window| in_flight_bytes < window) => {
                let Some(frame) = frame else { break };
                let binary_bytes = match &frame {
                    SocketFrame::Binary(data) => data.len(),
                    _ => 0,
                };
                let message = match frame {
                    SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                    SocketFrame::Binary(data) => Message::Binary(data.into()),
                    SocketFrame::Ping(payload) => Message::Ping(payload.into()),
                    SocketFrame::Pong(payload) => Message::Pong(payload.into()),
                    SocketFrame::Close => Message::Close(None),
                };
                if outgoing.send(message).await.is_err() {
                    break;
                }
                if flow_window_bytes.is_some() {
                    in_flight_bytes = in_flight_bytes.saturating_add(binary_bytes);
                }
            }
        }
    }
    state.detach_terminal(&session_id).await;
}
