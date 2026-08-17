use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use treer_protocol::{
    AgentCommand, ApiError, CreateAgentRequest, CreateVirtualNetworkHostRequest, InputAgentRequest,
    MachineEnrollmentResponse, PromptAgentRequest, ProtocolError, RenameRequest,
    TerminalClientMessage, TerminalServerMessage, TransferServerMessage, WorkspaceEvent,
    AGENT_ID_HEADER,
};
use url::Url;
use uuid::Uuid;

use crate::agent_socket;
use crate::auth::{self, AuthStore, CurrentSession, MachineSession};
use crate::policy::{
    PolicyEngine, PolicyRequest, PolicyResource, PolicySubject, ACTION_VIRTUAL_HOST_CREATE,
    ACTION_VIRTUAL_HOST_DELETE, ACTION_VIRTUAL_HOST_LIST, RESOURCE_VIRTUAL_HOST,
};
use crate::state::{AppState, ShellOptions, SocketFrame, TransferDirection, TransferOptions};

const INDEX_HTML: &str = include_str!("../../../web/dist/index.html");
const XTERM_JS: &str = include_str!("../../../web/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../../../web/vendor/xterm.css");
const XTERM_FIT_JS: &str = include_str!("../../../web/vendor/addon-fit.js");

#[derive(Clone)]
pub struct BootstrapConfig {
    public_url: Url,
    artifacts_dir: PathBuf,
    release_artifact_base_url: Url,
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

pub fn router(
    state: AppState,
    bootstrap: BootstrapConfig,
    auth_store: AuthStore,
    policy: PolicyEngine,
) -> Router {
    let agent_control = Router::new()
        .route(
            "/agent/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
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
        .route(
            "/agent/workspaces/{workspace_id}/ssh/{server_id}",
            get(shell_terminal),
        )
        .route(
            "/agent/workspaces/{workspace_id}/scp/{server_id}",
            get(file_transfer),
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
            "/api/organizations/{organization_id}/members",
            get(auth::members),
        )
        .route(
            "/api/organizations/{organization_id}/members/{username}",
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
        .route("/api/workspaces/{workspace_id}/servers", get(list_servers))
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts",
            get(list_virtual_network_hosts).post(create_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(delete_virtual_network_host),
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
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_workspace_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_user,
        ));
    Router::new()
        .route("/", get(index))
        .route("/install.sh", get(install_script))
        .route("/api/machines/enroll", post(enroll_machine))
        .route("/artifacts/{platform}/{binary}", get(download_artifact))
        .route("/assets/xterm.js", get(xterm_js))
        .route("/assets/xterm.css", get(xterm_css))
        .route("/assets/addon-fit.js", get(xterm_fit_js))
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/register", post(auth::register))
        .route("/agent/connect", get(agent_socket::upgrade))
        .merge(agent_control)
        .merge(authenticated)
        .layer(Extension(bootstrap))
        .layer(Extension(policy))
        .layer(Extension(auth_store))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn xterm_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        XTERM_JS,
    )
}

async fn xterm_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        XTERM_CSS,
    )
}

async fn xterm_fit_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        XTERM_FIT_JS,
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "treer-proxy" }))
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
        .create_machine_enrollment(&workspace_id, &session.username)
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
        "TREER_ENROLLMENT_KEY={} \"$HOME/.local/libexec/treer/treer-agent-server\" connect --proxy {}",
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
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
) -> Result<Response, ApiFailure> {
    let enrollment = auth.claim_machine_enrollment_from_headers(&headers).await?;
    let response = MachineEnrollmentResponse {
        workspace_id: enrollment.workspace_id,
        server_id: enrollment.server_id,
        machine_token: enrollment.machine_token,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response())
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

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
  Linux-aarch64|Linux-arm64) platform=linux-aarch64 ;;
  Darwin-x86_64|Darwin-amd64) platform=darwin-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) platform=darwin-aarch64 ;;
  *) echo "treer: unsupported platform $(uname -s)/$(uname -m)" >&2; exit 1 ;;
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

echo "treer: binaries installed"
echo "treer: add $install_dir to PATH to use the treer command"
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
            .list_workspaces(&query.organization_id, &session.username)
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
            &session.username,
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
    Json(mut request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let destination = state
        .resolve_server(&workspace_id, &request.destination_server_id)
        .await?;
    request.destination_server_id = destination.server_id;
    let host = auth
        .create_virtual_network_host(&workspace_id, &session.username, request)
        .await?;
    Ok(Json(json!({ "host": host })))
}

async fn delete_virtual_network_host(
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.delete_virtual_network_host(&workspace_id, &hostname)
        .await?;
    Ok(Json(json!({ "deleted": true })))
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
    let destination = state
        .resolve_server(&workspace_id, &request.destination_server_id)
        .await?;
    request.destination_server_id = destination.server_id;
    let resource = virtual_host_policy_resource(
        &request.hostname,
        &request.destination_server_id,
        &request.target_host,
        request.target_port,
    );
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
    let resource = virtual_host_policy_resource(
        &host.hostname,
        &host.destination_server_id,
        &host.target_host,
        host.target_port,
    );
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
    Ok(Json(json!({ "deleted": true, "hostname": host.hostname })))
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

fn virtual_host_policy_resource(
    hostname: &str,
    destination_server_id: &str,
    target_host: &str,
    target_port: Option<u16>,
) -> PolicyResource {
    let resource = PolicyResource::new(RESOURCE_VIRTUAL_HOST, hostname)
        .with_attribute("destination_server_id", destination_server_id)
        .with_attribute("target_host", target_host);
    if let Some(port) = target_port {
        resource.with_attribute("target_port", port.to_string())
    } else {
        resource
    }
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
    state.resolve_server(&workspace_id, &server_id).await?;
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
    let agent_ids = agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    auth.delete_machine(&workspace_id, &server_id, &agent_ids)
        .await?;
    let (server, deleted_agents) = state.delete_server(&workspace_id, &server_id).await?;
    Ok(Json(json!({
        "server": server,
        "deleted_agents": deleted_agents,
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

#[derive(Debug, Deserialize)]
struct ShellQuery {
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
    #[serde(default = "default_shell_cwd")]
    cwd: String,
    #[serde(default)]
    command: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransferDirectionQuery {
    Upload,
    Download,
}

#[derive(Debug, Deserialize)]
struct TransferQuery {
    direction: TransferDirectionQuery,
    path: String,
    #[serde(default)]
    recursive: bool,
}

fn default_shell_cwd() -> String {
    ".".to_string()
}

const fn default_terminal_cols() -> u16 {
    120
}

const fn default_terminal_rows() -> u16 {
    36
}

async fn agent_terminal(
    State(state): State<AppState>,
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    state.resolve_agent_server(&workspace_id, &agent_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_terminal(
            socket,
            state,
            workspace_id,
            TerminalTarget::Agent(agent_id),
            query.cols,
            query.rows,
        )
    }))
}

async fn shell_terminal(
    State(state): State<AppState>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Query(query): Query<ShellQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_terminal(
            socket,
            state,
            workspace_id,
            TerminalTarget::Shell {
                server_id,
                cwd: query.cwd,
                command: query.command,
            },
            query.cols,
            query.rows,
        )
    }))
}

async fn file_transfer(
    State(state): State<AppState>,
    Path((workspace_id, server_id)): Path<(String, String)>,
    Query(query): Query<TransferQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    state.resolve_server(&workspace_id, &server_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_file_transfer(socket, state, workspace_id, server_id, query)
    }))
}

async fn stream_file_transfer(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    server_id: String,
    query: TransferQuery,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (transfer_tx, mut transfer_rx) = tokio::sync::mpsc::channel::<SocketFrame>(16);
    let direction = match query.direction {
        TransferDirectionQuery::Upload => TransferDirection::Upload,
        TransferDirectionQuery::Download => TransferDirection::Download,
    };
    let session_id = match state
        .attach_transfer(
            &workspace_id,
            &server_id,
            TransferOptions {
                path: query.path,
                recursive: query.recursive,
                direction,
            },
            transfer_tx,
        )
        .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            let message = TransferServerMessage::Error { error };
            if let Ok(encoded) = serde_json::to_string(&message) {
                let _ = outgoing.send(Message::Text(encoded.into())).await;
            }
            return;
        }
    };

    loop {
        tokio::select! {
            frame = transfer_rx.recv() => {
                let Some(frame) = frame else { break };
                let finished = matches!(
                    &frame,
                    SocketFrame::Text(encoded)
                        if serde_json::from_str::<TransferServerMessage>(encoded).is_ok_and(
                            |message| matches!(
                                message,
                                TransferServerMessage::Complete { .. }
                                    | TransferServerMessage::Error { .. }
                            )
                        )
                );
                let message = match frame {
                    SocketFrame::Text(encoded) => Message::Text(encoded.into()),
                    SocketFrame::Binary(data) => Message::Binary(data.into()),
                    SocketFrame::Close => Message::Close(None),
                };
                if outgoing.send(message).await.is_err() || finished {
                    break;
                }
            }
            message = incoming.next() => {
                let Some(Ok(message)) = message else { break };
                let result = match message {
                    Message::Binary(data) => state.transfer_input(&session_id, data.to_vec()).await,
                    Message::Close(_) => break,
                    Message::Text(_) => Err(ProtocolError::new(
                        "invalid_transfer_message",
                        "file transfers accept binary data frames only",
                    )),
                    _ => continue,
                };
                if let Err(error) = result {
                    let message = TransferServerMessage::Error { error };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        if outgoing.send(Message::Text(encoded.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
    state.detach_transfer(&session_id).await;
}

enum TerminalTarget {
    Agent(String),
    Shell {
        server_id: String,
        cwd: String,
        command: Option<String>,
    },
}

async fn stream_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    target: TerminalTarget,
    cols: u16,
    rows: u16,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::unbounded_channel::<SocketFrame>();
    let attached = match target {
        TerminalTarget::Agent(agent_id) => {
            state
                .attach_terminal(&workspace_id, &agent_id, cols, rows, terminal_tx)
                .await
        }
        TerminalTarget::Shell {
            server_id,
            cwd,
            command,
        } => {
            state
                .attach_shell(
                    &workspace_id,
                    &server_id,
                    ShellOptions {
                        cwd,
                        command,
                        cols,
                        rows,
                    },
                    terminal_tx,
                )
                .await
        }
    };
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
    Path(workspace_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
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
    fn not_found(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: ProtocolError::new(code, message),
        }
    }
}

impl From<ProtocolError> for ApiFailure {
    fn from(error: ProtocolError) -> Self {
        let status = match error.code.as_str() {
            "workspace_not_found" | "server_not_found" | "agent_not_found" => StatusCode::NOT_FOUND,
            "workspace_exists" | "agent_ambiguous" | "server_ambiguous" => StatusCode::CONFLICT,
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
                    agents: vec![agent],
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

    #[test]
    fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
        let config = test_config();
        let url = install_script_url(&config.public_url);
        assert_eq!(url.as_str(), "https://treer.example/install.sh");
        assert!(url.query().is_none());
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
        assert!(connect.contains("treer-agent-server\" connect --proxy"));
        assert!(!connect.contains("install.sh"));
    }

    #[test]
    fn installer_is_posix_shell_and_only_installs_binaries() {
        let config = test_config();
        let script = render_install_script(&config.public_url);
        assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
        assert!(script.contains("platform=linux-aarch64"));
        assert!(script.contains(".local/libexec/treer"));
        assert!(script.contains("treer-agent-host"));
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
    async fn managed_agent_can_create_list_and_delete_virtual_hosts() {
        let state = state_with_managed_agent().await;
        let auth = AuthStore::in_memory("admin-password").await;
        let policy = PolicyEngine::allow_all();
        let machine = MachineSession {
            server_id: Some("machine-a".to_string()),
            workspace_id: Some("default".to_string()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

        let created = agent_create_virtual_network_host(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(machine.clone()),
            headers.clone(),
            Path("default".to_string()),
            Json(CreateVirtualNetworkHostRequest {
                hostname: "API.Internal".to_string(),
                destination_server_id: "machine-a".to_string(),
                target_host: "127.0.0.1".to_string(),
                target_port: Some(8080),
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
            State(state),
            Extension(auth),
            Extension(policy),
            Extension(machine),
            headers,
            Path(("default".to_string(), "api.internal".to_string())),
        )
        .await
        .unwrap_or_else(|error| panic!("delete virtual host: {}", error.error.message));
        assert_eq!(deleted.0["deleted"], true);
        assert_eq!(deleted.0["hostname"], "api.internal");
    }
}
