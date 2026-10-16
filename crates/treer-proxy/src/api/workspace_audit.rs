use super::*;
use crate::policy::{ACTION_WORKSPACE_AUDIT_READ, RESOURCE_WORKSPACE};
use treer_protocol::{WorkspaceAuditEventsPage, WorkspaceAuditEventsQuery};

pub(super) fn routes(auth: AuthStore) -> Router<AppState> {
    let human = Router::new()
        .route(
            "/api/workspaces/{workspace_id}/audit-events",
            get(human_audit),
        )
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_user,
        ));
    let agent = Router::new()
        .route(
            "/agent/workspaces/{workspace_id}/audit-events",
            get(agent_audit),
        )
        .route_layer(middleware::from_fn_with_state(auth, auth::require_machine));
    human
        .merge(agent)
        .route("/api/apps/{service_id}/audit-events", get(app_audit))
        // Outside authentication and Query extraction, including all errors.
        .layer(middleware::map_response(no_store))
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn human_audit(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Query(query): Query<WorkspaceAuditEventsQuery>,
) -> Result<Json<WorkspaceAuditEventsPage>, ApiFailure> {
    read_audit(
        &auth,
        &policy,
        &workspace,
        PolicySubject::Human {
            user_id: session.user_id,
        },
        query,
    )
    .await
}

async fn agent_audit(
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    agent: Option<Extension<crate::auth::AgentSession>>,
    Path(workspace): Path<String>,
    Query(query): Query<WorkspaceAuditEventsQuery>,
) -> Result<Json<WorkspaceAuditEventsPage>, ApiFailure> {
    if machine.workspace_id.as_deref() != Some(workspace.as_str()) {
        return Err(ApiFailure::forbidden(
            "policy_denied",
            "workspace identity does not match",
        ));
    }
    // Both extensions come from require_machine, which verifies the workload
    // credential and its machine/workspace binding. Never derive identity from
    // an unverified caller-supplied actor ID.
    let subject = match agent {
        Some(Extension(agent)) => PolicySubject::Agent {
            server_id: agent.server_id,
            agent_id: agent.agent_id,
        },
        None => PolicySubject::Machine {
            server_id: machine.server_id.ok_or_else(|| {
                ApiFailure::unauthorized("identity_required", "authenticated identity required")
            })?,
        },
    };
    read_audit(&auth, &policy, &workspace, subject, query).await
}

async fn app_audit(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service): Path<String>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceAuditEventsQuery>,
) -> Result<Json<WorkspaceAuditEventsPage>, ApiFailure> {
    // A service-audience token represents its caller, never the App's own
    // authority. The verified audience fixes the workspace; no query override.
    let (workspace, subject, _) =
        app_message_identity(&state, &auth, &identity, &headers, &service).await?;
    read_audit(&auth, &policy, &workspace, subject, query).await
}

fn validate_query(query: &WorkspaceAuditEventsQuery) -> Result<(), ApiFailure> {
    let invalid =
        || ApiFailure::bad_request("invalid_audit_query", "invalid audit filters or cursor");
    if query.before.is_some_and(|v| v <= 0) {
        return Err(invalid());
    }
    if let (Some(since), Some(until)) = (query.since, query.until) {
        if since >= until || until.signed_duration_since(since) > chrono::Duration::days(366) {
            return Err(invalid());
        }
    }
    for value in [&query.actor_id, &query.action, &query.resource_kind]
        .into_iter()
        .flatten()
    {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

async fn read_audit(
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace: &str,
    subject: PolicySubject,
    query: WorkspaceAuditEventsQuery,
) -> Result<Json<WorkspaceAuditEventsPage>, ApiFailure> {
    // Membership is checked from durable data on every read. Being owner of
    // another workspace (even in the same org) is not a workspace grant.
    let role_allowed = match &subject {
        PolicySubject::Human { user_id } => {
            auth.workspace_member_role(workspace, user_id).await? == "owner"
        }
        _ => false,
    };
    policy
        .authorize_sensitive_read(
            &PolicyRequest::new(
                workspace,
                subject,
                ACTION_WORKSPACE_AUDIT_READ,
                PolicyResource::new(RESOURCE_WORKSPACE, workspace),
            ),
            role_allowed,
        )
        .await?;
    validate_query(&query)?;
    let page = crate::audit::list_workspace(&auth.pool(), workspace, &query)
        .await
        .map_err(|_| ApiFailure::internal("audit_unavailable", "workspace audit is unavailable"))?;
    // Reading does not append an event: paginating must not grow the very feed
    // being read. This limitation is documented rather than claiming coverage.
    Ok(Json(page))
}

#[cfg(test)]
mod tests;
