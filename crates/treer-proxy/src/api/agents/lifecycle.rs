use super::*;
pub(crate) async fn list_agents(
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

pub(crate) async fn get_agent(
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn exec_machine(
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
pub(crate) async fn upload_machine_file(
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
pub(crate) async fn rename_server(
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

pub(crate) async fn delete_server(
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

pub(crate) async fn delete_agent(
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
