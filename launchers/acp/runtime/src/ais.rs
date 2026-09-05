use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
#[cfg(feature = "remote-codex-ui")]
use tokio::sync::broadcast;
use tokio::sync::Mutex;
#[cfg(feature = "remote-codex-ui")]
use tower_http::services::ServeDir;
use treer_protocol::{
    AgentInterfaceManifest, AgentInterfaceStatusResponse, AgentStatus, AgentTranscriptEntry,
    AgentTranscriptResponse, AGENT_INTERFACE_PROTOCOL_V1,
};
use uuid::Uuid;

use crate::acp::{augment_path, AcpRuntime};
use crate::cancel::Cancel;
use crate::files::{list_tree, preview_file, write_file};
use crate::journal::{history_to_entry, Journal};
use crate::transcript::transcript_page_from_entries;
use crate::types::{now_rfc3339, AIS_CAPABILITIES};

#[cfg(feature = "remote-codex-ui")]
#[path = "optional_ui/remote_codex.rs"]
mod remote_codex;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const FILE_PREVIEW_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub enum HarnessSpec {
    Fake,
    Configured {
        name: String,
        base_command: String,
        server_command: String,
    },
}

#[derive(Clone, Debug)]
pub struct AisConfig {
    pub agent_id: String,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
    pub port: u16,
    pub ui_dist: Option<PathBuf>,
    pub harness: HarnessSpec,
    pub bind_session_id: Option<String>,
    pub startup_timeout_ms: u64,
}

pub struct AisServer {
    pub port: u16,
    pub instance_id: String,
    pub journal_path: PathBuf,
    pub ui_path: Option<String>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl AisServer {
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            match task.await {
                Ok(result) => result,
                Err(err) if err.is_cancelled() => Ok(()),
                Err(err) => Err(anyhow!("AIS server task failed: {err}")),
            }
        } else {
            Ok(())
        }
    }
}

struct RuntimeStatus {
    status: AgentStatus,
    busy: bool,
    error: Option<String>,
}

struct AppState {
    agent_id: String,
    instance_id: String,
    cwd: PathBuf,
    journal: Arc<Journal>,
    runtime: Arc<AcpRuntime>,
    session_key: String,
    #[cfg(feature = "remote-codex-ui")]
    harness_id: String,
    #[cfg(feature = "remote-codex-ui")]
    harness_label: String,
    status: Mutex<RuntimeStatus>,
    abort: Mutex<Option<Cancel>>,
    #[cfg(feature = "remote-codex-ui")]
    ui_events: broadcast::Sender<Value>,
    ui_path: Option<String>,
}

pub async fn serve(config: AisConfig) -> Result<AisServer> {
    augment_path();
    #[cfg(not(feature = "remote-codex-ui"))]
    if config.ui_dist.is_some() {
        anyhow::bail!("--ui-dist requires a runtime built with remote-codex-ui support");
    }
    std::fs::create_dir_all(&config.cwd)
        .with_context(|| format!("create cwd {}", config.cwd.display()))?;
    std::fs::create_dir_all(&config.state_dir)
        .with_context(|| format!("create state dir {}", config.state_dir.display()))?;
    let journal_path = config.state_dir.join("journal.sqlite");
    let journal = Arc::new(Journal::open(&journal_path)?);

    let (def, runtime) = harness_runtime(&config)?;
    if crate::acp::classify_availability(&def) != "ready" {
        anyhow::bail!(
            "{} is not available ({})",
            def.display_name,
            crate::acp::classify_availability(&def)
        );
    }
    let bound_id = config
        .bind_session_id
        .clone()
        .or_else(|| {
            journal
                .bound_session()
                .ok()
                .flatten()
                .map(|bound| bound.session_id)
        })
        .filter(|value| !value.trim().is_empty());
    let session_key = if let Some(session_id) = bound_id {
        journal.bind_session(&def.id, &session_id, &config.cwd)?;
        match runtime.load_session(&def, &config.cwd, &session_id).await {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!(error = %err, "ACP session load failed; starting a new session");
                runtime.start_session(&def, &config.cwd).await?
            }
        }
    } else {
        runtime.start_session(&def, &config.cwd).await?
    };
    journal.set_kv("session_key", &session_key)?;
    journal.bind_session(
        &def.id,
        session_key
            .split_once("::")
            .map(|(_, rest)| rest)
            .unwrap_or(&session_key),
        &config.cwd,
    )?;

    let instance_id = format!("acp_{}", Uuid::new_v4().simple());
    #[cfg(feature = "remote-codex-ui")]
    let ui_path = config.ui_dist.as_ref().map(|_| "/".to_string());
    #[cfg(not(feature = "remote-codex-ui"))]
    let ui_path = None;
    #[cfg(feature = "remote-codex-ui")]
    let (ui_events, _) = broadcast::channel(256);
    let state = Arc::new(AppState {
        agent_id: config.agent_id.clone(),
        instance_id: instance_id.clone(),
        cwd: config.cwd.clone(),
        journal,
        runtime: Arc::new(runtime),
        session_key,
        status: Mutex::new(RuntimeStatus {
            status: AgentStatus::Idle,
            busy: false,
            error: None,
        }),
        abort: Mutex::new(None),
        #[cfg(feature = "remote-codex-ui")]
        ui_events,
        ui_path: ui_path.clone(),
        #[cfg(feature = "remote-codex-ui")]
        harness_id: def.id.clone(),
        #[cfg(feature = "remote-codex-ui")]
        harness_label: def.display_name.clone(),
    });

    let router = Router::new()
        .route("/v1/manifest", get(manifest))
        .route("/v1/health", get(health))
        .route("/v1/status", get(status))
        .route("/v1/transcript", get(transcript))
        .route("/v1/prompts", post(submit_prompt))
        .route("/v1/abort", post(abort))
        .route("/v1/files/tree", get(files_tree))
        .route("/v1/files", get(files_read).put(files_write));
    #[cfg(feature = "remote-codex-ui")]
    let router = if let Some(ui_dist) = &config.ui_dist {
        router
            .merge(remote_codex::routes())
            .fallback_service(ServeDir::new(ui_dist))
    } else {
        router.merge(remote_codex::routes())
    };
    let router = router
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind AIS on {addr}"))?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .context("AIS HTTP server")
    });
    Ok(AisServer {
        port,
        instance_id,
        journal_path,
        ui_path,
        shutdown: Some(tx),
        task: Some(task),
    })
}

fn harness_runtime(config: &AisConfig) -> Result<(crate::acp::AcpAgentDef, AcpRuntime)> {
    match &config.harness {
        HarnessSpec::Fake => {
            let script = write_fake_agent(&config.state_dir)?;
            let python = which_python().ok_or_else(|| anyhow!("python3 is required for --fake"))?;
            let command = format!(r#"{python} "{}""#, script.display());
            let runtime = AcpRuntime::catalog(Some(command), config.startup_timeout_ms);
            Ok((runtime.agent_def(Some("custom"))?, runtime))
        }
        HarnessSpec::Configured {
            name,
            base_command,
            server_command,
        } => {
            let runtime = AcpRuntime::catalog(None, config.startup_timeout_ms);
            let mut def = runtime.agent_def(Some(name))?;
            def.base_command = base_command.clone();
            def.server_command = server_command.clone();
            Ok((def, runtime))
        }
    }
}

fn write_fake_agent(state_dir: &Path) -> Result<PathBuf> {
    let path = state_dir.join("fake_acp_agent.py");
    std::fs::write(&path, include_str!("../tests/fixtures/fake_acp_agent.py"))?;
    Ok(path)
}

pub fn which_python() -> Option<String> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("-c")
            .arg("import sys; sys.exit(0)")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

async fn manifest(State(state): State<Arc<AppState>>) -> Json<AgentInterfaceManifest> {
    Json(AgentInterfaceManifest {
        protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
        instance_id: state.instance_id.clone(),
        capabilities: AIS_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        ui_path: state.ui_path.clone(),
    })
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let ready = state.runtime.process_alive(&state.session_key).await;
    Json(json!({
        "instance_id": state.instance_id,
        "status": if ready { "ok" } else { "starting" },
    }))
}

async fn status(State(state): State<Arc<AppState>>) -> Json<AgentInterfaceStatusResponse> {
    let current = state.status.lock().await;
    Json(AgentInterfaceStatusResponse {
        agent_id: state.agent_id.clone(),
        interface_instance_id: state.instance_id.clone(),
        status: current.status,
        busy: current.busy,
        error: current.error.clone(),
    })
}

#[derive(Debug, Deserialize)]
struct TranscriptQuery {
    page: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

async fn transcript(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TranscriptQuery>,
) -> Result<Json<AgentTranscriptResponse>, ApiError> {
    let page = parse_u32(query.page.as_deref().or(query.cursor.as_deref())).unwrap_or(0);
    let limit = parse_u32(query.limit.as_deref())
        .unwrap_or(1)
        .clamp(1, 1000);
    let entries = state.journal.entries().map_err(ApiError::from)?;
    let page = transcript_page_from_entries(&entries, page, limit);
    Ok(Json(AgentTranscriptResponse {
        agent_id: state.agent_id.clone(),
        interface_instance_id: state.instance_id.clone(),
        page: Some(page.page),
        page_count: Some(page.page_count),
        next_page: page.next_page,
        cursor: Some(page.cursor),
        next_cursor: page.next_cursor,
        entries: page.entries,
    }))
}

#[derive(Debug, Deserialize)]
struct PromptBody {
    operation_id: Option<String>,
    text: Option<String>,
}

async fn submit_prompt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PromptBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let operation_id = body
        .operation_id
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let text = body.text.as_deref().unwrap_or("").trim().to_string();
    if operation_id.is_empty() {
        return Err(ApiError::bad("operation_id is required"));
    }
    if text.is_empty() {
        return Err(ApiError::bad("text is required"));
    }
    if state.journal.claim_operation(&operation_id)? {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "accepted": true,
                "duplicate": true,
                "operation_id": operation_id
            })),
        ));
    }
    let user = AgentTranscriptEntry {
        id: format!("{operation_id}:user"),
        kind: "message".into(),
        role: Some("user".into()),
        content: json!(text),
        created_at: Some(now_rfc3339()),
    };
    state.journal.upsert_entry(&user)?;
    {
        let mut status = state.status.lock().await;
        status.status = AgentStatus::Working;
        status.busy = true;
        status.error = None;
    }
    let cancel = Cancel::new();
    *state.abort.lock().await = Some(cancel.clone());
    let state_clone = state.clone();
    let turn_id = operation_id.clone();
    tokio::spawn(async move {
        let last_emit = std::sync::Mutex::new(
            std::time::Instant::now()
                .checked_sub(Duration::from_millis(50))
                .unwrap_or_else(std::time::Instant::now),
        );
        let result = state_clone
            .runtime
            .prompt_with_progress(
                &state_clone.session_key,
                &text,
                &turn_id,
                cancel,
                |items, usage| {
                    for item in items {
                        let _ = state_clone.journal.upsert_entry(&history_to_entry(item));
                    }
                    if let Some(usage) = usage {
                        let _ = state_clone
                            .journal
                            .set_kv(&format!("turn_usage:{turn_id}"), &usage.to_string());
                    }
                    let mut last = last_emit.lock().expect("progress throttle");
                    if last.elapsed() >= Duration::from_millis(40) {
                        *last = std::time::Instant::now();
                        drop(last);
                        #[cfg(feature = "remote-codex-ui")]
                        {
                            let state = state_clone.clone();
                            tokio::spawn(async move {
                                remote_codex::emit_state(&state).await;
                            });
                        }
                    }
                },
            )
            .await;
        match result {
            Ok(items) => {
                for item in items {
                    let _ = state_clone.journal.upsert_entry(&history_to_entry(&item));
                }
                let mut status = state_clone.status.lock().await;
                status.status = AgentStatus::Idle;
                status.busy = false;
                status.error = None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "ACP prompt failed");
                let mut status = state_clone.status.lock().await;
                status.status = AgentStatus::Blocked;
                status.busy = false;
                status.error = Some(err.to_string());
            }
        }
        *state_clone.abort.lock().await = None;
        #[cfg(feature = "remote-codex-ui")]
        remote_codex::emit_state(&state_clone).await;
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "accepted": true,
            "operation_id": operation_id
        })),
    ))
}

async fn abort(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    if let Some(cancel) = state.abort.lock().await.clone() {
        cancel.cancel();
        let _ = state.runtime.interrupt(&state.session_key).await;
    }
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true })))
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    path: Option<String>,
}

async fn files_tree(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = query.path.as_deref().unwrap_or("");
    let entries = list_tree(&state.cwd, rel).map_err(ApiError::from)?;
    Ok(Json(json!({
        "path": rel.replace('\\', "/"),
        "entries": entries,
    })))
}

async fn files_read(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = query.path.as_deref().unwrap_or("");
    if rel.is_empty() {
        return Err(ApiError::bad("path is required"));
    }
    let preview = preview_file(&state.cwd, rel, FILE_PREVIEW_LIMIT).map_err(ApiError::from)?;
    Ok(Json(serde_json::to_value(preview)?))
}

#[derive(Debug, Deserialize)]
struct WriteBody {
    content: String,
}

async fn files_write(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PathQuery>,
    Json(body): Json<WriteBody>,
) -> Result<Json<Value>, ApiError> {
    let rel = query.path.as_deref().unwrap_or("");
    if rel.is_empty() {
        return Err(ApiError::bad("path is required"));
    }
    write_file(&state.cwd, rel, &body.content).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true, "path": rel.replace('\\', "/") })))
}

fn parse_u32(value: Option<&str>) -> Option<u32> {
    value.and_then(|raw| raw.parse::<u32>().ok())
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::bad(error.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::bad(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_harness_uses_profile_commands() {
        let config = AisConfig {
            agent_id: "agent-test".into(),
            cwd: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/treer-acp-test"),
            port: 0,
            ui_dist: None,
            harness: HarnessSpec::Configured {
                name: "codex".into(),
                base_command: "/profile/bin/codex".into(),
                server_command: "/profile/bin/codex-acp --stdio".into(),
            },
            bind_session_id: None,
            startup_timeout_ms: 1,
        };

        let (def, _) = harness_runtime(&config).expect("configured harness");
        assert_eq!(def.id, "codex");
        assert_eq!(def.base_command, "/profile/bin/codex");
        assert_eq!(def.server_command, "/profile/bin/codex-acp --stdio");
    }
}
