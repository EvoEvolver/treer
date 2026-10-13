use super::*;
use crate::policy::{ACTION_AGENT_TREE_MANAGE, ACTION_AGENT_TREE_READ, RESOURCE_AGENT_TREE};
use treer_protocol::{
    CreateWorkloadFolderRequest, DeleteWorkloadFolderQuery, SetWorkloadPlacementRequest,
    UpdateWorkloadFolderRequest,
};

// Router middleware establishes membership or machine/workload authentication.
#[allow(clippy::too_many_arguments)]
pub(super) async fn authorize_tree(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    session: Option<&CurrentSession>,
    machine: Option<&MachineSession>,
    headers: &HeaderMap,
    workspace: &str,
    action: &str,
) -> Result<(), ApiFailure> {
    if let Some(session) = session {
        auth.require_workspace_member(workspace, &session.user_id)
            .await?;
        if matches!(action, ACTION_AGENT_TREE_MANAGE | ACTION_AGENT_TREE_READ)
            && auth
                .require_workspace_owner(workspace, &session.user_id)
                .await
                .is_ok()
        {
            tracing::info!(workspace_id=workspace,actor_id=%session.user_id,action,"owner Tree break-glass");
            return Ok(());
        }
        policy
            .authorize(&PolicyRequest::new(
                workspace,
                PolicySubject::Human {
                    user_id: session.user_id.clone(),
                },
                action,
                PolicyResource::new(RESOURCE_AGENT_TREE, workspace),
            ))
            .await?;
        return Ok(());
    } else if machine.is_none() {
        return Err(ApiFailure::forbidden(
            "identity_required",
            "authenticated identity is required",
        ));
    }
    let context =
        AgentRequestContext::new(state, policy, auth, machine, headers, workspace).await?;
    context
        .authorize(action, PolicyResource::new(RESOURCE_AGENT_TREE, workspace))
        .await
}

pub(super) async fn get_workload_tree(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace,
        ACTION_AGENT_TREE_READ,
    )
    .await?;
    Ok(Json(json!(auth.workload_tree(&workspace).await?)))
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn create_workload_folder(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path(workspace): Path<String>,
    Json(request): Json<CreateWorkloadFolderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    Ok(Json(
        json!({"folder":auth.create_workload_folder(&workspace,request).await?}),
    ))
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn update_workload_folder(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace, id)): Path<(String, String)>,
    Json(request): Json<UpdateWorkloadFolderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    Ok(Json(
        json!({"folder":auth.update_workload_folder(&workspace,&id,request).await?}),
    ))
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn delete_workload_folder(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace, id)): Path<(String, String)>,
    Query(query): Query<DeleteWorkloadFolderQuery>,
) -> Result<Json<Value>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    if query.require_empty {
        auth.remove_workload_folder(&workspace, &id, true).await?;
    } else {
        auth.delete_workload_folder(&workspace, &id).await?;
    }
    Ok(Json(json!({"ok":true})))
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn set_workload_placement(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    session: Option<Extension<CurrentSession>>,
    machine: Option<Extension<MachineSession>>,
    headers: HeaderMap,
    Path((workspace, id)): Path<(String, String)>,
    Json(request): Json<SetWorkloadPlacementRequest>,
) -> Result<Json<Value>, ApiFailure> {
    authorize_tree(
        &state,
        &auth,
        &policy,
        session.as_deref(),
        machine.as_deref(),
        &headers,
        &workspace,
        ACTION_AGENT_TREE_MANAGE,
    )
    .await?;
    Ok(Json(
        json!({"placement":auth.set_workload_placement(&workspace,&id,request).await?}),
    ))
}
