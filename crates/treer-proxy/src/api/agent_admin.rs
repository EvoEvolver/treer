//! Organization and workspace administration by Agents (`/agent/workspaces/
//! {ws}/admin/...`, reached through the Controller's `/api/org-admin/...`
//! local passthrough). Every route needs the Agent's authenticated workload
//! credential and is evaluated by the Agent's workspace Policy exactly like
//! any other action: with no Policy, in monitor mode or under an allow
//! default it is allowed; an enforced deny rule denies it. Policy is
//! cooperative, not a containment boundary. The organization is always the
//! Agent's own workspace's organization; workspace routes act on the Agent's
//! own workspace only. Deleting or transferring the organization stays a
//! human action.
use super::*;
use crate::auth::{AdminActor, AgentSession};
use crate::policy::{
    ACTION_AUDIT_READ, ACTION_MACHINE_ENROLL, ACTION_ORG_GROUP_MANAGE, ACTION_ORG_INVITE_MANAGE,
    ACTION_ORG_MANAGE, ACTION_ORG_MEMBER_MANAGE, ACTION_POLICY_PROVIDER_MANAGE,
    ACTION_WORKSPACE_ACCESS_MANAGE, ACTION_WORKSPACE_MANAGE, RESOURCE_ORGANIZATION,
    RESOURCE_WORKSPACE,
};
use crate::policy_provider_store::PolicyProviderStore;

type AgentExt = Option<Extension<AgentSession>>;

#[derive(Deserialize)]
pub(super) struct AdminRenameRequest {
    name: String,
}

#[derive(Deserialize)]
pub(super) struct AdminRoleRequest {
    role: String,
}

#[derive(Deserialize)]
pub(super) struct AdminAccessModeRequest {
    access_mode: String,
}

#[derive(Deserialize)]
pub(super) struct AdminCreateWorkspaceRequest {
    #[serde(default)]
    workspace_id: Option<String>,
    name: String,
}

#[derive(Deserialize)]
pub(super) struct AdminAuditQuery {
    workspace_id: Option<String>,
    before: Option<i64>,
    #[serde(default = "default_audit_limit")]
    limit: u16,
}

const fn default_audit_limit() -> u16 {
    50
}

/// Authenticate the Agent and evaluate `action` on `resource` in its own
/// workspace Policy.
async fn authorize_agent_admin(
    policy: &PolicyEngine,
    agent: &AgentExt,
    workspace_id: &str,
    action: &str,
    resource: PolicyResource,
) -> Result<AdminActor, ApiFailure> {
    let Some(Extension(agent)) = agent else {
        return Err(ApiFailure::forbidden(
            "agent_identity_required",
            "administration requires an Agent workload credential",
        ));
    };
    if agent.workspace_id != workspace_id {
        return Err(ApiFailure::forbidden(
            "agent_scope_denied",
            "Agents administer only their own workspace and its organization",
        ));
    }
    policy
        .authorize(&PolicyRequest::new(
            workspace_id,
            PolicySubject::Agent {
                server_id: agent.server_id.clone(),
                agent_id: agent.agent_id.clone(),
            },
            action,
            resource,
        ))
        .await?;
    Ok(AdminActor::Agent {
        agent_id: agent.agent_id.clone(),
        workspace_id: agent.workspace_id.clone(),
    })
}

async fn org_admin(
    auth: &AuthStore,
    policy: &PolicyEngine,
    agent: &AgentExt,
    workspace_id: &str,
    action: &str,
) -> Result<(AdminActor, String), ApiFailure> {
    let organization_id = auth.workspace_organization(workspace_id).await?;
    let actor = authorize_agent_admin(
        policy,
        agent,
        workspace_id,
        action,
        PolicyResource::new(RESOURCE_ORGANIZATION, &organization_id),
    )
    .await?;
    Ok((actor, organization_id))
}

async fn workspace_admin(
    policy: &PolicyEngine,
    agent: &AgentExt,
    workspace_id: &str,
    action: &str,
) -> Result<AdminActor, ApiFailure> {
    authorize_agent_admin(
        policy,
        agent,
        workspace_id,
        action,
        PolicyResource::new(RESOURCE_WORKSPACE, workspace_id),
    )
    .await
}

// ---- Organization -------------------------------------------------------

pub(super) async fn get_organization(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (_, organization_id) =
        org_admin(&auth, &policy, &agent, &workspace_id, ACTION_ORG_MANAGE).await?;
    let row = sqlx::query("SELECT name, created_at FROM organizations WHERE organization_id = $1")
        .bind(&organization_id)
        .fetch_one(&auth.pool())
        .await
        .map_err(|error| ApiFailure::internal("database_error", &error.to_string()))?;
    use sqlx::Row;
    Ok(Json(json!({ "organization": {
        "organization_id": organization_id,
        "name": row.get::<String, _>("name"),
        "created_at": row.get::<String, _>("created_at"),
        "role": "agent",
    }})))
}

pub(super) async fn rename_organization(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<AdminRenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) =
        org_admin(&auth, &policy, &agent, &workspace_id, ACTION_ORG_MANAGE).await?;
    Ok(Json(json!({
        "organization": auth.rename_organization_as(&organization_id, &actor, &request.name).await?
    })))
}

pub(super) async fn list_members(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_MEMBER_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "members": auth.list_members_as(&organization_id, &actor).await?,
        "current_role": "agent",
    })))
}

pub(super) async fn update_member_role(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, user_id)): Path<(String, String)>,
    Json(request): Json<AdminRoleRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_MEMBER_MANAGE,
    )
    .await?;
    auth.update_member_role_as(&organization_id, &actor, &user_id, &request.role)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn remove_member(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, user_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_MEMBER_MANAGE,
    )
    .await?;
    auth.remove_member_as(&organization_id, &actor, &user_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_invitations(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<treer_protocol::InvitationList>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_INVITE_MANAGE,
    )
    .await?;
    Ok(Json(treer_protocol::InvitationList {
        invitations: auth
            .list_organization_invitations(&organization_id, &actor)
            .await?,
    }))
}

pub(super) async fn create_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<treer_protocol::CreateInvitationRequest>,
) -> Result<Response, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_INVITE_MANAGE,
    )
    .await?;
    let created = auth
        .create_organization_invitation(&organization_id, &actor, request)
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(created)).into_response())
}

pub(super) async fn revoke_invitation(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, invitation_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_INVITE_MANAGE,
    )
    .await?;
    auth.revoke_organization_invitation(&organization_id, &actor, &invitation_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list_groups(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_GROUP_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "groups": auth.list_organization_groups_as(&organization_id, &actor).await?
    })))
}

pub(super) async fn create_group(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<AdminRenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_GROUP_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "group": auth.create_organization_group_as(&organization_id, &actor, &request.name).await?
    })))
}

pub(super) async fn delete_group(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, group_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_ORG_GROUP_MANAGE,
    )
    .await?;
    auth.delete_organization_group_as(&organization_id, &actor, &group_id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn set_group_member(
    auth: &AuthStore,
    policy: &PolicyEngine,
    agent: &AgentExt,
    (workspace_id, group_id, user_id): (String, String, String),
    present: bool,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) =
        org_admin(auth, policy, agent, &workspace_id, ACTION_ORG_GROUP_MANAGE).await?;
    Ok(Json(json!({
        "groups": auth
            .set_organization_group_member_as(&organization_id, &actor, &group_id, &user_id, present)
            .await?
    })))
}

pub(super) async fn add_group_member(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(path): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    set_group_member(&auth, &policy, &agent, path, true).await
}

pub(super) async fn remove_group_member(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(path): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    set_group_member(&auth, &policy, &agent, path, false).await
}

pub(super) async fn audit_events(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Query(query): Query<AdminAuditQuery>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) =
        org_admin(&auth, &policy, &agent, &workspace_id, ACTION_AUDIT_READ).await?;
    let events = auth
        .list_audit_events_as(
            &organization_id,
            &actor,
            query.workspace_id.as_deref(),
            query.before,
            query.limit,
        )
        .await?;
    let next_cursor = events.last().map(|event| event.sequence);
    Ok(Json(
        json!({ "events": events, "next_cursor": next_cursor }),
    ))
}

// ---- Workspaces -----------------------------------------------------------

pub(super) async fn list_workspaces(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "organization_id": organization_id,
        "workspaces": auth.list_workspaces_as(&organization_id, &actor).await?,
    })))
}

pub(super) async fn create_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<AdminCreateWorkspaceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let (actor, organization_id) = org_admin(
        &auth,
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_MANAGE,
    )
    .await?;
    let new_workspace_id = request
        .workspace_id
        .unwrap_or_else(|| format!("ws_{}", Uuid::new_v4().simple()));
    let info = auth
        .create_workspace_as(&organization_id, &new_workspace_id, &request.name, &actor)
        .await?;
    state.create_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

pub(super) async fn rename_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<AdminRenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(&policy, &agent, &workspace_id, ACTION_WORKSPACE_MANAGE).await?;
    let info = auth
        .rename_workspace_as(&workspace_id, &actor, &request.name)
        .await?;
    state.rename_workspace_info(info.clone()).await?;
    Ok(Json(json!({ "workspace": info })))
}

pub(super) async fn delete_workspace(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiFailure> {
    let request = auth::parse_optional_json::<treer_protocol::DeleteWorkspaceRequest>(&body, "{}")?;
    let actor = workspace_admin(&policy, &agent, &workspace_id, ACTION_WORKSPACE_MANAGE).await?;
    super::workspaces::delete_workspace_as(&state, &auth, &workspace_id, &actor, &request).await
}

pub(super) async fn workspace_access(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth.workspace_access_info_as(&workspace_id, &actor).await?
    })))
}

pub(super) async fn update_workspace_access(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<AdminAccessModeRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth
            .update_workspace_access_mode_as(&workspace_id, &actor, &request.access_mode)
            .await?
    })))
}

pub(super) async fn effective_workspace_access(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<treer_protocol::EffectiveWorkspaceAccess>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(
        auth.effective_workspace_access(&workspace_id, &actor)
            .await?,
    ))
}

pub(super) async fn put_user_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, user_id)): Path<(String, String)>,
    Json(request): Json<AdminRoleRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth
            .upsert_workspace_user_grant_as(&workspace_id, &actor, &user_id, &request.role)
            .await?
    })))
}

pub(super) async fn delete_user_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, user_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth
            .remove_workspace_user_grant_as(&workspace_id, &actor, &user_id)
            .await?
    })))
}

pub(super) async fn put_group_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, group_id)): Path<(String, String)>,
    Json(request): Json<AdminRoleRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth
            .upsert_workspace_group_grant_as(&workspace_id, &actor, &group_id, &request.role)
            .await?
    })))
}

pub(super) async fn delete_group_grant(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path((workspace_id, group_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_WORKSPACE_ACCESS_MANAGE,
    )
    .await?;
    Ok(Json(json!({
        "access": auth
            .remove_workspace_group_grant_as(&workspace_id, &actor, &group_id)
            .await?
    })))
}

// ---- Policy Provider and machine enrollment -------------------------------

pub(super) async fn get_policy_provider(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_POLICY_PROVIDER_MANAGE,
    )
    .await?;
    super::policy_provider::get_policy_provider(
        State(state),
        Extension(auth),
        Extension(config),
        Extension(policy),
        Extension(store),
        Path(workspace_id),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn set_policy_provider(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
    Json(request): Json<treer_protocol::SetWorkspacePolicyProviderRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_POLICY_PROVIDER_MANAGE,
    )
    .await?;
    super::policy_provider::set_policy_provider_as(
        &state,
        &auth,
        &config,
        &policy,
        &store,
        &actor,
        workspace_id,
        request,
    )
    .await
}

pub(super) async fn clear_policy_provider(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(store): Extension<PolicyProviderStore>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let actor = workspace_admin(
        &policy,
        &agent,
        &workspace_id,
        ACTION_POLICY_PROVIDER_MANAGE,
    )
    .await?;
    super::policy_provider::clear_policy_provider_as(&auth, &policy, &store, &actor, &workspace_id)
        .await
}

pub(super) async fn machine_bootstrap(
    State(state): State<AppState>,
    Extension(config): Extension<BootstrapConfig>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    agent: AgentExt,
    Path(workspace_id): Path<String>,
) -> Result<Response, ApiFailure> {
    let actor = workspace_admin(&policy, &agent, &workspace_id, ACTION_MACHINE_ENROLL).await?;
    let response = super::machines::machine_bootstrap(
        &state,
        &config,
        &auth,
        workspace_id.clone(),
        &actor.label(),
    )
    .await?;
    if let Err(error) = auth
        .record_workspace_audit(crate::audit::NewWorkspaceAuditEvent {
            workspace_id: &workspace_id,
            actor_kind: actor.kind(),
            actor_id: Some(actor.id()),
            action: "machine.enrollment.created",
            resource_kind: "workspace",
            resource_id: &workspace_id,
            resource_name: None,
            payload: json!({}),
        })
        .await
    {
        tracing::warn!(?error, %workspace_id, "failed to record enrollment audit event");
    }
    // The enrollment key is a secret; never cache this response.
    Ok(([(header::CACHE_CONTROL, "no-store")], response).into_response())
}
