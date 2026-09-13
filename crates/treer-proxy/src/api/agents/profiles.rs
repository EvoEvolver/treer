use super::*;
pub(crate) async fn list_agent_launch_profiles(
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

pub(crate) async fn get_agent_launch_profile(
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
pub(crate) async fn create_agent_launch_profile(
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
pub(crate) async fn update_agent_launch_profile(
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
pub(crate) async fn delete_agent_launch_profile(
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
pub(crate) async fn launch_agent_profile(
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

pub(crate) fn agent_request_from_launch_profile(
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
