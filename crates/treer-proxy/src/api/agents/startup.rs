use super::*;

#[allow(clippy::too_many_arguments)]
async fn admin_startup_handler(
    state: AppState,
    auth: AuthStore,
    policy: PolicyEngine,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    workspace_id: String,
    target: String,
    request: Option<ReplaceAgentStartupRequest>,
    restart: bool,
) -> Result<Json<Value>, ApiFailure> {
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|m| &m.0),
        &headers,
        &workspace_id,
    )
    .await?
    .or_else(|| {
        session.as_ref().map(|session| PolicySubject::Human {
            user_id: session.user_id.clone(),
        })
    });
    let target = if target == "self" {
        match &subject {
            Some(PolicySubject::Agent { agent_id, .. }) => agent_id.clone(),
            _ => {
                return Err(ApiFailure::bad_request(
                    "agent_identity_required",
                    "self requires an authenticated Agent",
                ))
            }
        }
    } else {
        target
    };
    let agent = resolve_workload_at(
        &state,
        &auth,
        &workspace_id,
        headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok()),
        &target,
    )
    .await?;
    require_machine_target(subject.as_ref(), &agent.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STARTUP_MANAGE,
        agent_policy_resource(&agent),
    )
    .await?;
    if agent.kind == "app"
        || auth
            .resolve_app_deployment_by_runtime(&workspace_id, &agent.agent_id)
            .await?
            .is_some()
    {
        return Err(ApiFailure::bad_request(
            "app_startup_managed",
            "App startup is managed by its deployment; edit the App deployment instead",
        ));
    }
    if restart {
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_STOP,
            agent_policy_resource(&agent),
        )
        .await?;
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_CREATE,
            PolicyResource::new(RESOURCE_MACHINE, &agent.server_id),
        )
        .await?;
    }
    let Some(request) = request else {
        // A read needs no lifecycle guard: an offline Controller must not hold
        // the shared mutation lock while the request waits for its response.
        return Ok(Json(
            state
                .send_command(
                    &workspace_id,
                    &agent.server_id,
                    AgentCommand::StartupAdminGet {
                        agent_id: agent.agent_id,
                    },
                )
                .await?,
        ));
    };
    request.validate()?;
    // Serialize with start/stop/delete across Proxy requests. Controller holds
    // its recovery lock across persistence and restart as well.
    let guard = auth
        .workload_lifecycle_guard(&workspace_id, &agent.agent_id)
        .await?;
    let command = if restart {
        AgentCommand::RestartWithStartup {
            agent_id: agent.agent_id.clone(),
            request,
        }
    } else {
        AgentCommand::StartupAdminSet {
            agent_id: agent.agent_id.clone(),
            request,
        }
    };
    // Saving alone preserves stopped intent. Explicit restart records running
    // intent before dispatch, matching the existing lifecycle recovery path.
    if restart {
        auth.set_agent_recovery_enabled(&workspace_id, &agent.agent_id, true)
            .await?;
    }
    let data = state
        .send_command(&workspace_id, &agent.server_id, command)
        .await?;
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
            action: if restart {
                "agent.startup.restarted"
            } else {
                "agent.startup.admin_set"
            },
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({"server_id": &agent.server_id}),
        })
        .await
    {
        tracing::warn!(?error, "failed to record startup management audit");
    }
    Ok(Json(data))
}

macro_rules! admin_startup_route {
    ($name:ident, $restart:expr) => {
        #[allow(clippy::too_many_arguments)]
        pub(crate) async fn $name(
            State(state): State<AppState>,
            Extension(auth): Extension<AuthStore>,
            Extension(policy): Extension<PolicyEngine>,
            session: Option<Extension<CurrentSession>>,
            machine: Option<Extension<MachineSession>>,
            headers: HeaderMap,
            Path((workspace_id, target)): Path<(String, String)>,
            Json(request): Json<ReplaceAgentStartupRequest>,
        ) -> Result<Json<Value>, ApiFailure> {
            admin_startup_handler(
                state,
                auth,
                policy,
                session,
                machine,
                headers,
                workspace_id,
                target,
                Some(request),
                $restart,
            )
            .await
        }
    };
}
admin_startup_route!(set_admin_agent_startup, false);
admin_startup_route!(restart_agent_with_startup, true);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_admin_agent_startup(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    admin_startup_handler(
        state,
        auth,
        policy,
        session,
        machine,
        headers,
        workspace_id,
        target,
        None,
        false,
    )
    .await
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
    auth.set_agent_recovery_enabled(&workspace_id, &agent_id, true)
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
    auth.set_agent_recovery_enabled(&workspace_id, &agent_id, false)
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
    let stopped_agent_ids = auth
        .stopped_agent_recovery_ids(&workspace_id, &server_id, &request.agent_ids)
        .await?;
    let active_agent_ids: Vec<String> = auth
        .active_agent_ids(&workspace_id, &server_id, &request.agent_ids)
        .await?
        .into_iter()
        .filter(|id| !stopped_agent_ids.contains(id))
        .collect();
    let app_startups = auth
        .list_app_deployments_for_server(&workspace_id, &server_id)
        .await?
        .into_iter()
        .filter_map(|app| {
            let id = app
                .runtime_agent_id
                .clone()
                .unwrap_or_else(|| app.app_id.clone());
            if app.startup_revision == 0
                || !request.agent_ids.contains(&id)
                || !(active_agent_ids.contains(&id) || stopped_agent_ids.contains(&id))
            {
                return None;
            }
            Some(treer_protocol::AppStartupRevision {
                agent_id: id,
                revision: app.startup_revision,
                request: ReplaceAgentStartupRequest {
                    cwd: Some(app.cwd),
                    command: app.command,
                    args: app.args,
                },
            })
        })
        .collect();
    Ok(Json(ValidateAgentStartupResponse {
        app_startups,
        stopped_agent_ids,
        active_agent_ids,
    }))
}
