use std::collections::HashMap;
use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use treer_protocol::{
    AgentCommand, ApiError, CreateAgentRequest, CreateWorkspaceRequest, InputAgentRequest,
    PromptAgentRequest, ProtocolError, TerminalClientMessage, TerminalServerMessage,
    WorkspaceEvent,
};
use url::Url;
use uuid::Uuid;

use crate::agent_socket;
use crate::auth::{self, AuthStore, CurrentSession};
use crate::state::{AppState, SocketFrame};

const INDEX_HTML: &str = include_str!("../../../web/index.html");
const XTERM_JS: &str = include_str!("../../../web/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../../../web/vendor/xterm.css");
const XTERM_FIT_JS: &str = include_str!("../../../web/vendor/addon-fit.js");

#[derive(Clone)]
pub struct BootstrapConfig {
    public_url: Url,
    artifacts_dir: PathBuf,
}

impl BootstrapConfig {
    pub fn new(public_url: Url, artifacts_dir: PathBuf) -> Self {
        Self {
            public_url,
            artifacts_dir,
        }
    }
}

pub fn router(state: AppState, bootstrap: BootstrapConfig, auth_store: AuthStore) -> Router {
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
            get(get_agent),
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
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_machine,
        ));
    let authenticated = Router::new()
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
            "/api/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent),
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
            auth::require_user,
        ));
    let administrator = Router::new()
        .route("/api/admin/invitations", post(auth::create_invitation))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_admin,
        ));
    Router::new()
        .route("/", get(index))
        .route("/install.sh", post(install_script))
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
        .merge(administrator)
        .layer(Extension(bootstrap))
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
    let script_url = install_script_url(&config.public_url);
    let authorization = format!("Authorization: Bearer {enrollment}");
    Ok(Json(json!({
        "command": format!(
            "curl -fsSL -X POST -H {} {} | sh",
            shell_quote(&authorization),
            shell_quote(script_url.as_str()),
        ),
        "script_url": script_url.as_str(),
        "workspace_id": workspace_id,
    })))
}

async fn install_script(
    Extension(config): Extension<BootstrapConfig>,
    Extension(auth): Extension<AuthStore>,
    headers: HeaderMap,
) -> Result<Response, ApiFailure> {
    let enrollment = auth.claim_machine_enrollment_from_headers(&headers).await?;
    let script = render_install_script(
        &config.public_url,
        &enrollment.workspace_id,
        &enrollment.server_id,
        &enrollment.machine_token,
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        script,
    )
        .into_response())
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
    let data = tokio::fs::read(&path).await.map_err(|error| {
        tracing::warn!(path = %path.display(), %error, "bootstrap artifact unavailable");
        ApiFailure::not_found("artifact_not_found", "artifact not found")
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        data,
    )
        .into_response())
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

fn render_install_script(
    public_url: &Url,
    workspace_id: &str,
    server_id: &str,
    machine_token: &str,
) -> String {
    let mut artifact_base = public_url.clone();
    artifact_base.set_path("/artifacts/");
    format!(
        r#"#!/bin/sh
set -eu

proxy_url={proxy_url}
artifact_base={artifact_base}
workspace={workspace}
server_id={server_id}
machine_token={machine_token}
workspace_root=${{TREER_WORKSPACE_ROOT:-$(pwd)}}
install_dir=${{TREER_INSTALL_DIR:-"${{HOME:?HOME is required}}/.local/bin"}}
server_dir=${{TREER_AGENT_SERVER_INSTALL_DIR:-"${{HOME}}/.local/libexec/treer"}}
state_dir=${{TREER_STATE_DIR:-"${{HOME}}/.local/state/treer"}}
listen=${{TREER_AGENT_SERVER_LISTEN:-127.0.0.1:8790}}

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

mkdir -p "$install_dir" "$server_dir" "$state_dir"
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

workspace_key=$(printf '%s' "$workspace" | tr -c 'A-Za-z0-9_.-' '_')
pid_file="$state_dir/agent-server-$workspace_key.pid"
if [ -f "$pid_file" ]; then
  old_pid=$(cat "$pid_file" 2>/dev/null || true)
  if [ -n "$old_pid" ] && kill -0 "$old_pid" 2>/dev/null; then
    old_command=$(ps -p "$old_pid" -o command= 2>/dev/null || true)
    case "$old_command" in
      *treer-agent-server*)
        echo "treer: stopping legacy agent server (pid $old_pid)"
        kill "$old_pid" 2>/dev/null || true
        sleep 1
        if kill -0 "$old_pid" 2>/dev/null; then
          echo "treer: legacy agent server did not stop (pid $old_pid)" >&2
          exit 1
        fi
        ;;
      *) echo "treer: ignoring stale agent server pid $old_pid" ;;
    esac
  fi
  rm -f "$pid_file"
fi

TREER_STATE_DIR="$state_dir" TREER_MACHINE_TOKEN="$machine_token" \
  "$server_dir/treer-agent-server" \
  service --workspace "$workspace" install \
  --proxy "$proxy_url" \
  --server-id "$server_id" \
  --root "$workspace_root" \
  --listen "$listen"

echo "treer: workspace $workspace at $workspace_root"
echo "treer: add $install_dir to PATH to use the treer command"
echo "treer: manage the host service with $server_dir/treer-agent-server service --workspace $workspace <status|stop|start|restart|logs|uninstall>"
"#,
        proxy_url = shell_quote(public_url.as_str()),
        artifact_base = shell_quote(artifact_base.as_str().trim_end_matches('/')),
        workspace = shell_quote(workspace_id),
        server_id = shell_quote(server_id),
        machine_token = shell_quote(machine_token),
    )
}

async fn list_workspaces(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "workspaces": state.list_workspaces().await }))
}

async fn create_workspace(
    State(state): State<AppState>,
    Json(request): Json<CreateWorkspaceRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let workspace_id = request
        .workspace_id
        .unwrap_or_else(|| format!("ws_{}", Uuid::new_v4().simple()));
    let info = state.create_workspace(workspace_id, request.name).await?;
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
    Path((workspace_id, agent_id)): Path<(String, String)>,
    Query(query): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    state.resolve_agent_server(&workspace_id, &agent_id).await?;
    Ok(ws.on_upgrade(move |socket| {
        stream_agent_terminal(socket, state, workspace_id, agent_id, query)
    }))
}

async fn stream_agent_terminal(
    socket: WebSocket,
    state: AppState,
    workspace_id: String,
    agent_id: String,
    query: TerminalQuery,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::unbounded_channel::<SocketFrame>();
    let session_id = match state
        .attach_terminal(
            &workspace_id,
            &agent_id,
            query.cols,
            query.rows,
            terminal_tx,
        )
        .await
    {
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
            "workspace_exists" | "agent_ambiguous" => StatusCode::CONFLICT,
            "server_offline" | "no_online_server" => StatusCode::SERVICE_UNAVAILABLE,
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

    fn test_config() -> BootstrapConfig {
        BootstrapConfig::new(
            Url::parse("https://treer.example/").expect("valid URL"),
            PathBuf::from("dist"),
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
    fn installer_is_posix_shell_and_contains_runtime_configuration() {
        let config = test_config();
        let script =
            render_install_script(&config.public_url, "default", "srv_test", "srv_test.secret");
        assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
        assert!(script.contains("platform=linux-aarch64"));
        assert!(script.contains(".local/libexec/treer"));
        assert!(script.contains("treer-agent-host"));
        assert!(script.contains("service --workspace \"$workspace\" install"));
        assert!(script.contains("--proxy \"$proxy_url\""));
        assert!(script.contains("--server-id \"$server_id\""));
        assert!(script.contains("TREER_MACHINE_TOKEN=\"$machine_token\""));
        assert!(script.contains("workspace='default'"));
        assert!(script.contains("https://treer.example/artifacts"));
        assert!(!script.contains("nohup"));
    }

    #[test]
    fn artifact_paths_reject_directory_traversal() {
        assert!(valid_artifact_component("linux-aarch64"));
        assert!(!valid_artifact_component("../linux-aarch64"));
        assert!(!valid_artifact_component("linux/aarch64"));
    }
}
