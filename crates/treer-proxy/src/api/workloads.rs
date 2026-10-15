use super::*;

#[derive(Clone, Copy)]
pub(super) enum WorkloadAction {
    Start,
    Stop,
    Restart,
}

impl WorkloadAction {
    fn audit_action(self) -> &'static str {
        match self {
            Self::Start => "agent.started",
            Self::Stop => "agent.stopped",
            Self::Restart => "agent.restarted",
        }
    }
}

// Stop is complete when the Controller confirms there is no live target. Keep
// transport, authorization, and terminal errors fatal so deletion cannot orphan
// a process whose state is unknown. Accept Host codes from older Controllers too.
pub(super) fn stop_target_is_absent(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "agent_not_found" | "process_not_found" | "process_not_running"
    )
}

pub(super) fn app_workload_agent(app: &AppDeployment) -> AgentInfo {
    AgentInfo {
        path: None,
        agent_id: app
            .runtime_agent_id
            .clone()
            .unwrap_or_else(|| app.app_id.clone()),
        workspace_id: app.workspace_id.clone(),
        server_id: app.server_id.clone(),
        kind: "app".into(),
        name: app.name.clone(),
        cwd: app.cwd.clone(),
        status: treer_protocol::AgentStatus::Exited,
        pid: None,
        run_generation: 0,
        started_at: app.created_at,
        updated_at: app.updated_at,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    }
}

/// Durable App metadata also makes a workload addressable before its first
/// successful Controller snapshot (including an offline stop).
pub(super) async fn resolve_workload_at(
    state: &AppState,
    auth: &AuthStore,
    workspace: &str,
    caller: Option<&str>,
    target: &str,
) -> Result<AgentInfo, ApiFailure> {
    let tree = auth.workload_tree(workspace).await?;
    let id = if target.starts_with("ag_") || target.starts_with("app_") {
        target.to_owned()
    } else {
        let snapshot = state.snapshot(workspace).await?;
        let mut rows = snapshot
            .agents
            .iter()
            .map(|a| treer_protocol::WorkloadTarget {
                id: a.agent_id.clone(),
                name: a.name.clone(),
            })
            .collect::<Vec<_>>();
        for app in auth.list_app_deployments(workspace).await? {
            let id = app.runtime_agent_id.unwrap_or(app.app_id);
            if !rows.iter().any(|r| r.id == id) {
                rows.push(treer_protocol::WorkloadTarget { id, name: app.name });
            }
        }
        tree.resolve_target(&rows, caller, target)?
    };
    let mut agent = match state.resolve_agent(workspace, &id).await {
        Ok(agent) => agent,
        Err(error) if error.code == "agent_not_found" => {
            let app = match auth
                .resolve_app_deployment_by_runtime(workspace, &id)
                .await?
            {
                Some(app) => app,
                None => auth
                    .resolve_app_deployment(workspace, &id)
                    .await
                    .map_err(|e| {
                        if e.code() == "app_not_found" {
                            ApiFailure::from(error)
                        } else {
                            ApiFailure::from(e)
                        }
                    })?,
            };
            app_workload_agent(&app)
        }
        Err(error) => return Err(error.into()),
    };
    agent.path = Some(tree.workload_path(&agent.agent_id, &agent.name)?);
    Ok(agent)
}

/// Send the saved App launch declaration to the same Controller lifecycle used
/// by every Agent. This is also the migration path for pre-unification Hosts.
pub(super) async fn ensure_app_workload(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
    start_missing: bool,
) -> Result<AppDeployment, ApiFailure> {
    let app = auth
        .bind_app_workload(&app.workspace_id, &app.app_id)
        .await?;
    let id = app.runtime_agent_id.as_deref().expect("bound App identity");
    let credential = match auth
        .create_agent_credential(&app.workspace_id, &app.server_id, id)
        .await
    {
        Ok(credential) => Some(credential),
        Err(error) if error.code() == "agent_credential_exists" => None,
        Err(error) => return Err(error.into()),
    };
    if app.desired_state == AppDesiredState::Stopped {
        auth.set_agent_recovery_enabled(&app.workspace_id, id, false)
            .await?;
    }
    let mut args = vec![app.command.clone()];
    args.extend(app.args.clone());
    let request = CreateAgentRequest {
        folder_id: None,
        server_id: Some(app.server_id.clone()),
        kind: "app".into(),
        name: app.name.clone(),
        cwd: app.cwd.clone(),
        args,
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };
    let command = |credential| {
        if app.startup_revision > 0 {
            AgentCommand::AppEnsureLifecycle {
                agent_id: id.to_owned(),
                workload_credential: credential,
                request: request.clone(),
                enabled: app.desired_state == AppDesiredState::Running,
                start_missing,
                revision: app.startup_revision,
            }
        } else {
            AgentCommand::EnsureLifecycle {
                agent_id: id.to_string(),
                workload_credential: credential,
                request: request.clone(),
                enabled: app.desired_state == AppDesiredState::Running,
                start_missing,
            }
        }
    };
    let mut result = state
        .send_command(&app.workspace_id, &app.server_id, command(credential))
        .await;
    if result
        .as_ref()
        .is_err_and(|error| error.code == "lifecycle_credential_missing")
    {
        let credential = auth
            .rotate_agent_credential(&app.workspace_id, &app.server_id, id)
            .await?;
        result = state
            .send_command(&app.workspace_id, &app.server_id, command(Some(credential)))
            .await;
    }
    if let Err(error) = result {
        auth.set_app_last_error(&app.workspace_id, &app.app_id, Some(&error.message))
            .await?;
        return Err(error.into());
    }
    auth.set_app_last_error(&app.workspace_id, &app.app_id, None)
        .await?;
    Ok(app)
}

pub(super) async fn apply_workload_action(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
    agent: &AgentInfo,
    action: WorkloadAction,
    actor: &str,
) -> Result<Value, ApiFailure> {
    let guard = auth
        .workload_lifecycle_guard(workspace_id, &agent.agent_id)
        .await?;
    let enabled = !matches!(action, WorkloadAction::Stop);
    // Central stopped intent is committed before dispatch so it survives a
    // disconnected Controller or a lost acknowledgement.
    auth.set_agent_recovery_enabled(workspace_id, &agent.agent_id, enabled)
        .await?;
    let mut agent_id = agent.agent_id.clone();
    let mut app = auth
        .resolve_app_deployment_by_runtime(workspace_id, &agent.agent_id)
        .await?;
    if app.is_none() && agent.kind == "app" {
        app = match auth
            .resolve_app_deployment(workspace_id, &agent.agent_id)
            .await
        {
            Ok(app) => Some(app),
            Err(error) if error.code() == "app_not_found" => None,
            Err(error) => return Err(error.into()),
        };
    }
    if let Some(app) = app {
        let app = auth.bind_app_workload(workspace_id, &app.app_id).await?;
        agent_id = app.runtime_agent_id.clone().expect("bound App identity");
        let app = auth
            .set_app_desired_state(
                workspace_id,
                &app.app_id,
                if enabled {
                    AppDesiredState::Running
                } else {
                    AppDesiredState::Stopped
                },
                actor,
            )
            .await?;
        if !matches!(action, WorkloadAction::Stop) {
            ensure_app_workload(state, auth, &app, false).await?;
        }
        auth.set_agent_recovery_enabled(workspace_id, &agent_id, enabled)
            .await?;
    }
    let command = match action {
        WorkloadAction::Start => AgentCommand::Start { agent_id },
        WorkloadAction::Stop => AgentCommand::Stop { agent_id },
        WorkloadAction::Restart => AgentCommand::Restart { agent_id },
    };
    let result = state
        .send_command(workspace_id, &agent.server_id, command)
        .await;
    let result = match result {
        Ok(value) => value,
        Err(error) if matches!(action, WorkloadAction::Stop) && stop_target_is_absent(&error) => {
            serde_json::to_value(agent)?
        }
        Err(error) => return Err(error.into()),
    };
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    Ok(result)
}

pub(super) async fn remove_app_workload(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
) -> Result<AppDeployment, ApiFailure> {
    let id = app.runtime_agent_id.as_deref().unwrap_or(&app.app_id);
    let guard = auth.workload_lifecycle_guard(&app.workspace_id, id).await?;
    let stopped = auth
        .set_app_desired_state(
            &app.workspace_id,
            &app.app_id,
            AppDesiredState::Stopped,
            "system",
        )
        .await?;
    auth.set_agent_recovery_enabled(&app.workspace_id, id, false)
        .await?;
    stop_app_runtime(state, auth, &stopped).await?;
    // Revoke before deleting the service metadata, so a saved Controller spec
    // can never recover this identity after reconnect.
    auth.delete_agent(&app.workspace_id, id).await?;
    let deleted = auth
        .delete_app_deployment(&app.workspace_id, &app.app_id)
        .await?;
    let _ = state.delete_agent(&app.workspace_id, id).await;
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    Ok(deleted)
}

#[allow(clippy::too_many_arguments)]
async fn lifecycle_handler(
    state: AppState,
    auth: AuthStore,
    policy: PolicyEngine,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    workspace_id: String,
    target: String,
    action: WorkloadAction,
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
    if matches!(action, WorkloadAction::Restart | WorkloadAction::Stop) {
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_STOP,
            agent_policy_resource(&agent),
        )
        .await?;
    }
    if !matches!(action, WorkloadAction::Stop) {
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_CREATE,
            PolicyResource::new(RESOURCE_MACHINE, &agent.server_id),
        )
        .await?;
    }
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let data = apply_workload_action(&state, &auth, &workspace_id, &agent, action, actor).await?;
    let (actor_kind, actor_id) = control_audit_actor(session.as_deref(), subject.as_ref());
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind,
            actor_id,
            action: action.audit_action(),
            resource_kind: "agent",
            resource_id: &agent.agent_id,
            resource_name: Some(&agent.name),
            payload: json!({"server_id": &agent.server_id}),
        })
        .await
    {
        tracing::warn!(?error, "failed to record workload lifecycle audit");
    }
    Ok(Json(data))
}

macro_rules! lifecycle_route {
    ($name:ident, $action:ident) => {
        #[allow(clippy::too_many_arguments)]
        pub(super) async fn $name(
            State(state): State<AppState>,
            Extension(auth): Extension<AuthStore>,
            Extension(policy): Extension<PolicyEngine>,
            session: Option<Extension<CurrentSession>>,
            machine: Option<Extension<MachineSession>>,
            headers: HeaderMap,
            Path((workspace_id, target)): Path<(String, String)>,
        ) -> Result<Json<Value>, ApiFailure> {
            lifecycle_handler(
                state,
                auth,
                policy,
                session,
                machine,
                headers,
                workspace_id,
                target,
                WorkloadAction::$action,
            )
            .await
        }
    };
}

lifecycle_route!(start_agent, Start);
lifecycle_route!(restart_agent, Restart);
