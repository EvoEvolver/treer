use super::*;
use sha2::{Digest, Sha256};
use treer_protocol::{SendAppEmailRequest, SendAppEmailResponse};

/// The App runtime behind a request: `(workspace, server, runtime agent)`.
struct EmailCaller {
    workspace_id: String,
    server_id: String,
    agent_id: String,
}

/// App routes authenticate the App runtime's service-audience token.
async fn app_caller(
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service_id: &str,
) -> Result<EmailCaller, ApiFailure> {
    let claims = authenticate_workspace_app(auth, identity, headers, service_id).await?;
    if claims.principal_kind != AppPrincipalKind::Agent {
        return Err(email_app_required());
    }
    Ok(EmailCaller {
        workspace_id: claims.workspace_id,
        server_id: claims.machine_id.ok_or_else(email_app_required)?,
        agent_id: claims.sub,
    })
}

/// Machine routes: the Controller relays the calling workload (`treer email`).
async fn machine_caller(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: String,
) -> Result<EmailCaller, ApiFailure> {
    let PolicySubject::Agent {
        server_id,
        agent_id,
    } = agent_policy_subject(state, machine, headers, &workspace_id).await?
    else {
        return Err(email_app_required());
    };
    Ok(EmailCaller {
        workspace_id,
        server_id,
        agent_id,
    })
}

/// Only a Managed App runtime on its own machine (and, on App routes, for the
/// addressed service) may send or look up email.
async fn owned_app(
    auth: &AuthStore,
    caller: &EmailCaller,
    service_id: Option<&str>,
) -> Result<AppDeployment, ApiFailure> {
    auth.resolve_app_deployment_by_runtime(&caller.workspace_id, &caller.agent_id)
        .await?
        .filter(|app| {
            app.server_id == caller.server_id && service_id.is_none_or(|id| id == app.service_id)
        })
        .ok_or_else(email_app_required)
}

/// `POST /api/apps/{service_id}/email`.
pub(super) async fn send_app_email(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendAppEmailRequest>,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let caller = app_caller(&auth, &identity, &headers, &service_id).await?;
    send_email_as_app(&auth, &policy, caller, Some(&service_id), &request).await
}

/// `POST /agent/workspaces/{workspace_id}/email`.
pub(super) async fn send_core_app_email(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendAppEmailRequest>,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let caller = machine_caller(&state, &machine, &headers, workspace_id).await?;
    send_email_as_app(&auth, &policy, caller, None, &request).await
}

/// `GET /api/apps/{service_id}/email/{idempotency_key}`.
pub(super) async fn app_email_status(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path((service_id, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let caller = app_caller(&auth, &identity, &headers, &service_id).await?;
    let app = owned_app(&auth, &caller, Some(&service_id)).await?;
    auth.app_email_status(&caller.workspace_id, &app.app_id, &key)
        .await
        .map(Json)
}

/// `GET /agent/workspaces/{workspace_id}/email/{idempotency_key}`.
pub(super) async fn core_app_email_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(machine): Extension<MachineSession>,
    Path((workspace_id, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let caller = machine_caller(&state, &machine, &headers, workspace_id).await?;
    let app = owned_app(&auth, &caller, None).await?;
    auth.app_email_status(&caller.workspace_id, &app.app_id, &key)
        .await
        .map(Json)
}

/// Ownership, `email.send` Policy and body-free audit for both send routes,
/// before the store reserves and dispatches.
async fn send_email_as_app(
    auth: &AuthStore,
    policy: &PolicyEngine,
    caller: EmailCaller,
    service_id: Option<&str>,
    request: &SendAppEmailRequest,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let app = owned_app(auth, &caller, service_id).await?;
    let workspace_id = caller.workspace_id.as_str();
    let recipient_hash = format!(
        "{:x}",
        Sha256::digest(request.to.to_ascii_lowercase().as_bytes())
    );
    let result = async {
        authorize_control(
            policy,
            workspace_id,
            Some(&PolicySubject::Agent {
                server_id: app.server_id.clone(),
                agent_id: caller.agent_id.clone(),
            }),
            crate::policy::ACTION_EMAIL_SEND,
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &app.service_id),
        )
        .await?;
        auth.send_app_email(workspace_id, &app.app_id, request)
            .await
            .map(Json)
    }
    .await;
    let outcome = match &result {
        Ok(_) => "sent",
        Err(error) => error.error.code.as_str(),
    };
    tracing::info!(workspace_id = %workspace_id, app_id = %app.app_id,
        recipient_hash = %recipient_hash, outcome, "App email request outcome");
    result
}

fn email_app_required() -> ApiFailure {
    ApiFailure::forbidden(
        "email_app_required",
        "only a Managed App runtime may use App email, for its own service",
    )
}

#[cfg(test)]
pub(super) async fn relayed_email_code(
    auth: &AuthStore,
    server_id: &str,
    agent_id: &str,
    request: &SendAppEmailRequest,
) -> String {
    let caller = EmailCaller {
        workspace_id: "default".into(),
        server_id: server_id.into(),
        agent_id: agent_id.into(),
    };
    send_email_as_app(auth, &PolicyEngine::allow_all(), caller, None, request)
        .await
        .unwrap_err()
        .error
        .code
}
