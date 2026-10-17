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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(ingress): Extension<IngressConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let mut agent = resolve_workload_at(
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
    // Humans are admitted by membership, not as control-route subjects; the
    // interface origin is gated per human (#143 Phase 2).
    let human = session.as_deref().map(|session| PolicySubject::Human {
        user_id: session.user_id.clone(),
    });
    super::super::network::hydrate_agent_interface_origin(
        &policy,
        &ingress,
        &workspace_id,
        human.as_ref(),
        &mut agent,
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
    state
        .send_command(
            workspace_id,
            &server_id,
            AgentCommand::CreationPlacementCheck {},
        )
        .await?;
    let agent_name = request.name.clone();
    let workload_credential = auth
        .reserve_named_agent(
            workspace_id,
            &server_id,
            &agent_id,
            &request.name,
            request.folder_id.as_deref(),
            match subject {
                Some(PolicySubject::Agent { agent_id, .. }) => Some(agent_id.as_str()),
                _ => None,
            },
            session
                .filter(|session| match subject {
                    None => true,
                    Some(PolicySubject::Human { user_id }) => user_id == &session.user_id,
                    _ => false,
                })
                .map(|session| session.user_id.as_str()),
        )
        .await?;
    let data = state
        .send_command(
            workspace_id,
            &server_id,
            AgentCommand::Create {
                agent_id: agent_id.clone(),
                workload_credential,
                request,
            },
        )
        .await;
    let mut data = match data {
        Ok(data) => data,
        Err(mut error) => {
            use treer_protocol::{AgentCreationErrorDetails, AgentCreationFailureState};
            let not_started = error.code == "command_not_found"
                && error.details.as_ref().is_some_and(|details| {
                    details.creation_state == AgentCreationFailureState::NotStarted
                        && details.agent_id.as_deref() == Some(agent_id.as_str())
                });
            let command = error
                .details
                .as_ref()
                .and_then(|details| details.command.as_deref())
                .and_then(treer_protocol::agent_install_command_name);
            let creation_state = if not_started {
                match auth.delete_agent(workspace_id, &agent_id).await {
                    Ok(()) => AgentCreationFailureState::IdentityRemoved,
                    Err(_) => AgentCreationFailureState::CleanupFailed,
                }
            } else {
                AgentCreationFailureState::ReservedOutcomeUnknown
            };
            // Never forward a Host/runtime message containing PATH/argv. Keep
            // transport/policy codes, but report the reservation state explicitly.
            error.message = if error.code == "command_not_found" {
                "Required executable is not installed on this machine"
            } else {
                "Agent creation failed; its startup outcome could not be confirmed"
            }
            .into();
            error.details = Some(Box::new(AgentCreationErrorDetails {
                command,
                machine_id: Some(server_id.clone()),
                agent_id: Some(agent_id.clone()),
                creation_state,
            }));
            let (actor_kind, actor_id) = control_audit_actor(session, subject);
            if auth.record_workspace_audit(NewWorkspaceAuditEvent {
                workspace_id,
                actor_kind,
                actor_id,
                action: "agent.create.failed",
                resource_kind: "agent",
                resource_id: &agent_id,
                resource_name: Some(&agent_name),
                payload: json!({"server_id":server_id,"code":error.code,"creation_state":creation_state}),
            }).await.is_err() {
                tracing::warn!("failed to record Agent creation failure audit event");
            }
            error.hint = Some(vec![agent_id]);
            return Err(ApiFailure::from(error));
        }
    };
    if auth
        .confirm_agent_creation(workspace_id, &server_id, &agent_id)
        .await
        .is_err()
    {
        let mut error = ProtocolError::new("agent_creation_confirmation_failed", "Agent creation was acknowledged but bookkeeping could not be confirmed; inspect the Agent before retrying");
        error.details = Some(Box::new(treer_protocol::AgentCreationErrorDetails {
            command: None,
            machine_id: Some(server_id.clone()),
            agent_id: Some(agent_id.clone()),
            creation_state: treer_protocol::AgentCreationFailureState::ReservedOutcomeUnknown,
        }));
        return Err(error.into());
    }
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
