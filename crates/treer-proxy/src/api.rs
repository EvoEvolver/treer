use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, Version};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use treer_protocol::{
    AgentCommand, AgentInboxRequest, AgentInboxResponse, AgentInfo, ApiError, CreateAgentRequest,
    CreateMachineServiceRequest, CreateVirtualNetworkHostRequest, InputAgentRequest,
    MachineEnrollmentRequest, MachineEnrollmentResponse, MachineService, MailAddress,
    MailAddressKind, MailboxResponse, PromptAgentRequest, ProtocolError, RenameRequest,
    SendAgentMailRequest, SendAgentMailResponse, TerminalClientMessage, TerminalServerMessage,
    UpdateMachineServiceRequest, VirtualNetworkHostsSnapshot, WorkloadIdentityTokenRequest,
    WorkloadIdentityVerifyRequest, WorkspaceEvent, WorkspaceHuman, AGENT_ID_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::agent_socket;
use crate::auth::{self, AuthStore, CurrentSession, MachineSession};
use crate::identity::IdentityIssuer;
use crate::policy::{
    PolicyEngine, PolicyRequest, PolicyResource, PolicySubject, ACTION_HUMAN_LIST,
    ACTION_IDENTITY_TOKEN_ISSUE, ACTION_MAIL_READ, ACTION_MAIL_SEND, ACTION_SERVICE_CREATE,
    ACTION_SERVICE_DELETE, ACTION_SERVICE_LIST, ACTION_SERVICE_PROBE, ACTION_SERVICE_UPDATE,
    ACTION_VIRTUAL_HOST_CREATE, ACTION_VIRTUAL_HOST_DELETE, ACTION_VIRTUAL_HOST_LIST,
    RESOURCE_AGENT_MAILBOX, RESOURCE_HUMAN_DIRECTORY, RESOURCE_HUMAN_MAILBOX,
    RESOURCE_MACHINE_SERVICE, RESOURCE_VIRTUAL_HOST,
};
use crate::state::{AppState, SocketFrame};

#[derive(Clone)]
pub struct BootstrapConfig {
    public_url: Url,
    artifacts_dir: PathBuf,
    release_artifact_base_url: Url,
}

#[derive(Clone)]
pub struct BrowserAccess {
    origin: HeaderValue,
    origin_text: Arc<str>,
}

#[derive(Clone)]
struct WorkloadIdentityApi {
    auth: AuthStore,
    policy: PolicyEngine,
    issuer: IdentityIssuer,
}

impl BootstrapConfig {
    pub fn new(
        public_url: Url,
        artifacts_dir: PathBuf,
        mut release_artifact_base_url: Url,
    ) -> Self {
        let mut path = release_artifact_base_url
            .path()
            .trim_end_matches('/')
            .to_string();
        path.push('/');
        release_artifact_base_url.set_path(&path);
        release_artifact_base_url.set_query(None);
        release_artifact_base_url.set_fragment(None);
        Self {
            public_url,
            artifacts_dir,
            release_artifact_base_url,
        }
    }
}

impl BrowserAccess {
    pub fn new(app_public_url: &Url) -> anyhow::Result<Self> {
        let origin_text: Arc<str> = app_public_url.origin().ascii_serialization().into();
        let origin = HeaderValue::from_str(&origin_text)
            .context("app public URL produced an invalid HTTP Origin")?;
        Ok(Self {
            origin,
            origin_text,
        })
    }

    fn cors_layer(&self) -> CorsLayer {
        CorsLayer::new()
            .allow_origin(self.origin.clone())
            .allow_credentials(true)
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
    }

    fn validate_if_present(&self, headers: &HeaderMap) -> Result<(), ApiFailure> {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return Ok(());
        };
        if origin == self.origin {
            Ok(())
        } else {
            Err(ApiFailure::forbidden(
                "browser_origin_denied",
                &format!("browser requests must originate from {}", self.origin_text),
            ))
        }
    }
}

pub fn router(
    state: AppState,
    bootstrap: BootstrapConfig,
    auth_store: AuthStore,
    policy: PolicyEngine,
    identity: IdentityIssuer,
    browser: BrowserAccess,
) -> Router {
    let cors = browser.cors_layer();
    let workload_identity = WorkloadIdentityApi {
        auth: auth_store.clone(),
        policy: policy.clone(),
        issuer: identity.clone(),
    };
    let agent_control = Router::new()
        .route("/agent/machine/identity", post(bind_machine_identity))
        .route(
            "/agent/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route(
            "/agent/workspaces/{workspace_id}/identity/token",
            post(agent_issue_identity_token),
        )
        .route(
            "/agent/workspaces/{workspace_id}/mail",
            post(agent_send_mail),
        )
        .route(
            "/agent/workspaces/{workspace_id}/inbox",
            post(agent_read_inbox),
        )
        .route(
            "/agent/workspaces/{workspace_id}/humans",
            get(agent_list_humans),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services",
            get(agent_list_machine_services).post(agent_create_machine_service),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(agent_update_machine_service).delete(agent_delete_machine_service),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}/probe",
            post(agent_probe_machine_service),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts",
            get(agent_list_virtual_network_hosts).post(agent_create_virtual_network_host),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(agent_delete_virtual_network_host),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        )
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_machine,
        ));
    let authenticated = Router::new()
        .route(
            "/api/organizations",
            get(auth::organizations).post(auth::create_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}",
            axum::routing::patch(auth::rename_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}/members",
            get(auth::members),
        )
        .route(
            "/api/organizations/{organization_id}/members/{user_id}",
            axum::routing::patch(auth::update_member_role_handler)
                .delete(auth::remove_member_handler),
        )
        .route(
            "/api/organizations/{organization_id}/invitations",
            post(auth::create_invitation),
        )
        .route(
            "/api/workspaces/{workspace_id}/bootstrap",
            post(bootstrap_info),
        )
        .route(
            "/api/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/api/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route(
            "/api/workspaces/{workspace_id}/inbox",
            post(human_read_inbox),
        )
        .route("/api/workspaces/{workspace_id}/servers", get(list_servers))
        .route(
            "/api/workspaces/{workspace_id}/services",
            get(list_machine_services).post(create_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(update_machine_service).delete(delete_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}/probe",
            post(probe_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts",
            get(list_virtual_network_hosts).post(create_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(delete_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/{*path}",
            any(proxy_virtual_network_host_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        )
        .route(
            "/api/workspaces/{workspace_id}/events",
            get(workspace_events),
        )
        .route("/api/auth/me", get(auth::me))
        .route(
            "/api/auth/profile",
            axum::routing::patch(auth::update_profile),
        )
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_workspace_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_user,
        ));
    let admin = Router::new()
        .route("/api/admin/me", get(auth::admin_me))
        .route("/api/admin/logout", post(auth::admin_logout))
        .route("/api/admin/dashboard", get(auth::admin_dashboard))
        .route(
            "/api/admin/invitations",
            post(auth::admin_create_invitation),
        )
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_admin,
        ));
    Router::new()
        .route("/install.sh", get(install_script))
        .route("/api/machines/enroll", post(enroll_machine))
        .route("/artifacts/{platform}/{binary}", get(download_artifact))
        .route("/api/health", get(health))
        .route("/.well-known/jwks.json", get(workload_identity_jwks))
        .route("/.treer/identity/verify", post(verify_workload_identity))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/config", get(auth::oauth_config))
        .route("/api/auth/oauth/{provider}/start", get(auth::oauth_start))
        .route(
            "/api/auth/oauth/{provider}/callback",
            get(auth::oauth_callback),
        )
        .route(
            "/api/auth/request-password-reset",
            post(auth::request_password_reset),
        )
        .route("/api/auth/reset-password", post(auth::reset_password))
        .route("/api/auth/register", post(auth::register))
        .route("/api/admin/login", post(auth::admin_login))
        .route("/agent/connect", get(agent_socket::upgrade))
        .merge(agent_control)
        .merge(authenticated)
        .merge(admin)
        .layer(Extension(bootstrap))
        .layer(Extension(policy))
        .layer(Extension(identity))
        .layer(Extension(workload_identity))
        .layer(Extension(auth_store))
        .layer(Extension(browser))
        .with_state(state)
        .layer(cors)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "treer-proxy" }))
}

async fn workload_identity_jwks(Extension(identity): Extension<IdentityIssuer>) -> Response {
    (
        [(header::CACHE_CONTROL, "public, max-age=300")],
        Json(identity.jwks()),
    )
        .into_response()
}

async fn verify_workload_identity(
    Extension(identity): Extension<IdentityIssuer>,
    Json(request): Json<WorkloadIdentityVerifyRequest>,
) -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(identity.verify(&request.token, request.audience.trim())),
    )
        .into_response()
}

async fn agent_issue_identity_token(
    State(state): State<AppState>,
    Extension(identity_api): Extension<WorkloadIdentityApi>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<WorkloadIdentityTokenRequest>,
) -> Result<Response, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = identity_api
        .auth
        .resolve_machine_service(&workspace_id, request.audience.trim())
        .await?;
    identity_api
        .policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_IDENTITY_TOKEN_ISSUE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let PolicySubject::Agent {
        server_id,
        agent_id,
    } = subject
    else {
        return Err(ApiFailure::internal(
            "identity_subject_error",
            "identity token subject was not an agent",
        ));
    };
    let token = identity_api
        .issuer
        .issue(&workspace_id, &server_id, &agent_id, &service.service_id)
        .map_err(|error| {
            tracing::error!(%error, "failed to sign workload identity token");
            ApiFailure::internal("identity_signing_failed", "failed to sign identity token")
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(token)).into_response())
}

async fn agent_send_mail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<SendAgentMailRequest>,
) -> Result<Json<SendAgentMailResponse>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let PolicySubject::Agent { agent_id, .. } = &subject else {
        return Err(ApiFailure::internal(
            "mail_subject_error",
            "mail sender was not an agent",
        ));
    };
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "invalid_mail_recipients",
            "mail must have 1-32 recipients",
        ));
    }
    let snapshot = state.snapshot(&workspace_id).await?;
    let humans = auth.list_workspace_humans(&workspace_id).await?;
    let sender = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == *agent_id)
        .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?;
    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    for raw_target in &request.recipients {
        let target = raw_target.trim();
        let target = if matches!(target, "self" | ".") {
            agent_id.as_str()
        } else {
            target
        };
        let recipient = resolve_mail_recipient(&snapshot.agents, &humans, target)?;
        if !seen.insert((recipient.kind, recipient.id.clone())) {
            continue;
        }
        let resource_type = match recipient.kind {
            MailAddressKind::Agent => RESOURCE_AGENT_MAILBOX,
            MailAddressKind::Human => RESOURCE_HUMAN_MAILBOX,
        };
        policy
            .authorize(&PolicyRequest::new(
                &workspace_id,
                subject.clone(),
                ACTION_MAIL_SEND,
                PolicyResource::new(resource_type, &recipient.id),
            ))
            .await?;
        recipients.push(recipient);
    }
    let message = auth
        .send_agent_mail(
            &workspace_id,
            MailAddress {
                kind: MailAddressKind::Agent,
                id: sender.agent_id.clone(),
                name: sender.name.clone(),
            },
            recipients,
            request.context_ids,
            &request.body,
        )
        .await?;
    Ok(Json(SendAgentMailResponse { message }))
}

fn resolve_mail_recipient(
    agents: &[AgentInfo],
    humans: &[WorkspaceHuman],
    target: &str,
) -> Result<MailAddress, ProtocolError> {
    let mut matches = agents
        .iter()
        .filter(|agent| agent.agent_id == target)
        .map(|agent| MailAddress {
            kind: MailAddressKind::Agent,
            id: agent.agent_id.clone(),
            name: agent.name.clone(),
        })
        .chain(
            humans
                .iter()
                .filter(|human| human.user_id == target)
                .map(|human| MailAddress {
                    kind: MailAddressKind::Human,
                    id: human.user_id.clone(),
                    name: human.preferred_name.clone(),
                }),
        )
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches.extend(
            agents
                .iter()
                .filter(|agent| agent.name == target)
                .map(|agent| MailAddress {
                    kind: MailAddressKind::Agent,
                    id: agent.agent_id.clone(),
                    name: agent.name.clone(),
                }),
        );
        matches.extend(
            humans
                .iter()
                .filter(|human| human.preferred_name == target)
                .map(|human| MailAddress {
                    kind: MailAddressKind::Human,
                    id: human.user_id.clone(),
                    name: human.preferred_name.clone(),
                }),
        );
    }
    match matches.as_slice() {
        [] => Err(ProtocolError::new(
            "recipient_not_found",
            format!("no Agent or human recipient matches {target}"),
        )),
        [recipient] => Ok(recipient.clone()),
        _ => Err(ProtocolError::new(
            "recipient_ambiguous",
            format!("more than one Agent or human is named {target}; use a stable id"),
        )),
    }
}

async fn agent_list_humans(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_HUMAN_LIST,
            PolicyResource::new(RESOURCE_HUMAN_DIRECTORY, &workspace_id),
        ))
        .await?;
    Ok(Json(json!({
        "humans": auth.list_workspace_humans(&workspace_id).await?
    })))
}

async fn agent_read_inbox(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(request): Json<AgentInboxRequest>,
) -> Result<Json<AgentInboxResponse>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let PolicySubject::Agent { agent_id, .. } = &subject else {
        return Err(ApiFailure::internal(
            "mail_subject_error",
            "mailbox reader was not an agent",
        ));
    };
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_MAIL_READ,
            PolicyResource::new(RESOURCE_AGENT_MAILBOX, agent_id),
        ))
        .await?;
    Ok(Json(
        auth.read_agent_inbox(&workspace_id, agent_id, request.limit)
            .await?,
    ))
}

async fn human_read_inbox(
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<AgentInboxRequest>,
) -> Result<Json<MailboxResponse>, ApiFailure> {
    Ok(Json(
        auth.read_human_mailbox(&workspace_id, &session.user_id, request.limit)
            .await?,
    ))
}

async fn bootstrap_info(
    State(state): State<AppState>,
    Extension(config): Extension<BootstrapConfig>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    state.snapshot(&workspace_id).await?;
    let enrollment = auth
        .create_machine_enrollment(&workspace_id, &session.user_id)
        .await?;
    let (install_command, connect_command) = bootstrap_commands(&config.public_url, &enrollment);
    let script_url = install_script_url(&config.public_url);
    Ok(Json(json!({
        "install_command": install_command,
        "connect_command": connect_command,
        "script_url": script_url.as_str(),
        "workspace_id": workspace_id,
    })))
}

fn bootstrap_commands(public_url: &Url, enrollment_key: &str) -> (String, String) {
    let script_url = install_script_url(public_url);
    let install_command = format!("curl -fsSL {} | sh", shell_quote(script_url.as_str()));
    let connect_command = format!(
        "TREER_ENROLLMENT_KEY={} treer-agent-server connect --proxy {}",
        shell_quote(enrollment_key),
        shell_quote(public_url.as_str()),
    );
    (install_command, connect_command)
}

async fn install_script(Extension(config): Extension<BootstrapConfig>) -> Response {
    let script = render_install_script(&config.public_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        script,
    )
        .into_response()
}

async fn enroll_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
    request: Option<Json<MachineEnrollmentRequest>>,
) -> Result<Response, ApiFailure> {
    let request = request.as_ref().map(|request| &request.0);
    let enrollment = auth
        .claim_machine_enrollment_from_headers(
            &headers,
            request.map(|request| request.installation_id.as_str()),
            request.map(|request| request.name.as_str()),
        )
        .await?;
    state
        .allow_server_reenrollment(&enrollment.workspace_id, &enrollment.server_id)
        .await;
    let response = MachineEnrollmentResponse {
        workspace_id: enrollment.workspace_id,
        server_id: enrollment.server_id,
        machine_token: enrollment.machine_token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response())
}

async fn bind_machine_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(machine): Extension<MachineSession>,
    Json(request): Json<MachineEnrollmentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = machine.workspace_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine workspace identity is required",
        )
    })?;
    let server_id = machine.server_id.as_deref().ok_or_else(|| {
        ProtocolError::new(
            "machine_identity_required",
            "machine server identity is required",
        )
    })?;
    auth.bind_machine_identity(
        workspace_id,
        server_id,
        &request.installation_id,
        &request.name,
    )
    .await?;
    if state.resolve_server(workspace_id, server_id).await.is_ok() {
        let name = normalize_display_name(request.name)?;
        state.rename_server(workspace_id, server_id, name).await?;
    }
    Ok(Json(json!({ "bound": true, "server_id": server_id })))
}

async fn download_artifact(
    Extension(config): Extension<BootstrapConfig>,
    Path((platform, binary)): Path<(String, String)>,
) -> Result<Response, ApiFailure> {
    if !valid_artifact_component(&platform)
        || !matches!(
            binary.as_str(),
            "treer" | "treer-agent-host" | "treer-agent-server"
        )
    {
        return Err(ApiFailure::not_found(
            "artifact_not_found",
            "artifact not found",
        ));
    }
    let path = config.artifacts_dir.join(&platform).join(&binary);
    match tokio::fs::read(&path).await {
        Ok(data) => Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            data,
        )
            .into_response()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let release_url = release_artifact_url(&config, &platform, &binary)?;
            tracing::info!(
                path = %path.display(),
                release = %release_url,
                "redirecting missing bootstrap artifact to release"
            );
            Ok(Redirect::temporary(release_url.as_str()).into_response())
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "bootstrap artifact unavailable");
            Err(ApiFailure::not_found(
                "artifact_not_found",
                "artifact not found",
            ))
        }
    }
}

fn release_artifact_url(
    config: &BootstrapConfig,
    platform: &str,
    binary: &str,
) -> Result<Url, ApiFailure> {
    config
        .release_artifact_base_url
        .join(&format!("{binary}-{platform}"))
        .map_err(|error| {
            ProtocolError::new(
                "artifact_url_error",
                format!("failed to build release artifact URL: {error}"),
            )
            .into()
        })
}

fn install_script_url(public_url: &Url) -> Url {
    let mut url = public_url.clone();
    url.set_path("/install.sh");
    url.set_query(None);
    url
}

fn valid_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_install_script(public_url: &Url) -> String {
    let mut artifact_base = public_url.clone();
    artifact_base.set_path("/artifacts/");
    format!(
        r#"#!/bin/sh
set -eu

artifact_base={artifact_base}
install_dir=${{TREER_INSTALL_DIR:-"${{HOME:?HOME is required}}/.local/bin"}}
server_dir=${{TREER_AGENT_SERVER_INSTALL_DIR:-"${{HOME}}/.local/libexec/treer"}}

echo "treer: security notice" >&2
echo "treer: the Agent Server is a persistent proxy and agent host that runs with your user account's system permissions" >&2
echo "treer: workspace agents can execute commands and make network requests on this machine" >&2
echo "treer: use a dedicated account, VM, container, or other sandbox when possible" >&2

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "treer: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

case "$platform" in
  linux-*)
    if ! command -v unshare >/dev/null 2>&1; then
      echo "treer: warning: transparent agent networking requires unshare(1) from util-linux" >&2
    fi
    ;;
esac

if command -v curl >/dev/null 2>&1; then
  fetch() {{ curl -fsSL "$1" -o "$2"; }}
elif command -v wget >/dev/null 2>&1; then
  fetch() {{ wget -q "$1" -O "$2"; }}
else
  echo "treer: curl or wget is required" >&2
  exit 1
fi

mkdir -p "$install_dir" "$server_dir"
tmp_dir=$(mktemp -d "${{TMPDIR:-/tmp}}/treer-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

echo "treer: downloading $platform binaries"
fetch "$artifact_base/$platform/treer" "$tmp_dir/treer"
fetch "$artifact_base/$platform/treer-agent-host" "$tmp_dir/treer-agent-host"
fetch "$artifact_base/$platform/treer-agent-server" "$tmp_dir/treer-agent-server"
chmod 755 "$tmp_dir/treer" "$tmp_dir/treer-agent-host" "$tmp_dir/treer-agent-server"
mv "$tmp_dir/treer" "$install_dir/treer"
mv "$tmp_dir/treer-agent-host" "$server_dir/treer-agent-host"
mv "$tmp_dir/treer-agent-server" "$server_dir/treer-agent-server"
ln -sf "$server_dir/treer-agent-server" "$install_dir/treer-agent-server"

echo "treer: binaries installed"
echo "treer: add $install_dir to PATH to use treer and treer-agent-server"
echo "treer: run the workspace connection command from the Proxy UI next"
"#,
        artifact_base = shell_quote(artifact_base.as_str().trim_end_matches('/')),
    )
}

#[derive(Deserialize)]
struct ListWorkspacesQuery {
    organization_id: String,
}

#[derive(Deserialize)]
struct CreateWorkspaceApiRequest {
    organization_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    name: String,
}

async fn list_workspaces(
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

async fn create_workspace(
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

async fn workspace_snapshot(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(serde_json::to_value(
        state.snapshot(&workspace_id).await?,
    )?))
}

async fn list_servers(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    Ok(Json(json!({ "servers": snapshot.servers })))
}

async fn list_machine_services(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "services": auth.list_machine_services(&workspace_id).await?
    })))
}

async fn create_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    request.server_id = state
        .resolve_server(&workspace_id, &request.server_id)
        .await?
        .server_id;
    let service = auth
        .create_machine_service(&workspace_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({ "service": service })))
}

async fn update_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(mut request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if let Some(server_id) = request.server_id.as_deref() {
        request.server_id = Some(
            state
                .resolve_server(&workspace_id, server_id)
                .await?
                .server_id,
        );
    }
    let service = auth
        .update_machine_service(&workspace_id, &service_id, &session.user_id, request)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "service": service })))
}

async fn delete_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let service = auth
        .delete_machine_service(&workspace_id, &service_id)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({
        "deleted": true,
        "service_id": service.service_id,
        "name": service.name,
    })))
}

async fn probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

async fn list_virtual_network_hosts(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "hosts": auth.list_virtual_network_hosts(&workspace_id).await?
    })))
}

async fn create_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let host = auth
        .create_virtual_network_host(&workspace_id, &session.user_id, request)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "host": host })))
}

async fn delete_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.delete_virtual_network_host(&workspace_id, &hostname)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn proxy_virtual_network_host_root(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, String::new(), request).await
}

async fn proxy_virtual_network_host_path(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, hostname, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(request.headers())?;
    proxy_virtual_network_host(state, auth, workspace_id, hostname, path, request).await
}

async fn proxy_virtual_network_host(
    state: AppState,
    auth: AuthStore,
    workspace_id: String,
    hostname: String,
    path: String,
    mut request: Request<Body>,
) -> Result<Response, ApiFailure> {
    let host = auth
        .resolve_virtual_network_host(&workspace_id, &hostname)
        .await?
        .ok_or_else(|| ApiFailure::not_found("virtual_host_not_found", &hostname))?;
    if host.service_protocol != treer_protocol::MachineServiceProtocol::Http {
        return Err(ApiFailure::bad_request(
            "service_protocol_mismatch",
            "browser access requires an HTTP service",
        ));
    }
    let stream = state
        .open_browser_network_stream(
            &workspace_id,
            &host.destination_server_id,
            &host.target_host,
            host.target_port.unwrap_or(80),
        )
        .await?;

    let query = request.uri().query();
    let target = if path.is_empty() {
        query.map_or_else(|| "/".to_string(), |query| format!("/?{query}"))
    } else {
        query.map_or_else(|| format!("/{path}"), |query| format!("/{path}?{query}"))
    };
    *request.uri_mut() = target
        .parse::<Uri>()
        .map_err(|error| ApiFailure::bad_gateway("invalid_tunnel_uri", &error.to_string()))?;
    *request.version_mut() = Version::HTTP_11;

    let upgraded = request.headers().contains_key(header::UPGRADE);
    let downstream_upgrade = upgraded.then(|| hyper::upgrade::on(&mut request));
    sanitize_tunnel_request_headers(request.headers_mut(), upgraded, &host.hostname)?;

    let io = TokioIo::new(stream);
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<_, Body>(io)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_handshake_failed", &error.to_string()))?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            tracing::debug!(%error, "virtual host tunnel connection closed");
        }
    });
    let mut response = sender
        .send_request(request)
        .await
        .map_err(|error| ApiFailure::bad_gateway("tunnel_request_failed", &error.to_string()))?;

    let target_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
        .then(|| hyper::upgrade::on(&mut response));
    sanitize_tunnel_response_headers(response.headers_mut(), target_upgrade.is_some());
    if let (Some(downstream), Some(target)) = (downstream_upgrade, target_upgrade) {
        tokio::spawn(async move {
            let Ok(downstream) = downstream.await else {
                return;
            };
            let Ok(target) = target.await else { return };
            let mut downstream = TokioIo::new(downstream);
            let mut target = TokioIo::new(target);
            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut target).await;
        });
    }
    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}

fn sanitize_tunnel_request_headers(
    headers: &mut HeaderMap,
    upgraded: bool,
    hostname: &str,
) -> Result<(), ApiFailure> {
    headers.remove(header::COOKIE);
    headers.remove(header::AUTHORIZATION);
    headers.remove(header::PROXY_AUTHORIZATION);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(hostname)
            .map_err(|error| ApiFailure::bad_gateway("invalid_virtual_host", &error.to_string()))?,
    );
    Ok(())
}

fn sanitize_tunnel_response_headers(headers: &mut HeaderMap, upgraded: bool) {
    headers.remove(header::SET_COOKIE);
    if !upgraded {
        remove_hop_by_hop_headers(headers);
    }
}

fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    for name in [
        header::CONNECTION,
        header::UPGRADE,
        header::TRANSFER_ENCODING,
        header::TE,
        header::TRAILER,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
    headers.remove("proxy-connection");
}

async fn agent_list_machine_services(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_LIST,
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, "*"),
        ))
        .await?;
    list_machine_services(Extension(auth), Path(workspace_id)).await
}

async fn agent_create_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    request.server_id = state
        .resolve_server(&workspace_id, &request.server_id)
        .await?
        .server_id;
    let resource = machine_service_policy_resource(
        &format!("new:{}", request.name.trim().to_ascii_lowercase()),
        &request.name,
        &request.server_id,
        &request.target_host,
        request.target_port,
    );
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_SERVICE_CREATE,
            resource,
        ))
        .await?;
    let service = auth
        .create_machine_service(&workspace_id, &policy_actor_name(&subject), request)
        .await?;
    Ok(Json(json!({ "service": service })))
}

async fn agent_update_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
    Json(mut request): Json<UpdateMachineServiceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let current = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    if let Some(server_id) = request.server_id.as_deref() {
        request.server_id = Some(
            state
                .resolve_server(&workspace_id, server_id)
                .await?
                .server_id,
        );
    }
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_SERVICE_UPDATE,
            machine_service_policy_resource(
                &current.service_id,
                &current.name,
                &current.server_id,
                &current.target_host,
                current.target_port,
            ),
        ))
        .await?;
    let service = auth
        .update_machine_service(
            &workspace_id,
            &current.service_id,
            &policy_actor_name(&subject),
            request,
        )
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "service": service })))
}

async fn agent_delete_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_DELETE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let service = auth
        .delete_machine_service(&workspace_id, &service.service_id)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({
        "deleted": true,
        "service_id": service.service_id,
        "name": service.name,
    })))
}

async fn agent_probe_machine_service(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, service_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let service = auth
        .resolve_machine_service(&workspace_id, &service_id)
        .await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_SERVICE_PROBE,
            machine_service_policy_resource(
                &service.service_id,
                &service.name,
                &service.server_id,
                &service.target_host,
                service.target_port,
            ),
        ))
        .await?;
    let result = state
        .send_command(
            &workspace_id,
            &service.server_id,
            AgentCommand::ProbeNetwork {
                host: service.target_host.clone(),
                port: service.target_port,
                timeout_ms: 3_000,
            },
        )
        .await?;
    Ok(Json(json!({ "service": service, "health": result })))
}

async fn agent_list_virtual_network_hosts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_VIRTUAL_HOST_LIST,
            PolicyResource::new(RESOURCE_VIRTUAL_HOST, "*"),
        ))
        .await?;
    list_virtual_network_hosts(Extension(auth), Path(workspace_id)).await
}

async fn agent_create_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path(workspace_id): Path<String>,
    Json(mut request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    request.hostname = auth::normalize_virtual_hostname(&request.hostname)?;
    let service = auth
        .resolve_machine_service(&workspace_id, &request.service_id)
        .await?;
    request.service_id.clone_from(&service.service_id);
    let resource = virtual_host_policy_resource(&request.hostname, &service);
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject.clone(),
            ACTION_VIRTUAL_HOST_CREATE,
            resource,
        ))
        .await?;
    let host = auth
        .create_virtual_network_host(&workspace_id, &policy_actor_name(&subject), request)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "host": host })))
}

async fn agent_delete_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
    Extension(machine): Extension<MachineSession>,
    headers: HeaderMap,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let subject = agent_policy_subject(&state, &machine, &headers, &workspace_id).await?;
    let host = auth
        .resolve_virtual_network_host(&workspace_id, &hostname)
        .await?
        .ok_or_else(|| {
            ApiFailure::not_found("virtual_host_not_found", "virtual host does not exist")
        })?;
    let service = auth
        .resolve_machine_service(&workspace_id, &host.service_id)
        .await?;
    let resource = virtual_host_policy_resource(&host.hostname, &service);
    policy
        .authorize(&PolicyRequest::new(
            &workspace_id,
            subject,
            ACTION_VIRTUAL_HOST_DELETE,
            resource,
        ))
        .await?;
    auth.delete_virtual_network_host(&workspace_id, &host.hostname)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "deleted": true, "hostname": host.hostname })))
}

async fn publish_virtual_network_hosts(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<(), ApiFailure> {
    let snapshot = virtual_network_hosts_snapshot(auth, workspace_id).await?;
    state
        .broadcast_proxy_message(
            workspace_id,
            &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
        )
        .await;
    Ok(())
}

pub fn spawn_virtual_network_host_refresh(state: AppState, auth: AuthStore) {
    tokio::spawn(async move {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(30));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh.tick().await;
        loop {
            refresh.tick().await;
            if let Err(error) = auth.refresh_virtual_network_hosts().await {
                tracing::warn!(%error, "failed to reload virtual hosts");
                continue;
            }
            let Ok(workspaces) = auth.all_workspaces().await else {
                tracing::warn!("failed to list workspaces for virtual-host refresh");
                continue;
            };
            for workspace in workspaces {
                match virtual_network_hosts_snapshot(&auth, &workspace.workspace_id).await {
                    Ok(snapshot) => {
                        state
                            .broadcast_proxy_message(
                                &workspace.workspace_id,
                                &treer_protocol::ProxyMessage::VirtualNetworkHosts { snapshot },
                            )
                            .await;
                    }
                    Err(error) => {
                        let (_, error) = error.into_parts();
                        tracing::warn!(
                            workspace = %workspace.workspace_id,
                            message = %error.message,
                            "failed to refresh virtual hosts"
                        );
                    }
                }
            }
        }
    });
}

pub(crate) async fn virtual_network_hosts_snapshot(
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<VirtualNetworkHostsSnapshot, auth::AuthFailure> {
    auth.virtual_network_hosts_snapshot(workspace_id).await
}

async fn agent_policy_subject(
    state: &AppState,
    machine: &MachineSession,
    headers: &HeaderMap,
    workspace_id: &str,
) -> Result<PolicySubject, ApiFailure> {
    let agent_id = headers
        .get(AGENT_ID_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::trim)
                .map_err(|_| ProtocolError::new("invalid_agent_identity", "agent ID is invalid"))
        })
        .transpose()?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ProtocolError::new(
                "invalid_agent_identity",
                "managed agent identity is required",
            )
        })?;
    let agent = state.resolve_agent(workspace_id, agent_id).await?;
    if machine
        .server_id
        .as_ref()
        .is_some_and(|server_id| server_id != &agent.server_id)
    {
        return Err(ProtocolError::new(
            "policy_subject_mismatch",
            "agent does not belong to the authenticated machine",
        )
        .into());
    }
    Ok(PolicySubject::Agent {
        server_id: agent.server_id,
        agent_id: agent.agent_id,
    })
}

fn policy_actor_name(subject: &PolicySubject) -> String {
    match subject {
        PolicySubject::Agent { agent_id, .. } => format!("agent:{agent_id}"),
        PolicySubject::Machine { server_id } => format!("machine:{server_id}"),
    }
}

fn virtual_host_policy_resource(hostname: &str, service: &MachineService) -> PolicyResource {
    PolicyResource::new(RESOURCE_VIRTUAL_HOST, hostname)
        .with_attribute("service_id", &service.service_id)
        .with_attribute("destination_server_id", &service.server_id)
        .with_attribute("target_host", &service.target_host)
        .with_attribute("target_port", service.target_port.to_string())
}

fn machine_service_policy_resource(
    service_id: &str,
    name: &str,
    server_id: &str,
    target_host: &str,
    target_port: u16,
) -> PolicyResource {
    PolicyResource::new(RESOURCE_MACHINE_SERVICE, service_id)
        .with_attribute("name", name)
        .with_attribute("server_id", server_id)
        .with_attribute("target_host", target_host)
        .with_attribute("target_port", target_port.to_string())
}

async fn list_agents(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let snapshot = state.snapshot(&workspace_id).await?;
    Ok(Json(json!({ "agents": snapshot.agents })))
}

async fn get_agent(
    State(state): State<AppState>,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(serde_json::to_value(
        state.resolve_agent(&workspace_id, &target).await?,
    )?))
}

async fn rename_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    let name = normalize_display_name(request.name)?;
    auth.set_machine_name(&workspace_id, &server_id, &name)
        .await?;
    Ok(Json(serde_json::to_value(
        state.rename_server(&workspace_id, &server_id, name).await?,
    )?))
}

async fn delete_server(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, server_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let server = state.resolve_server(&workspace_id, &server_id).await?;
    let agents = state
        .snapshot(&workspace_id)
        .await?
        .agents
        .into_iter()
        .filter(|agent| agent.server_id == server_id)
        .collect::<Vec<_>>();
    let stops = agents
        .iter()
        .filter(|agent| !agent.status.is_terminal())
        .map(|agent| async {
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(
                    &workspace_id,
                    &server_id,
                    AgentCommand::Stop {
                        agent_id: agent.agent_id.clone(),
                    },
                ),
            )
            .await
        });
    let _ = futures_util::future::join_all(stops).await;
    let shutdown_requested = if server.labels.get("treer.shutdown").map(String::as_str) == Some("1")
    {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                state.send_command(&workspace_id, &server_id, AgentCommand::ShutdownMachine),
            )
            .await,
            Ok(Ok(_))
        )
    } else {
        false
    };
    let agent_ids = agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    auth.delete_machine(&workspace_id, &server_id, &agent_ids)
        .await?;
    let (server, deleted_agents) = state.delete_server(&workspace_id, &server_id).await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({
        "server": server,
        "deleted_agents": deleted_agents,
        "shutdown_requested": shutdown_requested,
    })))
}

async fn rename_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let name = normalize_display_name(request.name)?;
    auth.set_agent_name(&workspace_id, &agent.agent_id, &name)
        .await?;
    Ok(Json(serde_json::to_value(
        state
            .rename_agent(&workspace_id, &agent.agent_id, name)
            .await?,
    )?))
}

async fn delete_agent(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    if !agent.status.is_terminal() {
        state
            .send_command(
                &workspace_id,
                &agent.server_id,
                AgentCommand::Stop {
                    agent_id: agent.agent_id.clone(),
                },
            )
            .await?;
    }
    auth.delete_agent(&workspace_id, &agent.agent_id).await?;
    Ok(Json(serde_json::to_value(
        state.delete_agent(&workspace_id, &agent.agent_id).await?,
    )?))
}

fn normalize_display_name(name: String) -> Result<String, ProtocolError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ProtocolError::new(
            "invalid_name",
            "name must contain 1-80 visible characters",
        ));
    }
    Ok(name.to_string())
}

async fn create_agent(
    State(state): State<AppState>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let server_id = state
        .select_server(&workspace_id, request.server_id.as_deref())
        .await?;
    let agent_id = format!("ag_{}", Uuid::new_v4().simple());
    let data = state
        .send_command(
            &workspace_id,
            &server_id,
            AgentCommand::Create { agent_id, request },
        )
        .await?;
    Ok(Json(data))
}

async fn prompt_agent(
    State(state): State<AppState>,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<PromptAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Prompt {
                agent_id: agent.agent_id,
                text: request.text,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn input_agent(
    State(state): State<AppState>,
    Path((workspace_id, target)): Path<(String, String)>,
    Json(request): Json<InputAgentRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Input {
                agent_id: agent.agent_id,
                data: request.data,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn read_agent(
    State(state): State<AppState>,
    Path((workspace_id, target)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let lines = query.get("lines").and_then(|value| value.parse().ok());
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Read {
                agent_id: agent.agent_id,
                lines,
            },
        )
        .await?;
    Ok(Json(data))
}

async fn stop_agent(
    State(state): State<AppState>,
    Path((workspace_id, target)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let agent = state.resolve_agent(&workspace_id, &target).await?;
    let data = state
        .send_command(
            &workspace_id,
            &agent.server_id,
            AgentCommand::Stop {
                agent_id: agent.agent_id,
            },
        )
        .await?;
    Ok(Json(data))
}

#[derive(Debug, Deserialize)]
struct TerminalQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

async fn agent_terminal(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    state.resolve_agent_server(&workspace_id, &agent_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_terminal(
            socket,
            state,
            workspace_id,
            agent_id,
            query.cols,
            query.rows,
        )
    }))
}

async fn stream_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    agent_id: String,
    cols: u16,
    rows: u16,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::unbounded_channel::<SocketFrame>();
    let attached = state
        .attach_terminal(&workspace_id, &agent_id, cols, rows, terminal_tx)
        .await;
    let session_id = match attached {
        Ok(session_id) => session_id,
        Err(error) => {
            let message = TerminalServerMessage::Error { error };
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = outgoing.send(Message::Text(encoded.into())).await;
            }
            return;
        }
    };

    loop {
        tokio::select! {
            frame = terminal_rx.recv() => {
                let Some(frame) = frame else { break };
                let message = match frame {
                    SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                    SocketFrame::Binary(data) => Message::Binary(data.into()),
                    SocketFrame::Close => Message::Close(None),
                };
                if outgoing.send(message).await.is_err() {
                    break;
                }
            }
            message = incoming.next() => {
                let Some(Ok(message)) = message else { break };
                let result = match message {
                    Message::Binary(data) => state.terminal_input(&session_id, data.to_vec()).await,
                    Message::Text(text) => match serde_json::from_str::<TerminalClientMessage>(&text) {
                        Ok(TerminalClientMessage::Resize { cols, rows }) => {
                            state.terminal_resize(&session_id, cols, rows).await
                        }
                        Err(error) => Err(ProtocolError::new("invalid_terminal_message", error.to_string())),
                    },
                    Message::Close(_) => break,
                    _ => continue,
                };
                if let Err(error) = result {
                    let message = TerminalServerMessage::Error { error };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        if outgoing.send(Message::Text(encoded.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
    state.detach_terminal(&session_id).await;
}

async fn workspace_events(
    State(state): State<AppState>,
    Extension(browser): Extension<BrowserAccess>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    state.snapshot(&workspace_id).await?;
    Ok(ws.on_upgrade(move |socket| stream_workspace_events(socket, state, workspace_id)))
}

async fn stream_workspace_events(socket: WebSocket, state: AppState, workspace_id: String) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut events = state.subscribe();
    if let Ok(snapshot) = state.snapshot(&workspace_id).await {
        let initial = WorkspaceEvent {
            revision: snapshot.revision,
            workspace_id: workspace_id.clone(),
            event: "workspace.snapshot".to_string(),
            data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
        };
        if send_event(&mut outgoing, &initial).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.workspace_id == workspace_id => {
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let Ok(snapshot) = state.snapshot(&workspace_id).await else { break };
                    let event = WorkspaceEvent {
                        revision: snapshot.revision,
                        workspace_id: workspace_id.clone(),
                        event: "workspace.snapshot".to_string(),
                        data: serde_json::to_value(snapshot).unwrap_or(Value::Null),
                    };
                    if send_event(&mut outgoing, &event).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            message = incoming.next() => {
                if message.is_none() || message.is_some_and(|item| item.is_err()) {
                    break;
                }
            }
        }
    }
}

async fn send_event(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &WorkspaceEvent,
) -> Result<(), axum::Error> {
    let encoded = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    outgoing.send(Message::Text(encoded.into())).await
}

pub struct ApiFailure {
    status: StatusCode,
    error: ProtocolError,
}

impl ApiFailure {
    fn forbidden(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: ProtocolError::new(code, message),
        }
    }

    fn bad_request(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ProtocolError::new(code, message),
        }
    }

    fn not_found(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: ProtocolError::new(code, message),
        }
    }

    fn bad_gateway(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            error: ProtocolError::new(code, message),
        }
    }

    fn internal(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new(code, message),
        }
    }
}

impl From<ProtocolError> for ApiFailure {
    fn from(error: ProtocolError) -> Self {
        let status = match error.code.as_str() {
            "workspace_not_found"
            | "server_not_found"
            | "agent_not_found"
            | "recipient_not_found" => StatusCode::NOT_FOUND,
            "workspace_exists" | "agent_ambiguous" | "server_ambiguous" | "recipient_ambiguous" => {
                StatusCode::CONFLICT
            }
            "policy_denied" | "policy_subject_mismatch" => StatusCode::FORBIDDEN,
            "server_offline" | "no_online_server" | "ssh_unsupported" | "scp_unsupported" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "invalid_agent_identity" | "invalid_name" | "invalid_request" => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::BAD_GATEWAY,
        };
        Self { status, error }
    }
}

impl From<auth::AuthFailure> for ApiFailure {
    fn from(error: auth::AuthFailure) -> Self {
        let (status, error) = error.into_parts();
        Self { status, error }
    }
}

impl From<serde_json::Error> for ApiFailure {
    fn from(error: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new("encode_error", error.to_string()),
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    use tower::ServiceExt;

    async fn state_with_managed_agent() -> AppState {
        let state = AppState::new();
        let now = chrono::Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            labels: Default::default(),
            status: treer_protocol::ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let agent = treer_protocol::AgentInfo {
            agent_id: "agent-a".to_string(),
            workspace_id: "default".to_string(),
            server_id: "machine-a".to_string(),
            kind: "command".to_string(),
            name: "agent-a".to_string(),
            cwd: ".".to_string(),
            status: treer_protocol::AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
        };
        let mut recipient = agent.clone();
        recipient.agent_id = "agent-b".to_string();
        recipient.name = "reviewer".to_string();
        let connection_id = Uuid::new_v4();
        let (outgoing, _incoming) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, outgoing)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                treer_protocol::AgentServerSnapshot {
                    server,
                    agents: vec![agent, recipient],
                },
            )
            .await
            .expect("apply agent snapshot");
        state
    }

    fn test_config() -> BootstrapConfig {
        BootstrapConfig::new(
            Url::parse("https://treer.example/").expect("valid URL"),
            PathBuf::from("dist"),
            Url::parse("https://github.example/releases/latest/download")
                .expect("valid release URL"),
        )
    }

    fn test_browser_access() -> BrowserAccess {
        BrowserAccess::new(&Url::parse("https://app.treer.ai/").expect("app URL"))
            .expect("browser access")
    }

    #[tokio::test]
    async fn trailing_slash_browser_tunnel_route_is_registered() {
        let auth = AuthStore::for_test("admin-password").await;
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/workspaces/default/virtual-hosts/self.test/proxy/")
                    .body(Body::empty())
                    .expect("tunnel request"),
            )
            .await
            .expect("route response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn browser_access_accepts_only_the_configured_origin_when_present() {
        let browser = test_browser_access();
        assert!(browser.validate_if_present(&HeaderMap::new()).is_ok());

        let mut allowed = HeaderMap::new();
        allowed.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://app.treer.ai"),
        );
        assert!(browser.validate_if_present(&allowed).is_ok());

        let mut denied = HeaderMap::new();
        denied.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://other.treer.ai"),
        );
        let error = browser
            .validate_if_present(&denied)
            .expect_err("other browser origin must be denied");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.error.code, "browser_origin_denied");
    }

    #[tokio::test]
    async fn cors_preflight_allows_the_configured_app_with_credentials() {
        let auth = AuthStore::for_test("admin-password").await;
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/auth/login")
                    .header(header::ORIGIN, "https://app.treer.ai")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                    .body(Body::empty())
                    .expect("preflight request"),
            )
            .await
            .expect("preflight response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.treer.ai"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some(&HeaderValue::from_static("true"))
        );
    }

    #[tokio::test]
    async fn cors_headers_are_present_on_authenticated_route_errors() {
        let auth = AuthStore::for_test("admin-password").await;
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        )
        .await
        .expect("identity issuer");
        let app = router(
            AppState::new(),
            test_config(),
            auth,
            PolicyEngine::allow_all(),
            identity,
            test_browser_access(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header(header::ORIGIN, "https://app.treer.ai")
                    .body(Body::empty())
                    .expect("authenticated request"),
            )
            .await
            .expect("authenticated response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://app.treer.ai"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
            Some(&HeaderValue::from_static("true"))
        );
    }

    #[test]
    fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
        let config = test_config();
        let url = install_script_url(&config.public_url);
        assert_eq!(url.as_str(), "https://treer.example/install.sh");
        assert!(url.query().is_none());
    }

    #[tokio::test]
    async fn legacy_enrollment_requests_without_identity_remain_supported() {
        let auth = AuthStore::for_test("admin-password").await;
        let enrollment = auth
            .create_machine_enrollment("default", "admin")
            .await
            .expect("create enrollment");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {enrollment}"))
                .expect("enrollment authorization"),
        );
        let response = enroll_machine(State(AppState::new()), Extension(auth), headers, None)
            .await
            .unwrap_or_else(|error| panic!("legacy enrollment: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn bootstrap_separates_public_installation_from_workspace_connection() {
        let config = test_config();
        let key = "enr_v1_64656661756c74_abc.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let (install, connect) = bootstrap_commands(&config.public_url, key);
        assert_eq!(
            install,
            "curl -fsSL 'https://treer.example/install.sh' | sh"
        );
        assert!(!install.contains("enr_"));
        assert!(!install.contains("connect"));
        assert!(connect.contains(key));
        assert!(connect.contains("treer-agent-server connect --proxy"));
        assert!(!connect.contains("install.sh"));
    }

    #[test]
    fn installer_is_posix_shell_and_only_installs_binaries() {
        let config = test_config();
        let script = render_install_script(&config.public_url);
        assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
        assert!(script.contains("platform=linux-aarch64"));
        assert!(script.contains("transparent agent networking requires unshare(1)"));
        assert!(script.contains("persistent proxy and agent host"));
        assert!(script.contains("container, or other sandbox"));
        assert!(script.contains(".local/libexec/treer"));
        assert!(script.contains("treer-agent-host"));
        assert!(script.contains(
            "ln -sf \"$server_dir/treer-agent-server\" \"$install_dir/treer-agent-server\""
        ));
        assert!(script.contains("https://treer.example/artifacts"));
        assert!(!script.contains("service --workspace"));
        assert!(!script.contains("machine_token"));
        assert!(!script.contains("TREER_MACHINE_TOKEN"));
        assert!(!script.contains("TREER_ENROLLMENT_KEY"));
        assert!(!script.contains("systemctl"));
        assert!(!script.contains("launchctl"));
        assert!(!script.contains("nohup"));
    }

    #[cfg(unix)]
    #[test]
    fn rendered_installer_has_valid_shell_syntax() {
        let config = test_config();
        let script = render_install_script(&config.public_url);
        let mut child = Command::new("sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .spawn()
            .expect("start shell parser");
        child
            .stdin
            .take()
            .expect("shell stdin")
            .write_all(script.as_bytes())
            .expect("write installer");
        assert!(child.wait().expect("wait for shell parser").success());
    }

    #[test]
    fn artifact_paths_reject_directory_traversal() {
        assert!(valid_artifact_component("linux-aarch64"));
        assert!(!valid_artifact_component("../linux-aarch64"));
        assert!(!valid_artifact_component("linux/aarch64"));
    }

    #[test]
    fn release_artifact_names_match_tagged_assets() {
        let config = test_config();
        let url = release_artifact_url(&config, "darwin-aarch64", "treer-agent-host")
            .unwrap_or_else(|_| panic!("release artifact URL"));
        assert_eq!(
            url.as_str(),
            "https://github.example/releases/latest/download/treer-agent-host-darwin-aarch64"
        );
    }

    #[tokio::test]
    async fn missing_local_artifacts_redirect_to_the_release() {
        let mut config = test_config();
        config.artifacts_dir = std::env::temp_dir().join(format!(
            "treer-missing-artifacts-{}",
            Uuid::new_v4().simple()
        ));
        let response = download_artifact(
            Extension(config),
            Path(("darwin-aarch64".to_string(), "treer".to_string())),
        )
        .await
        .unwrap_or_else(|_| panic!("missing artifact should redirect"));
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://github.example/releases/latest/download/treer-darwin-aarch64")
        );
    }

    #[test]
    fn display_names_are_trimmed_and_validated() {
        assert_eq!(
            normalize_display_name("  build machine  ".to_string()).expect("valid name"),
            "build machine"
        );
        assert!(normalize_display_name("  ".to_string()).is_err());
        assert!(normalize_display_name("bad\nname".to_string()).is_err());
        assert!(normalize_display_name("x".repeat(81)).is_err());
    }

    #[tokio::test]
    async fn agent_policy_subject_is_bound_to_authenticated_machine() {
        let state = state_with_managed_agent().await;
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let missing = agent_policy_subject(&state, &machine, &HeaderMap::new(), "default")
            .await
            .expect_err("agent identity is required");
        assert_eq!(missing.status, StatusCode::BAD_REQUEST);
        assert_eq!(missing.error.code, "invalid_agent_identity");

        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
        let subject = agent_policy_subject(&state, &machine, &headers, "default")
            .await
            .unwrap_or_else(|error| panic!("matching subject: {}", error.error.message));
        assert_eq!(
            subject,
            PolicySubject::Agent {
                server_id: "machine-a".to_string(),
                agent_id: "agent-a".to_string(),
            }
        );

        let error = agent_policy_subject(
            &state,
            &MachineSession {
                server_id: Some("machine-b".to_string()),
                workspace_id: Some("default".to_string()),
            },
            &headers,
            "default",
        )
        .await
        .expect_err("foreign machine must not claim the agent");
        assert_eq!(error.status, StatusCode::FORBIDDEN);
        assert_eq!(error.error.code, "policy_subject_mismatch");
    }

    #[tokio::test]
    async fn agent_identity_tokens_use_the_canonical_service_audience() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test",
                CreateMachineServiceRequest {
                    name: "api".to_string(),
                    server_id: "machine-a".to_string(),
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create service");
        let identity = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("identity issuer");
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

        let response = agent_issue_identity_token(
            State(state),
            Extension(WorkloadIdentityApi {
                auth,
                policy: PolicyEngine::allow_all(),
                issuer: identity.clone(),
            }),
            Extension(machine),
            headers,
            Path("default".to_string()),
            Json(WorkloadIdentityTokenRequest {
                audience: "api".to_string(),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("issue identity token: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read token response");
        let token: treer_protocol::WorkloadIdentityTokenResponse =
            serde_json::from_slice(&body).expect("decode token response");
        assert_eq!(token.audience, service.service_id);
        let verified = identity.verify(&token.access_token, &service.service_id);
        assert!(verified.active);
        let claims = verified.claims.expect("verified claims");
        assert_eq!(claims.sub, "agent-a");
        assert_eq!(claims.machine_id, "machine-a");
    }

    #[tokio::test]
    async fn mail_recipients_share_one_agent_and_human_namespace() {
        let snapshot = state_with_managed_agent()
            .await
            .snapshot("default")
            .await
            .expect("workspace snapshot");
        let humans = vec![
            WorkspaceHuman {
                user_id: "usr_owner".to_string(),
                preferred_name: "Owner".to_string(),
                role: "owner".to_string(),
            },
            WorkspaceHuman {
                user_id: "usr_reviewer".to_string(),
                preferred_name: "reviewer".to_string(),
                role: "member".to_string(),
            },
        ];

        let human = resolve_mail_recipient(&snapshot.agents, &humans, "Owner")
            .expect("unique preferred name");
        assert_eq!(human.kind, MailAddressKind::Human);
        assert_eq!(human.id, "usr_owner");

        let stable_id = resolve_mail_recipient(&snapshot.agents, &humans, "usr_reviewer")
            .expect("stable human id");
        assert_eq!(stable_id.kind, MailAddressKind::Human);

        let ambiguous = resolve_mail_recipient(&snapshot.agents, &humans, "reviewer")
            .expect_err("Agent and human display names share one namespace");
        assert_eq!(ambiguous.code, "recipient_ambiguous");
    }

    #[tokio::test]
    async fn managed_agent_mail_resolves_recipients_without_interrupting_runtime() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let policy = PolicyEngine::allow_all();
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut sender_headers = HeaderMap::new();
        sender_headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

        let sent = agent_send_mail(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            sender_headers,
            Path("default".to_string()),
            Json(SendAgentMailRequest {
                recipients: vec!["reviewer".to_string()],
                context_ids: vec![],
                body: "Check this when convenient.".to_string(),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("send mail: {}", error.error.message));
        assert_eq!(sent.0.message.sender.id, "agent-a");
        assert_eq!(sent.0.message.recipients[0].id, "agent-b");
        assert_eq!(
            state
                .resolve_agent("default", "agent-b")
                .await
                .expect("recipient remains available")
                .status,
            treer_protocol::AgentStatus::Idle
        );

        let mut recipient_headers = HeaderMap::new();
        recipient_headers.insert(AGENT_ID_HEADER, "agent-b".parse().expect("agent header"));
        let inbox = agent_read_inbox(
            State(state),
            Extension(auth),
            Extension(policy),
            Extension(machine),
            recipient_headers,
            Path("default".to_string()),
            Json(AgentInboxRequest { limit: 50 }),
        )
        .await
        .unwrap_or_else(|error| panic!("read inbox: {}", error.error.message));
        assert_eq!(inbox.0.messages, [sent.0.message]);
        assert_eq!(inbox.0.remaining_unread, 0);
    }

    #[tokio::test]
    async fn managed_agent_can_manage_services_and_virtual_hosts() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let policy = PolicyEngine::allow_all();
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

        let service = agent_create_machine_service(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            headers.clone(),
            Path("default".to_string()),
            Json(CreateMachineServiceRequest {
                name: "API".to_string(),
                server_id: "machine-a".to_string(),
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("create service: {}", error.error.message));
        let service_id = service.0["service"]["service_id"]
            .as_str()
            .expect("service id")
            .to_string();
        assert_eq!(service.0["service"]["created_by"], "agent:agent-a");

        let created = agent_create_virtual_network_host(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            headers.clone(),
            Path("default".to_string()),
            Json(CreateVirtualNetworkHostRequest {
                hostname: "API.Internal".to_string(),
                service_id: service_id.clone(),
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("create virtual host: {}", error.error.message));
        assert_eq!(created.0["host"]["hostname"], "api.internal");
        assert_eq!(created.0["host"]["created_by"], "agent:agent-a");

        let listed = agent_list_virtual_network_hosts(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            headers.clone(),
            Path("default".to_string()),
        )
        .await
        .unwrap_or_else(|error| panic!("list virtual hosts: {}", error.error.message));
        assert_eq!(listed.0["hosts"].as_array().map(Vec::len), Some(1));

        let deleted = agent_delete_virtual_network_host(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            headers.clone(),
            Path(("default".to_string(), "api.internal".to_string())),
        )
        .await
        .unwrap_or_else(|error| panic!("delete virtual host: {}", error.error.message));
        assert_eq!(deleted.0["deleted"], true);
        assert_eq!(deleted.0["hostname"], "api.internal");

        let deleted_service = agent_delete_machine_service(
            State(state),
            Extension(auth),
            Extension(policy),
            Extension(machine),
            headers,
            Path(("default".to_string(), service_id)),
        )
        .await
        .unwrap_or_else(|error| panic!("delete service: {}", error.error.message));
        assert_eq!(deleted_service.0["deleted"], true);
    }

    #[tokio::test]
    async fn browser_tunnel_rejects_tcp_services_before_opening_a_stream() {
        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "database".to_string(),
                    server_id: "machine-a".to_string(),
                    target_host: "127.0.0.1".to_string(),
                    target_port: 5432,
                    protocol: treer_protocol::MachineServiceProtocol::Tcp,
                },
            )
            .await
            .expect("create machine service");
        auth.create_virtual_network_host(
            "default",
            "test-user",
            CreateVirtualNetworkHostRequest {
                hostname: "database.internal".to_string(),
                service_id: service.service_id,
            },
        )
        .await
        .expect("create virtual host");

        let error = proxy_virtual_network_host(
            AppState::new(),
            auth,
            "default".to_string(),
            "database.internal".to_string(),
            String::new(),
            Request::new(Body::empty()),
        )
        .await
        .expect_err("TCP services must not enter the HTTP tunnel");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.error.code, "service_protocol_mismatch");
    }

    #[tokio::test]
    async fn browser_tunnel_forwards_http_without_leaking_gateway_credentials() {
        let state = AppState::new();
        let now = chrono::Utc::now();
        let server = treer_protocol::ServerInfo {
            server_id: "machine-a".to_string(),
            workspace_id: "default".to_string(),
            name: "machine-a".to_string(),
            hostname: "machine-a".to_string(),
            root: "/tmp".to_string(),
            labels: Default::default(),
            status: treer_protocol::ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server(server, connection_id, server_tx)
            .await
            .expect("register controller");

        let auth = AuthStore::for_test("admin-password").await;
        auth.seed_test_workspace("default").await;
        let service = auth
            .create_machine_service(
                "default",
                "test-user",
                CreateMachineServiceRequest {
                    name: "app".to_string(),
                    server_id: "machine-a".to_string(),
                    target_host: "127.0.0.1".to_string(),
                    target_port: 8080,
                    protocol: treer_protocol::MachineServiceProtocol::Http,
                },
            )
            .await
            .expect("create machine service");
        auth.create_virtual_network_host(
            "default",
            "test-user",
            CreateVirtualNetworkHostRequest {
                hostname: "app.internal".to_string(),
                service_id: service.service_id,
            },
        )
        .await
        .expect("create virtual host");

        let controller_state = state.clone();
        let controller = tokio::spawn(async move {
            let open = match server_rx.recv().await.expect("network open") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
                }
                _ => panic!("expected network open"),
            };
            assert_eq!(open.kind, treer_protocol::NetworkBinaryKind::Open);
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Opened,
                        stream_id: open.stream_id.clone(),
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("open stream");

            let request = loop {
                let frame = match server_rx.recv().await.expect("HTTP request frame") {
                    SocketFrame::Binary(encoded) => {
                        treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                    }
                    _ => continue,
                };
                if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                    break String::from_utf8(frame.payload).expect("HTTP request text");
                }
            };
            assert!(request.starts_with("GET /status?full=1 HTTP/1.1\r\n"));
            assert!(request.contains("host: app.internal\r\n"));
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));

            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nSet-Cookie: internal=secret\r\n\r\nhello".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
            controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::HalfClose,
                        stream_id: open.stream_id,
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("close response");
        });

        let request = Request::builder()
            .uri("/ignored?full=1")
            .header(header::COOKIE, "treer_session=secret")
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .expect("browser request");
        let response = proxy_virtual_network_host(
            state,
            auth,
            "default".to_string(),
            "app.internal".to_string(),
            "status".to_string(),
            request,
        )
        .await
        .unwrap_or_else(|error| panic!("tunnel request: {}", error.error.message));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("read tunnel response");
        assert_eq!(&body[..], b"hello");
        controller.await.expect("join controller");
    }
}
