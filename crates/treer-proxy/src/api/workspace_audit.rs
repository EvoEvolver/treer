use super::*;
use crate::audit::summary::{self, RangeError};
use crate::policy::{ACTION_WORKSPACE_AUDIT_READ, RESOURCE_WORKSPACE};
use treer_protocol::{
    WorkspaceAuditEventsPage, WorkspaceAuditEventsQuery, WorkspaceAuditSummary,
    WorkspaceAuditSummaryQuery,
};

/// Summaries per caller per minute (#191); the Console refreshes on demand.
pub(crate) const SUMMARIES_PER_CALLER_PER_MINUTE: usize = 30;
/// Summaries computing at once per Proxy; excess callers get 503 and retry.
pub(crate) const SUMMARIES_IN_FLIGHT: usize = 4;
/// Wall budget for one summary across its statements.
const SUMMARY_WALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(6);

static SUMMARY_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(SUMMARIES_IN_FLIGHT);

pub(super) fn routes(auth: AuthStore) -> Router<AppState> {
    let human = Router::new()
        .route(
            "/api/workspaces/{workspace_id}/audit-events",
            get(human_audit),
        )
        .route(
            "/api/workspaces/{workspace_id}/audit-summary",
            get(human_summary),
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
        .route(
            "/agent/workspaces/{workspace_id}/audit-summary",
            get(agent_summary),
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
    let subject = machine_subject(&machine, agent, &workspace)?;
    read_audit(&auth, &policy, &workspace, subject, query).await
}

fn machine_subject(
    machine: &MachineSession,
    agent: Option<Extension<crate::auth::AgentSession>>,
    workspace: &str,
) -> Result<PolicySubject, ApiFailure> {
    if machine.workspace_id.as_deref() != Some(workspace) {
        return Err(ApiFailure::forbidden(
            "policy_denied",
            "workspace identity does not match",
        ));
    }
    // Both extensions come from require_machine, which verifies the workload
    // credential and its machine/workspace binding. Never derive identity from
    // an unverified caller-supplied actor ID.
    Ok(match agent {
        Some(Extension(agent)) => PolicySubject::Agent {
            server_id: agent.server_id,
            agent_id: agent.agent_id,
        },
        None => PolicySubject::Machine {
            server_id: machine.server_id.clone().ok_or_else(|| {
                ApiFailure::unauthorized("identity_required", "authenticated identity required")
            })?,
        },
    })
}

async fn human_summary(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace): Path<String>,
    Query(query): Query<WorkspaceAuditSummaryQuery>,
) -> Result<Json<WorkspaceAuditSummary>, ApiFailure> {
    read_summary(
        &state,
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

async fn agent_summary(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    agent: Option<Extension<crate::auth::AgentSession>>,
    Path(workspace): Path<String>,
    Query(query): Query<WorkspaceAuditSummaryQuery>,
) -> Result<Json<WorkspaceAuditSummary>, ApiFailure> {
    let subject = machine_subject(&machine, agent, &workspace)?;
    read_summary(&state, &auth, &policy, &workspace, subject, query).await
}

fn valid_filter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Filters shared by the list and summary routes (#191). `outcome` and
/// `actor_kind` are closed sets; the others are identifier-shaped.
fn validate_filters(
    actor_kind: Option<&str>,
    actor_id: Option<&str>,
    action: Option<&str>,
    resource_kind: Option<&str>,
    resource_id: Option<&str>,
    outcome: Option<&str>,
) -> Result<(), ApiFailure> {
    let invalid =
        || ApiFailure::bad_request("invalid_audit_query", "invalid audit filters or cursor");
    if actor_kind.is_some_and(|v| !matches!(v, "user" | "agent" | "machine" | "service" | "system"))
        || outcome.is_some_and(|v| !matches!(v, "succeeded" | "failed"))
    {
        return Err(invalid());
    }
    for value in [actor_id, action, resource_kind, resource_id]
        .into_iter()
        .flatten()
    {
        if !valid_filter(value) {
            return Err(invalid());
        }
    }
    Ok(())
}

async fn authorize_audit_read(
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace: &str,
    subject: PolicySubject,
) -> Result<(), ApiFailure> {
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
    Ok(())
}

async fn read_summary(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace: &str,
    subject: PolicySubject,
    query: WorkspaceAuditSummaryQuery,
) -> Result<Json<WorkspaceAuditSummary>, ApiFailure> {
    let caller = match &subject {
        PolicySubject::Human { user_id } => format!("audit-summary:user:{user_id}"),
        PolicySubject::Agent { agent_id, .. } => format!("audit-summary:agent:{agent_id}"),
        PolicySubject::Machine { server_id } => format!("audit-summary:machine:{server_id}"),
        other => format!("audit-summary:other:{other:?}"),
    };
    authorize_audit_read(auth, policy, workspace, subject).await?;
    validate_filters(
        query.actor_kind.as_deref(),
        query.actor_id.as_deref(),
        query.action.as_deref(),
        query.resource_kind.as_deref(),
        query.resource_id.as_deref(),
        query.outcome.as_deref(),
    )?;
    let range = summary::resolve_range(&query, chrono::Utc::now())
        .map_err(|RangeError::Invalid(why)| ApiFailure::bad_request("invalid_audit_query", why))?;
    // The caller is authorized before any budget is spent on it, and the
    // caps are checked before the zone lookup touches the database.
    if !state
        .upload_gate()
        .allow(&caller, SUMMARIES_PER_CALLER_PER_MINUTE)
    {
        return Err(ApiFailure {
            status: StatusCode::TOO_MANY_REQUESTS,
            error: ProtocolError::new(
                "audit_summary_rate_limited",
                "too many audit summaries for this caller; retry in a minute",
            ),
        });
    }
    let Ok(_slot) = SUMMARY_SLOTS.try_acquire() else {
        return Err(ApiFailure::service_unavailable(
            "audit_summary_busy",
            "too many audit summaries are being computed; retry shortly",
        ));
    };
    let unavailable = || {
        ApiFailure::service_unavailable(
            "audit_summary_unavailable",
            "workspace audit summary is unavailable",
        )
    };
    let pool = auth.pool();
    let computed = tokio::time::timeout(SUMMARY_WALL_BUDGET, async {
        if !summary::zone_exists(&pool, &range.tz).await? {
            return Ok(None);
        }
        summary::summarize(&pool, workspace, &query, &range)
            .await
            .map(Some)
    })
    .await;
    match computed {
        Ok(Ok(Some(summary))) => Ok(Json(summary)),
        Ok(Ok(None)) => Err(ApiFailure::bad_request(
            "invalid_audit_query",
            "unknown time zone",
        )),
        Ok(Err(error)) => {
            tracing::warn!(code = "audit_summary_failed", error = %error, "audit summary query failed");
            Err(unavailable())
        }
        Err(_elapsed) => Err(unavailable()),
    }
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
    if query.sequence.is_some_and(|v| v <= 0) {
        return Err(invalid());
    }
    validate_filters(
        query.actor_kind.as_deref(),
        query.actor_id.as_deref(),
        query.action.as_deref(),
        query.resource_kind.as_deref(),
        query.resource_id.as_deref(),
        query.outcome.as_deref(),
    )
}

async fn read_audit(
    auth: &AuthStore,
    policy: &PolicyEngine,
    workspace: &str,
    subject: PolicySubject,
    query: WorkspaceAuditEventsQuery,
) -> Result<Json<WorkspaceAuditEventsPage>, ApiFailure> {
    authorize_audit_read(auth, policy, workspace, subject).await?;
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

#[cfg(test)]
mod summary_tests;
