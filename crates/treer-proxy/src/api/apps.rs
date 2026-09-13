use super::*;

pub(super) async fn list_servers(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    Ok(Json(json!({ "servers": snapshot.servers })))
}

#[derive(Debug, Deserialize)]
pub(super) struct MachineTrafficQuery {
    #[serde(default = "default_traffic_hours")]
    hours: u16,
}

const fn default_traffic_hours() -> u16 {
    24
}

pub(super) async fn list_machine_traffic(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    Query(query): Query<MachineTrafficQuery>,
) -> Result<Json<Value>, ApiFailure> {
    if !(1..=24 * 30).contains(&query.hours) {
        return Err(ApiFailure::bad_request(
            "invalid_traffic_window",
            "traffic window must be between 1 and 720 hours",
        ));
    }
    let traffic = state
        .recent_machine_traffic(&workspace_id, query.hours)
        .await
        .map_err(|error| ApiFailure::internal("traffic_query_failed", &format!("{error:#}")))?;
    Ok(Json(json!({
        "hours": query.hours,
        "traffic": traffic,
    })))
}

pub(super) async fn list_agent_traffic(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    Query(query): Query<MachineTrafficQuery>,
) -> Result<Json<Value>, ApiFailure> {
    if !(1..=24 * 30).contains(&query.hours) {
        return Err(ApiFailure::bad_request(
            "invalid_traffic_window",
            "traffic window must be between 1 and 720 hours",
        ));
    }
    let traffic = state
        .recent_agent_traffic(&workspace_id, query.hours)
        .await
        .map_err(|error| ApiFailure::internal("traffic_query_failed", &format!("{error:#}")))?;
    Ok(Json(json!({"hours": query.hours, "traffic": traffic})))
}

pub(super) async fn hydrate_app_deployment(state: &AppState, app: &mut AppDeployment) {
    if app.desired_state == AppDesiredState::Stopped {
        app.status = AppDeploymentStatus::Stopped;
        return;
    }
    if let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() {
        if let Ok(agent) = state
            .resolve_agent(&app.workspace_id, runtime_agent_id)
            .await
        {
            app.pid = agent.pid;
            app.exit_code = agent.exit_code;
            app.status = if agent.status.is_terminal() {
                AppDeploymentStatus::Exited
            } else {
                AppDeploymentStatus::Running
            };
            return;
        }
    }
    app.status = if app.last_error.is_some() {
        AppDeploymentStatus::Unavailable
    } else {
        match state
            .resolve_server(&app.workspace_id, &app.server_id)
            .await
        {
            Ok(server) if server.status == ServerStatus::Online => AppDeploymentStatus::Pending,
            _ => AppDeploymentStatus::Unavailable,
        }
    };
}

pub(super) fn attach_app_public_url(
    config: &IngressConfig,
    ingresses: &[ServiceIngress],
    app: &mut AppDeployment,
) {
    let managed_hostname = config.base_domain_if_configured().and_then(|base_domain| {
        managed_app_ingress_hostname(&app.name, &app.app_id, base_domain).ok()
    });
    let ingress = ingresses.iter().find(|ingress| {
        managed_hostname.as_deref() == Some(ingress.hostname.as_str())
            && ingress.service_id == app.service_id
            && ingress.enabled
    });
    app.access = ingress.map(|ingress| ingress.access);
    app.public_url = ingress
        .and_then(|ingress| config.url_for_hostname(&ingress.hostname).ok())
        .map(|url| url.to_string());
}

pub(super) async fn hydrate_app_public_url(
    auth: &AuthStore,
    config: &IngressConfig,
    app: &mut AppDeployment,
) -> Result<(), ApiFailure> {
    let ingresses = auth.list_service_ingresses(&app.workspace_id).await?;
    attach_app_public_url(config, &ingresses, app);
    Ok(())
}

pub(super) async fn list_app_deployments(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let mut apps = auth.list_app_deployments(&workspace_id).await?;
    let ingresses = auth.list_service_ingresses(&workspace_id).await?;
    for app in &mut apps {
        hydrate_app_deployment(&state, app).await;
        attach_app_public_url(&config, &ingresses, app);
    }
    Ok(Json(json!({ "apps": apps })))
}

pub(super) async fn get_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let mut app = auth.resolve_app_deployment(&workspace_id, &target).await?;
    hydrate_app_deployment(&state, &mut app).await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateAppAccessRequest {
    access: ServiceIngressAccess,
}

pub(super) async fn update_app_access(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<UpdateAppAccessRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let mut app = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let ingress = auth
        .set_app_ingress_access(
            &app,
            &session.user_id,
            config.base_domain()?,
            request.access,
        )
        .await?;
    hydrate_app_deployment(&state, &mut app).await;
    attach_app_public_url(&config, &[ingress], &mut app);
    record_app_audit(&auth, Some(&session), None, "app.access.updated", &app).await;
    Ok(Json(json!({ "app": app })))
}

pub(super) fn app_mutation_actor<'a>(
    session: Option<&'a CurrentSession>,
    subject: Option<&'a PolicySubject>,
) -> &'a str {
    session.map_or_else(
        || match subject {
            Some(PolicySubject::Agent { agent_id, .. }) => agent_id.as_str(),
            Some(PolicySubject::Machine { server_id }) => server_id.as_str(),
            Some(PolicySubject::Human { user_id }) => user_id.as_str(),
            Some(PolicySubject::Service { service_id }) => service_id.as_str(),
            None => "system",
        },
        |session| session.user_id.as_str(),
    )
}

pub(super) async fn record_app_audit(
    auth: &AuthStore,
    session: Option<&CurrentSession>,
    subject: Option<&PolicySubject>,
    action: &'static str,
    app: &AppDeployment,
) {
    let (actor_kind, actor_id) = control_audit_actor(session, subject);
    if let Err(error) = auth
        .record_workspace_audit(NewWorkspaceAuditEvent {
            workspace_id: &app.workspace_id,
            actor_kind,
            actor_id,
            action,
            resource_kind: "app",
            resource_id: &app.app_id,
            resource_name: Some(&app.name),
            payload: json!({
                "server_id": &app.server_id,
                "service_id": &app.service_id,
                "hostname": &app.hostname,
                "access": app.access,
            }),
        })
        .await
    {
        tracing::warn!(?error, app_id = %app.app_id, action, "failed to record App audit event");
    }
}

pub(super) async fn launch_app_runtime(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
    actor: &str,
) -> Result<AppDeployment, ApiFailure> {
    let runtime_agent_id = format!("appw_{}", Uuid::new_v4().simple());
    let Some(claimed) = auth
        .claim_app_runtime(
            &app.workspace_id,
            &app.app_id,
            app.runtime_agent_id.as_deref(),
            &runtime_agent_id,
            actor,
        )
        .await?
    else {
        return auth
            .resolve_app_deployment(&app.workspace_id, &app.app_id)
            .await
            .map_err(Into::into);
    };
    let workload_credential = auth
        .create_agent_credential(&app.workspace_id, &app.server_id, &runtime_agent_id)
        .await?;
    let mut args = Vec::with_capacity(app.args.len() + 1);
    args.push(app.command.clone());
    args.extend(app.args.clone());
    let result = state
        .send_command(
            &app.workspace_id,
            &app.server_id,
            AgentCommand::Create {
                agent_id: runtime_agent_id.clone(),
                workload_credential,
                request: CreateAgentRequest {
                    server_id: Some(app.server_id.clone()),
                    kind: "app".to_string(),
                    name: format!("app:{}", app.name),
                    cwd: app.cwd.clone(),
                    args,
                    cols: 120,
                    rows: 36,
                    publish_ports: vec![app.port],
                    recipe: None,
                },
            },
        )
        .await;
    if let Err(error) = result {
        auth.set_app_last_error(&app.workspace_id, &app.app_id, Some(&error.message))
            .await?;
        let _ = auth
            .delete_agent(&app.workspace_id, &runtime_agent_id)
            .await;
        return Err(error.into());
    }
    Ok(claimed)
}

pub(super) async fn stop_app_runtime(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
) -> Result<(), ApiFailure> {
    let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() else {
        return Ok(());
    };
    let agent = state
        .resolve_agent(&app.workspace_id, runtime_agent_id)
        .await
        .ok();
    if let Some(agent) = agent.as_ref() {
        if !agent.status.is_terminal() {
            state
                .send_command(
                    &app.workspace_id,
                    &app.server_id,
                    AgentCommand::Stop {
                        agent_id: runtime_agent_id.to_string(),
                    },
                )
                .await?;
        }
    }
    auth.delete_agent(&app.workspace_id, runtime_agent_id)
        .await?;
    if agent.is_some() {
        let _ = state
            .delete_agent(&app.workspace_id, runtime_agent_id)
            .await;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn create_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateAppDeploymentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress_access = if request.public {
        ServiceIngressAccess::Public
    } else {
        ServiceIngressAccess::Workspace
    };
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    let server_id = state
        .select_server(&workspace_id, request.server_id.as_deref())
        .await?;
    request.server_id = Some(server_id.clone());
    require_machine_target(subject.as_ref(), &server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &server_id),
    )
    .await?;
    let ingress_base_domain = if request.public {
        Some(config.base_domain()?)
    } else {
        config.base_domain_if_configured()
    };
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let mut app = auth
        .create_app_deployment(&workspace_id, actor, server_id, request)
        .await?;
    if let Some(base_domain) = ingress_base_domain {
        if let Err(error) = auth
            .ensure_app_ingress(&app, actor, base_domain, Some(ingress_access))
            .await
        {
            let _ = auth.delete_app_deployment(&workspace_id, &app.app_id).await;
            publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
            return Err(error.into());
        }
    }
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    match launch_app_runtime(&state, &auth, &app, actor).await {
        Ok(started) => app = started,
        Err(error) => {
            tracing::warn!(?error, app_id = %app.app_id, "App deployment created but its runtime did not start");
            app = auth
                .resolve_app_deployment(&workspace_id, &app.app_id)
                .await?;
        }
    }
    hydrate_app_deployment(&state, &mut app).await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.created",
        &app,
    )
    .await;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn start_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let current = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Running,
            actor,
        )
        .await?;
    let running = if let Some(runtime_agent_id) = current.runtime_agent_id.as_deref() {
        state
            .resolve_agent(&workspace_id, runtime_agent_id)
            .await
            .is_ok_and(|agent| !agent.status.is_terminal())
    } else {
        false
    };
    let mut app = if running {
        current
    } else {
        launch_app_runtime(&state, &auth, &current, actor).await?
    };
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.started",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stop_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let mut app = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Stopped,
            actor,
        )
        .await?;
    stop_app_runtime(&state, &auth, &app).await?;
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.stopped",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn restart_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let current = auth
        .set_app_desired_state(
            &workspace_id,
            &current.app_id,
            AppDesiredState::Running,
            actor,
        )
        .await?;
    stop_app_runtime(&state, &auth, &current).await?;
    let mut app = launch_app_runtime(&state, &auth, &current, actor).await?;
    hydrate_app_deployment(&state, &mut app).await;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.restarted",
        &app,
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn delete_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let current = auth.resolve_app_deployment(&workspace_id, &target).await?;
    let mut response_app = current.clone();
    hydrate_app_public_url(&auth, &config, &mut response_app).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_DELETE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    stop_app_runtime(&state, &auth, &current).await?;
    let app = auth
        .delete_app_deployment(&workspace_id, &current.app_id)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.deleted",
        &app,
    )
    .await;
    response_app.status = app.status;
    Ok(Json(json!({ "app": response_app })))
}

pub(crate) async fn reconcile_app_deployments_for_server(
    state: AppState,
    auth: AuthStore,
    workspace_id: String,
    server_id: String,
) {
    let apps = match auth
        .list_app_deployments_for_server(&workspace_id, &server_id)
        .await
    {
        Ok(apps) => apps,
        Err(error) => {
            tracing::warn!(?error, %workspace_id, %server_id, "failed to load App deployments for reconciliation");
            return;
        }
    };
    for app in apps {
        let runtime = if let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() {
            state
                .resolve_agent(&workspace_id, runtime_agent_id)
                .await
                .ok()
        } else {
            None
        };
        if app.desired_state == AppDesiredState::Stopped {
            if runtime
                .as_ref()
                .is_some_and(|agent| !agent.status.is_terminal())
            {
                if let Err(error) = stop_app_runtime(&state, &auth, &app).await {
                    tracing::warn!(?error, app_id = %app.app_id, "failed to stop an undesired App runtime");
                }
            }
            continue;
        }
        if runtime
            .as_ref()
            .is_some_and(|agent| !agent.status.is_terminal())
        {
            continue;
        }
        let pending = runtime.is_none()
            && app.runtime_agent_id.is_some()
            && Utc::now().signed_duration_since(app.updated_at) < chrono::Duration::seconds(15);
        if pending {
            continue;
        }
        if app.runtime_agent_id.is_some() {
            if let Err(error) = stop_app_runtime(&state, &auth, &app).await {
                tracing::warn!(?error, app_id = %app.app_id, "failed to clean up the previous App runtime");
                continue;
            }
        }
        if let Err(error) = launch_app_runtime(&state, &auth, &app, "reconciler").await {
            tracing::warn!(?error, app_id = %app.app_id, "failed to reconcile App runtime");
        }
    }
}
