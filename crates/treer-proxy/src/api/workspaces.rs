use super::*;

#[derive(Deserialize)]
pub(super) struct ListWorkspacesQuery {
    organization_id: String,
}

#[derive(Deserialize)]
pub(super) struct CreateWorkspaceApiRequest {
    organization_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    name: String,
}

pub(super) async fn list_workspaces(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Query(query): Query<ListWorkspacesQuery>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "workspaces": auth
            .list_workspaces(&query.organization_id, &session.user_id)
            .await?
    })))
}

pub(super) async fn create_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Json(request): Json<CreateWorkspaceApiRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = request
        .workspace_id
        .unwrap_or_else(|| format!("ws_{}", Uuid::new_v4().simple()));
    let info = auth
        .create_workspace(
            &request.organization_id,
            &workspace_id,
            &request.name,
            &session.user_id,
        )
        .await?;
    state.create_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

pub(super) async fn rename_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let info = auth
        .rename_workspace(&workspace_id, &session.user_id, &request.name)
        .await?;
    state.rename_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

pub(super) async fn delete_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let deleted = auth
        .delete_workspace(&workspace_id, &session.user_id)
        .await?;
    state.delete_workspace(&workspace_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(serde_json::to_value(&deleted)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn workspace_snapshot(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(ingress): Extension<IngressConfig>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let mut snapshot = state.snapshot(&workspace_id).await?;
    let tree = auth.workload_tree(&workspace_id).await?;
    for agent in &mut snapshot.agents {
        agent.path = Some(tree.workload_path(&agent.agent_id, &agent.name)?);
    }
    let subject = control_policy_subject(
        &state,
        machine.as_ref().map(|value| &value.0),
        &headers,
        &workspace_id,
    )
    .await?;
    if let Some(PolicySubject::Machine { server_id }) = subject.as_ref() {
        snapshot
            .servers
            .retain(|server| &server.server_id == server_id);
        snapshot
            .agents
            .retain(|agent| &agent.server_id == server_id);
    } else if subject.is_some() {
        let mut visible = Vec::new();
        for agent in snapshot.agents {
            match authorize_control(
                &policy,
                &workspace_id,
                subject.as_ref(),
                ACTION_AGENT_DISCOVER,
                agent_policy_resource(&agent),
            )
            .await
            {
                Ok(()) => visible.push(agent),
                Err(error) if error.error.code == "policy_denied" => {}
                Err(error) => return Err(error),
            }
        }
        snapshot.agents = visible;
    }
    // Humans are not control-route Policy subjects (membership admits them),
    // but the interface origin is gated per human (#143 Phase 2).
    if let Some(session) = session.as_deref() {
        let human = PolicySubject::Human {
            user_id: session.user_id.clone(),
        };
        for agent in &mut snapshot.agents {
            super::network::hydrate_agent_interface_origin(
                &policy,
                &ingress,
                &workspace_id,
                Some(&human),
                agent,
            )
            .await?;
        }
    }
    let view = super::inventory_view::inventory_view(&headers);
    Ok(Json(super::inventory_view::snapshot_value(
        view, &snapshot,
    )?))
}
