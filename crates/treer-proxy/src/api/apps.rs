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

pub(super) async fn hydrate_app_deployment(
    state: &AppState,
    app: &mut AppDeployment,
    view: super::inventory_view::InventoryView,
) {
    if app.desired_state == AppDesiredState::Stopped {
        app.status = AppDeploymentStatus::Stopped;
        return;
    }
    if let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() {
        if let Ok(agent) = state
            .resolve_agent(&app.workspace_id, runtime_agent_id)
            .await
        {
            // The runtime pid follows the Agent inventory rule (#153): the
            // browser view never carries it.
            app.pid = match view {
                super::inventory_view::InventoryView::Browser => None,
                super::inventory_view::InventoryView::Trusted => agent.pid,
            };
            app.exit_code = agent.exit_code;
            app.restart_count = agent.run_generation.saturating_sub(1);
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
        crate::auth::platform_service_hostname(&app.service_id, base_domain).ok()
    });
    let ingress = ingresses.iter().find(|ingress| {
        managed_hostname.as_deref() == Some(ingress.hostname.as_str())
            && ingress.service_id.as_deref() == Some(app.service_id.as_str())
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
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let view = super::inventory_view::inventory_view(&headers, machine.is_some());
    let mut apps = auth.list_app_deployments(&workspace_id).await?;
    let ingresses = auth.list_service_ingresses(&workspace_id).await?;
    for app in &mut apps {
        hydrate_app_deployment(&state, app, view).await;
        attach_app_public_url(&config, &ingresses, app);
    }
    Ok(Json(json!({ "apps": apps })))
}

async fn resolve_app_target(
    state: &AppState,
    auth: &AuthStore,
    workspace: &str,
    headers: &HeaderMap,
    target: &str,
) -> Result<AppDeployment, ApiFailure> {
    let caller = headers.get(AGENT_ID_HEADER).and_then(|v| v.to_str().ok());
    let agent = resolve_workload_at(state, auth, workspace, caller, target).await?;
    Ok(auth
        .resolve_app_deployment(workspace, &agent.agent_id)
        .await?)
}

pub(super) async fn get_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let mut app = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, false),
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({ "app": app })))
}

#[derive(Debug, Deserialize)]
pub(super) struct UpdateAppAccessRequest {
    access: ServiceIngressAccess,
}

/// `PATCH /api/workspaces/{ws}/apps/{app}/ingress-limits` (#146): workspace
/// owners adjust the public ingress protection of a Managed App within the
/// hard ceilings. Nothing here can switch protection off.
pub(super) async fn update_app_ingress_limits(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(session): Extension<CurrentSession>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<treer_protocol::UpdateIngressLimitsRequest>,
) -> Result<Json<Value>, ApiFailure> {
    auth.require_workspace_owner(&workspace_id, &session.user_id)
        .await?;
    let app = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
    let hostname = crate::auth::platform_service_hostname(&app.service_id, config.base_domain()?)?;
    let ingress = auth
        .list_service_ingresses(&workspace_id)
        .await?
        .into_iter()
        .find(|ingress| {
            ingress.service_id.as_deref() == Some(app.service_id.as_str())
                && ingress.hostname == hostname
        })
        .ok_or_else(|| {
            ApiFailure::not_found("ingress_not_found", "the App has no managed ingress")
        })?;
    let updated = auth
        .update_service_ingress_limits(
            &workspace_id,
            &ingress.ingress_id,
            &session.user_id,
            &request,
        )
        .await?;
    Ok(Json(json!({
        "ingress": super::network::service_ingress_json(&config, updated)?
    })))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn update_app_access(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(provider_store): Extension<crate::policy_provider_store::PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<UpdateAppAccessRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let mut app = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
    if request.access == ServiceIngressAccess::Public
        && provider_store
            .get(&workspace_id)
            .await
            .map_err(|error| {
                ApiFailure::internal("policy_provider_store_failed", &error.to_string())
            })?
            .is_some_and(|provider| provider.app_id == app.app_id)
    {
        return Err(ApiFailure::bad_request(
            "policy_provider_must_be_private",
            "the active Policy Provider App must require a workspace session",
        ));
    }
    let ingress = auth
        .set_app_ingress_access(
            &app,
            &session.user_id,
            config.base_domain()?,
            request.access,
        )
        .await?;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, false),
    )
    .await;
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
                "startup_revision": app.startup_revision,
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
    _actor: &str,
) -> Result<AppDeployment, ApiFailure> {
    let app = auth
        .bind_app_workload(&app.workspace_id, &app.app_id)
        .await?;
    let guard = auth
        .workload_lifecycle_guard(&app.workspace_id, app.runtime_agent_id.as_deref().unwrap())
        .await?;
    let app = ensure_app_workload(state, auth, &app, true).await?;
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    Ok(app)
}

pub(super) async fn stop_app_runtime(
    state: &AppState,
    _auth: &AuthStore,
    app: &AppDeployment,
) -> Result<(), ApiFailure> {
    let Some(runtime_agent_id) = app.runtime_agent_id.as_deref() else {
        return Ok(());
    };
    match state
        .send_command(
            &app.workspace_id,
            &app.server_id,
            AgentCommand::Stop {
                agent_id: runtime_agent_id.to_string(),
            },
        )
        .await
    {
        Ok(_) => {}
        Err(error) if stop_target_is_absent(&error) => {}
        Err(error) => return Err(error.into()),
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
    if request.folder_id.is_some() {
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            crate::policy::ACTION_AGENT_TREE_MANAGE,
            PolicyResource::new(crate::policy::RESOURCE_AGENT_TREE, &workspace_id),
        )
        .await?;
    }
    state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::CreationPlacementCheck {},
        )
        .await?;
    let mut app = auth
        .create_app_deployment_for_caller(
            &workspace_id,
            actor,
            server_id,
            request,
            match subject.as_ref() {
                Some(PolicySubject::Agent { agent_id, .. }) => Some(agent_id.as_str()),
                _ => None,
            },
        )
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
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, machine.is_some()),
    )
    .await;
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
    let current = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
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
    apply_workload_action(
        &state,
        &auth,
        &workspace_id,
        &app_workload_agent(&current),
        WorkloadAction::Start,
        actor,
    )
    .await?;
    let mut app = auth
        .resolve_app_deployment(&workspace_id, &current.app_id)
        .await?;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, machine.is_some()),
    )
    .await;
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
    let current = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
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
        agent_policy_resource(&app_workload_agent(&current)),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    apply_workload_action(
        &state,
        &auth,
        &workspace_id,
        &app_workload_agent(&current),
        WorkloadAction::Stop,
        actor,
    )
    .await?;
    let mut app = auth
        .resolve_app_deployment(&workspace_id, &current.app_id)
        .await?;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, machine.is_some()),
    )
    .await;
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
    let current = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
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
        agent_policy_resource(&app_workload_agent(&current)),
    )
    .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    apply_workload_action(
        &state,
        &auth,
        &workspace_id,
        &app_workload_agent(&current),
        WorkloadAction::Restart,
        actor,
    )
    .await?;
    let mut app = auth
        .resolve_app_deployment(&workspace_id, &current.app_id)
        .await?;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, machine.is_some()),
    )
    .await;
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
pub(super) async fn update_app_deployment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(patch): Json<treer_protocol::UpdateAppDeploymentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let current = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    require_machine_target(subject.as_ref(), &current.server_id)?;
    let resource = agent_policy_resource(&app_workload_agent(&current));
    if !patch.changes_startup() {
        // Capability-only patch: presentation metadata, no process replacement.
        let Some(capabilities) = patch.ui_capabilities.as_deref() else {
            return Err(ProtocolError::new(
                "invalid_app_update",
                "at least one launch field or ui_capabilities is required",
            )
            .into());
        };
        authorize_control(
            &policy,
            &workspace_id,
            subject.as_ref(),
            ACTION_AGENT_UPDATE,
            resource,
        )
        .await?;
        let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
        let mut app = auth
            .set_app_ui_capabilities(&workspace_id, &current.app_id, capabilities, actor)
            .await?;
        hydrate_app_deployment(
            &state,
            &mut app,
            super::inventory_view::inventory_view(&headers, machine.is_some()),
        )
        .await;
        hydrate_app_public_url(&auth, &config, &mut app).await?;
        return Ok(Json(json!({"app": app})));
    }
    if let Some(capabilities) = patch.ui_capabilities.as_deref() {
        treer_protocol::normalize_app_ui_capabilities(capabilities)
            .map_err(|error| ApiFailure::bad_request(&error.code, &error.message))?;
    }
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STARTUP_MANAGE,
        resource.clone(),
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_STOP,
        resource,
    )
    .await?;
    authorize_control(
        &policy,
        &workspace_id,
        subject.as_ref(),
        ACTION_AGENT_CREATE,
        PolicyResource::new(RESOURCE_MACHINE, &current.server_id),
    )
    .await?;
    let id = current
        .runtime_agent_id
        .as_deref()
        .unwrap_or(&current.app_id);
    let guard = auth.workload_lifecycle_guard(&workspace_id, id).await?;
    let current = auth
        .resolve_app_deployment(&workspace_id, &current.app_id)
        .await?;
    let request = patch.apply(&current)?;
    // Negotiated, side-effect-free preflight before saving anything. An old
    // Controller must never silently accept a spec it cannot apply.
    state
        .send_command(
            &workspace_id,
            &current.server_id,
            AgentCommand::AppStartupCheck {
                agent_id: id.to_owned(),
                request: request.clone(),
            },
        )
        .await?;
    let actor = app_mutation_actor(session.as_deref(), subject.as_ref());
    let mut app = auth
        .update_app_startup(&workspace_id, &current.app_id, &request, actor)
        .await?;
    if let Some(capabilities) = patch.ui_capabilities.as_deref() {
        app = auth
            .set_app_ui_capabilities(&workspace_id, &app.app_id, capabilities, actor)
            .await?;
    }
    // Audit the committed change even if spawn/transport later fails. No argv,
    // credentials or file contents are included in this event.
    record_app_audit(
        &auth,
        session.as_deref(),
        subject.as_ref(),
        "app.startup.updated",
        &app,
    )
    .await;
    let result = state
        .send_command(
            &workspace_id,
            &app.server_id,
            AgentCommand::AppStartupUpdate {
                agent_id: id.to_owned(),
                request,
                revision: app.startup_revision,
            },
        )
        .await;
    auth.set_app_last_error(
        &workspace_id,
        &app.app_id,
        result
            .as_ref()
            .err()
            .map(|_| "new startup saved; apply failed; repair or retry start/restart"),
    )
    .await?;
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    result.map_err(|error| ProtocolError::new(error.code,
        format!("App startup revision {} was saved, but replacement failed; repair then start/restart, or explicitly update back to the previous specification", app.startup_revision)))?;
    app.last_error = None;
    hydrate_app_deployment(
        &state,
        &mut app,
        super::inventory_view::inventory_view(&headers, machine.is_some()),
    )
    .await;
    hydrate_app_public_url(&auth, &config, &mut app).await?;
    Ok(Json(json!({"app": app})))
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
    let current = resolve_app_target(&state, &auth, &workspace_id, &headers, &target).await?;
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
        agent_policy_resource(&app_workload_agent(&current)),
    )
    .await?;
    let app = remove_app_workload(&state, &auth, &current).await?;
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

pub(crate) async fn synchronize_app_lifecycles_for_server(
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
        // Reconnect only supplies durable launch declarations, including old
        // Apps lacking a local startup snapshot. Controller owns supervision.
        if let Err(error) = synchronize_one_app_lifecycle(&state, &auth, &app).await {
            tracing::warn!(?error, app_id = %app.app_id, "failed to synchronize App lifecycle");
        }
    }
}

async fn synchronize_one_app_lifecycle(
    state: &AppState,
    auth: &AuthStore,
    app: &AppDeployment,
) -> Result<(), ApiFailure> {
    let id = app.runtime_agent_id.as_deref().unwrap_or(&app.app_id);
    let guard = auth.workload_lifecycle_guard(&app.workspace_id, id).await?;
    // Reload inside the same lock used by start/stop/delete. A stale reconnect
    // inventory must never overwrite a newer user decision.
    let app = auth
        .resolve_app_deployment(&app.workspace_id, &app.app_id)
        .await?;
    let app = ensure_app_workload(state, auth, &app, true).await?;
    if app.desired_state == AppDesiredState::Stopped {
        stop_app_runtime(state, auth, &app).await?;
    }
    guard
        .commit()
        .await
        .map_err(|error| ApiFailure::internal("lifecycle_lock_failed", &error.to_string()))?;
    Ok(())
}
