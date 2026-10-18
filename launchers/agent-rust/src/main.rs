mod attachments;
mod files;
mod goals;
mod http;
mod image_format;
mod model;
mod rpc;
mod runtime;
mod store;
mod transcript;

use anyhow::{bail, Result};
use axum::{
    body::{to_bytes, Body},
    extract::{
        ws::{Message, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

const CONTRACT: &str = include_str!("../../agent/interface-contract.json");
const CAPABILITIES: &[&str] = &[
    "prompt.submit",
    "transcript.read",
    "state.observe",
    "abort",
    "prompt.queue",
    "prompt.notify",
    "prompt.queue.read",
];
fn treer_binary() -> String {
    if cfg!(feature = "fixture-harness") {
        "treer".into()
    } else {
        env!("SPIKE_TREER_PATH").into()
    }
}
#[derive(Clone)]
struct App {
    runtime: runtime::Handle,
    port: u16,
    agent: String,
    instance: String,
    public: PathBuf,
    files: files::Files,
    attachments: Option<Arc<tokio::sync::Mutex<attachments::Attachments>>>,
    image_slots: Arc<tokio::sync::Semaphore>,
}
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    if !cfg!(target_os = "macos") && !cfg!(target_os = "linux") {
        bail!("POSIX experiment only")
    }
    let configured_cwd = match env("AGENT_UI_CWD") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()
            .map_err(|_| anyhow::anyhow!("workspace_file_root_unavailable"))?,
    };
    let cwd = std::fs::canonicalize(configured_cwd)
        .map_err(|_| anyhow::anyhow!("workspace_file_root_unavailable"))?;
    let files = files::Files::new(cwd.clone())
        .map_err(|_| anyhow::anyhow!("workspace_file_root_unavailable"))?;
    let agent = env("TREER_AGENT_ID").unwrap_or_default();
    if agent.len() > 128
        || !agent
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        bail!("invalid agent id")
    }
    if !cfg!(feature = "fixture-harness")
        && (env("CODEX_BIN").is_some() || env("RUST_SPIKE_CODEX_ARGS").is_some())
    {
        bail!("harness overrides require the fixture-harness test feature")
    }
    let private_root = PathBuf::from(
        env(if agent.is_empty() {
            "TREER_RUST_SPIKE_STATE_DIR"
        } else {
            "TREER_AGENT_PRIVATE_STATE_DIR"
        })
        .ok_or_else(|| anyhow::anyhow!("explicit private state root required"))?,
    );
    if !agent.is_empty() {
        private_status(&agent).await?;
    }
    let _state_lease = store::Lease::acquire(&private_root, &cwd, &agent)?;
    let attachments = if !agent.is_empty() {
        Some(Arc::new(tokio::sync::Mutex::new(
            attachments::Attachments::open(
                &private_root,
                &cwd,
                agent.clone(),
                &env("TREER_AGENT_SERVER_URL").unwrap_or_default(),
                env("TREER_WORKLOAD_CREDENTIAL").unwrap_or_default(),
            )
            .map_err(|_| anyhow::anyhow!("attachment_storage_unavailable"))?,
        )))
    } else {
        None
    };
    let public = std::fs::canonicalize(
        env("AGENT_UI_PUBLIC_DIR")
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../agent/public")),
    )?;
    let listener = TcpListener::bind((
        "127.0.0.1",
        env("AGENT_UI_PORT").unwrap_or("0".into()).parse::<u16>()?,
    ))
    .await?;
    let port = listener.local_addr()?.port();
    let instance =
        env("AGENT_UI_INSTANCE_ID").unwrap_or(format!("rust-spike-{}", uuid::Uuid::new_v4()));
    let codex_args = if let Some(args) = env("RUST_SPIKE_CODEX_ARGS") {
        serde_json::from_str::<Vec<String>>(&args)?
    } else {
        vec!["app-server".into(), "--listen".into(), "stdio://".into()]
    };
    let handle = runtime::spawn(runtime::Config {
        cwd,
        private_root,
        codex: if cfg!(feature = "fixture-harness") {
            env("CODEX_BIN").unwrap_or(env!("SPIKE_CODEX_PATH").into())
        } else {
            env!("SPIKE_CODEX_PATH").into()
        },
        codex_args,
        session: env("AGENT_UI_SESSION_ID"),
        attachments: attachments.clone(),
        capture_stderr: env("RUST_SPIKE_CAPTURE_STDERR").as_deref() == Some("1"),
    })?;
    if let Some(file) = env("AGENT_UI_PORT_FILE") {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(file)?
            .write_all(port.to_string().as_bytes())?;
    }
    let app = App {
        runtime: handle.clone(),
        port,
        agent: agent.clone(),
        instance: instance.clone(),
        public,
        files,
        attachments,
        image_slots: Arc::new(tokio::sync::Semaphore::new(2)),
    };
    let files_idle = app.files.idle;
    let router = Router::new().fallback(any(route)).with_state(app);
    println!("Rust Agent spike listening on http://127.0.0.1:{port}");
    let should_register = !agent.is_empty() && env("RUST_SPIKE_REGISTER").as_deref() == Some("1");
    // Controller registration probes the manifest. Poll the HTTP server while
    // registration runs, otherwise that probe deadlocks against our own startup.
    let register = async move {
        if should_register {
            let mut args = vec![
                "interface".into(),
                "register".into(),
                "--port".into(),
                port.to_string(),
                "--instance-id".into(),
                instance,
                "--ui-path".into(),
                "/".into(),
            ];
            for capability in CAPABILITIES {
                args.extend(["--capability".into(), (*capability).into()]);
            }
            let mut registration = tokio::process::Command::new(treer_binary())
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let status =
                tokio::time::timeout(std::time::Duration::from_secs(10), registration.wait())
                    .await??;
            if !status.success() {
                bail!("interface registration failed")
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let shutdown_handle = handle.clone();
    let final_shutdown = handle.clone();
    if !agent.is_empty()
        && env("AGENT_UI_MESSAGE_WATCH").as_deref() != Some("0")
        && env("TREER_WORKLOAD_CREDENTIAL").is_some()
    {
        tokio::spawn(runtime::watch_messages(handle, agent));
    }
    let draining = Arc::new(tokio::sync::Notify::new());
    let drain_started = draining.clone();
    let serve = async {
        http::serve(listener, router, files_idle, async move {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap();
                let mut hup =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{},_=hup.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            if shutdown_handle.shutdown().await.is_err() {
                eprintln!("Rust spike: runtime_shutdown_unverified");
            }
            drain_started.notify_one();
        })
        .await?;
        Ok::<_, anyhow::Error>(())
    };
    // A WebSocket or a client that keeps a response open cannot prevent exit.
    // Provider cleanup happens before the bounded HTTP drain begins.
    let result = tokio::select! {
        result = async { tokio::try_join!(register, serve).map(|_| ()) } => result,
        _ = async { draining.notified().await; tokio::time::sleep(std::time::Duration::from_secs(2)).await; } => Ok(()),
    };
    final_shutdown.shutdown().await?;
    result?;
    Ok(())
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}
fn guard(h: &HeaderMap, app: &App) -> bool {
    if [
        "host",
        "origin",
        "x-treer-interface-source",
        "x-treer-agent-id",
        "x-treer-interface-instance",
    ]
    .iter()
    .any(|name| h.get_all(*name).iter().count() > 1)
    {
        return false;
    }
    let source = header(h, "x-treer-interface-source");
    let marked = source.is_some()
        || h.contains_key("x-treer-agent-id")
        || h.contains_key("x-treer-interface-instance");
    let valid = !app.agent.is_empty()
        && matches!(source, Some("proxy-ui" | "controller-api"))
        && header(h, "x-treer-agent-id") == Some(app.agent.as_str())
        && header(h, "x-treer-interface-instance") == Some(app.instance.as_str());
    if marked && !valid {
        return false;
    }
    let host = header(h, "host").unwrap_or("").to_lowercase();
    let local_host = [
        format!("127.0.0.1:{}", app.port),
        format!("localhost:{}", app.port),
        format!("[::1]:{}", app.port),
    ]
    .contains(&host);
    if !(local_host || valid && source == Some("controller-api") && host == "agent-interface") {
        return false;
    }
    if source == Some("controller-api") && h.contains_key("origin") {
        return false;
    }
    let browser =
        h.contains_key("origin") || h.keys().any(|k| k.as_str().starts_with("sec-fetch-"));
    !browser || valid
}
fn json_response(status: StatusCode, value: Value) -> Response {
    (status, [("cache-control", "no-store")], axum::Json(value)).into_response()
}
fn failure(status: StatusCode, code: &str) -> Response {
    json_response(status, json!({"error":{"code":code,"message":code}}))
}
fn limited(snapshot: &Value, limit: usize) -> Value {
    let mut value = snapshot.clone();
    if let Some(turns) = value["thread"]["turns"].as_array_mut() {
        let mut left = limit;
        let mut hidden = false;
        for turn in turns.iter_mut().rev() {
            if let Some(items) = turn["items"].as_array_mut() {
                if items.len() > left {
                    items.drain(..items.len() - left);
                    hidden = true;
                }
                left = left.saturating_sub(items.len());
            }
        }
        turns.retain(|t| {
            t["items"].as_array().is_some_and(|a| !a.is_empty()) || t["status"] == "inProgress"
        });
        value["thread"]["hasOlderItems"] = json!(hidden);
    }
    value["history"] = json!({"limit":limit});
    value
}
async fn route(State(app): State<App>, request: Request<Body>) -> Response {
    if !guard(request.headers(), &app) {
        return failure(StatusCode::FORBIDDEN, "invalid_interface_provenance");
    }
    let path = request.uri().path().trim_end_matches('/').to_string();
    let path = if path.is_empty() { "/".into() } else { path };
    let method = request.method().to_string();
    if path == "/api/attachments" || path.starts_with("/api/attachments/") {
        return attachment_route(&app, request, &path, &method).await;
    }
    if path == "/v1/files" || path == "/v1/files/tree" || path == "/v1/files/download" {
        return app
            .files
            .handle(
                &method,
                request.uri().query().unwrap_or(""),
                request.headers(),
                &path,
            )
            .await;
    }
    let queue_route = path == "/v1/prompts"
        || path == "/v1/queue"
        || path.starts_with("/v1/queue/")
        || path.starts_with("/v1/submissions/");
    if queue_route
        && !app.agent.is_empty()
        && header(request.headers(), "x-treer-interface-source") != Some("proxy-ui")
        && header(request.headers(), "x-treer-interface-source") != Some("controller-api")
    {
        return failure(StatusCode::FORBIDDEN, "controller_provenance_required");
    }
    if path == "/ws" {
        let (mut parts, _) = request.into_parts();
        use axum::extract::FromRequestParts;
        let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            return failure(StatusCode::BAD_REQUEST, "invalid_websocket");
        };
        let mut states = app.runtime.state.clone();
        return upgrade.max_message_size(16*1024).on_upgrade(move |mut socket|async move {
            let mut limit=50;
            loop {
                let payload={let state=states.borrow_and_update(); let mut value=limited(&state,limit);value["type"]=json!("state");value.to_string()};
                if !matches!(tokio::time::timeout(std::time::Duration::from_secs(5),socket.send(Message::Text(payload.into()))).await,Ok(Ok(()))){break}
                tokio::select! {
                    result=states.changed()=>if result.is_err(){break},
                    message=socket.recv()=>match message {
                        Some(Ok(Message::Text(text)))=>{if let Ok(value)=serde_json::from_str::<Value>(&text){if value["type"]=="history-limit" {limit=value["limit"].as_u64().unwrap_or(50).clamp(50,10000) as usize;}}},
                        Some(Ok(Message::Ping(data)))=>{let _=socket.send(Message::Pong(data)).await;},
                        Some(Err(error))=>{
                            let size_error=error.to_string().contains("Space limit exceeded");
                            let _=socket.send(Message::Close(Some(axum::extract::ws::CloseFrame{code:if size_error{1009}else{1002},reason:"Invalid WebSocket frame".into()}))).await;
                            break;
                        },
                        Some(Ok(Message::Close(_)))|None=>break,
                        _=>{},
                    }
                }
            }
        }).into_response();
    }
    let snapshot: Arc<Value> = app.runtime.state.borrow().clone();
    if method == "GET" || method == "HEAD" {
        let data = match path.as_str() {
            "/v1/manifest" => {
                let c: Value = serde_json::from_str(CONTRACT).unwrap();
                json!({"protocol":c["protocol"],"instance_id":app.instance,"capabilities":CAPABILITIES,"ui_path":"/"})
            }
            "/v1/health" => {
                return json_response(
                    if snapshot["ready"] == true {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    },
                    json!({"instance_id":app.instance,"status":if snapshot["ready"]==true{"ok"}else{"starting"}}),
                )
            }
            "/api/health" => json!({"ok":snapshot["ready"],"ready":snapshot["ready"]}),
            "/.treer/agent" => {
                json!({"protocol":"treer.agent.surface","version":1,"ready":snapshot["ready"],"title":"Codex (Rust experiment)","ui":true,"capabilities":CAPABILITIES})
            }
            "/api/state" | "/v1/state" => limited(&snapshot, 50),
            "/v1/status" => {
                json!({"agent_id":app.agent,"interface_instance_id":app.instance,"status":snapshot["runtime"]["state"],"busy":snapshot["runtime"]["state"]=="working","queue_length":snapshot["queue"].as_array().map_or(0,Vec::len),"error":snapshot["runtime"]["error"]})
            }
            "/v1/transcript" => transcript(&app, &snapshot, request.uri().query().unwrap_or("")),
            "/v1/queue" => return call(&app, "queue", Value::Null, StatusCode::OK).await,
            p if p.starts_with("/v1/submissions/") => {
                let Some(id) = submission_segment(p.trim_start_matches("/v1/submissions/")) else {
                    return failure(StatusCode::BAD_REQUEST, "invalid_submission_id");
                };
                return call(&app, "submission", json!({"id":id}), StatusCode::OK).await;
            }
            _ => {
                // Like the Node launcher, an unknown GET path serves the UI shell so deep links and stray URLs load it.
                let response = static_file(&app, &path).await;
                return if response.status() == StatusCode::NOT_FOUND {
                    static_file(&app, "/").await
                } else {
                    response
                };
            }
        };
        return json_response(StatusCode::OK, data);
    }
    if method == "DELETE" && (path.starts_with("/v1/queue/") || path.starts_with("/api/queue/")) {
        let Some(id) = submission_segment(path.rsplit('/').next().unwrap_or("")) else {
            return failure(StatusCode::BAD_REQUEST, "invalid_submission_id");
        };
        if path.starts_with("/api/queue/") {
            return match app.runtime.call("cancel", json!({"id":id})).await {
                Ok(value) if !value.is_null() && value["error"].is_null() => {
                    json_response(StatusCode::OK, limited(&app.runtime.state.borrow(), 50))
                }
                _ => json_response(
                    StatusCode::NOT_FOUND,
                    json!({"error":"Queued prompt not found"}),
                ),
            };
        }
        return call(&app, "cancel", json!({"id":id}), StatusCode::OK).await;
    }
    if method != "POST" && !(path == "/api/goal" && (method == "PUT" || method == "DELETE")) {
        return failure(StatusCode::METHOD_NOT_ALLOWED, "unsupported_operation");
    }
    let attested = header(request.headers(), "x-treer-interface-source") == Some("controller-api");
    let Ok(Ok(bytes)) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        to_bytes(request.into_body(), 128 * 1024),
    )
    .await
    else {
        return failure(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    };
    let mut body = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => v,
            _ => return failure(StatusCode::BAD_REQUEST, "invalid_json"),
        }
    };
    match path.as_str() {
        "/api/goal" if method == "PUT" || method == "DELETE" => {
            let limit = history_limit(&body);
            body["clear"] = json!(method == "DELETE");
            ui_call(&app, "goal", body, limit).await
        }
        "/api/goal/reconcile" => {
            let limit = history_limit(&body);
            ui_call(&app, "goal_reconcile", body, limit).await
        }
        "/api/goal/retry" => {
            let limit = history_limit(&body);
            ui_call(&app, "goal_retry", body, limit).await
        }
        "/v1/prompts" | "/api/prompt" => {
            if !app.agent.is_empty() && private_status(&app.agent).await.is_err() {
                return failure(StatusCode::SERVICE_UNAVAILABLE, "private_state_unavailable");
            }
            if path == "/api/prompt" {
                // Legacy UI prompts never accept caller-supplied attestation,
                // including when forwarded by the Controller (Node parity).
                body["sender"] = Value::Null;
                body["notify"] = Value::Null;
            }
            if !attested && (!body["sender"].is_null() || !body["notify"].is_null()) {
                return failure(StatusCode::FORBIDDEN, "attested_sender_required");
            }
            if path == "/api/prompt" {
                body["text"] = body["prompt"].clone();
                body["delivery"] = body["mode"]
                    .as_str()
                    .map(|s| json!(s))
                    .unwrap_or(json!("steer"));
                body["operation_id"] = json!(uuid::Uuid::new_v4().to_string());
            }
            let ui_limit = history_limit(&body);
            if path == "/api/prompt" {
                return ui_call(&app, "submit", body, ui_limit).await;
            }
            call(
                &app,
                "submit",
                body,
                if path == "/v1/prompts" {
                    StatusCode::ACCEPTED
                } else {
                    StatusCode::OK
                },
            )
            .await
        }
        "/v1/abort" => call(&app, "abort", body, StatusCode::ACCEPTED).await,
        "/api/interrupt" => {
            let limit = history_limit(&body);
            ui_call(&app, "abort", body, limit).await
        }
        "/api/settings" => {
            let limit = history_limit(&body);
            ui_call(&app, "settings", body, limit).await
        }
        "/api/history/older" => json_response(
            StatusCode::OK,
            limited(
                &snapshot,
                (body["limit"].as_u64().unwrap_or(50) as usize + 50).min(10000),
            ),
        ),
        _ => failure(StatusCode::NOT_IMPLEMENTED, "outside_rust_experiment_scope"),
    }
}
fn history_limit(body: &Value) -> usize {
    body["history_limit"]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.floor().clamp(50.0, 10000.0) as usize)
        .unwrap_or(50)
}

async fn attachment_route(app: &App, request: Request<Body>, path: &str, method: &str) -> Response {
    let Some(store) = &app.attachments else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"attachment_private_root_required"}),
        );
    };
    let Ok(permit) = app.image_slots.clone().try_acquire_owned() else {
        return json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error":"attachment_busy"}),
        );
    };
    let session = app.runtime.state.borrow()["thread"]["id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if session.is_empty() {
        return json_response(
            StatusCode::CONFLICT,
            json!({"error":"attachment_session_required"}),
        );
    }
    if private_status(&app.agent).await.is_err() {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"agent_private_state_unavailable"}),
        );
    }
    if method == "POST" && path == "/api/attachments" {
        let bytes = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            to_bytes(request.into_body(), image_format::FILE_LIMIT),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            _ => {
                return json_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    json!({"error":"attachment_too_large"}),
                )
            }
        };
        return match store.lock().await.upload(&session, &bytes).await {
            Ok(metadata) => json_response(StatusCode::CREATED, json!({"attachment":metadata})),
            Err(e) => attachment_error(&e),
        };
    }
    let id = path.trim_start_matches("/api/attachments/");
    if method == "GET" {
        return match store.lock().await.read(id, &session) {
            Ok((metadata, bytes)) => {
                let length = bytes.len();
                // Keep the transfer slot while the response is backpressured;
                // never create an unbounded queue of complete image buffers.
                let chunks = futures_util::stream::unfold(
                    (bytes, 0, permit),
                    |(bytes, offset, permit)| async move {
                        if offset == bytes.len() {
                            return None;
                        }
                        let end = (offset + 64 * 1024).min(bytes.len());
                        let chunk = bytes[offset..end].to_vec();
                        Some((Ok::<_, std::io::Error>(chunk), (bytes, end, permit)))
                    },
                );
                let chunks = futures_util::StreamExt::take_until(chunks, tokio::time::sleep(app.files.max_duration()));
                Response::builder()
                    .status(200)
                    .header(
                        "content-type",
                        metadata["mime"]
                            .as_str()
                            .unwrap_or("application/octet-stream"),
                    )
                    .header("content-length", length)
                    .header("x-content-type-options", "nosniff")
                    .header("content-security-policy", "default-src 'none'; sandbox")
                    .header("cache-control", "private, no-store")
                    .body(Body::from_stream(chunks))
                    .unwrap()
            }
            Err(e) => attachment_error(&e),
        };
    }
    if method == "DELETE" {
        return match store.lock().await.discard(id, &session) {
            Ok(()) => json_response(StatusCode::OK, json!({"removed":true})),
            Err(e) => attachment_error(&e),
        };
    }
    json_response(
        StatusCode::METHOD_NOT_ALLOWED,
        json!({"error":"method_not_allowed"}),
    )
}
fn attachment_error(error: &anyhow::Error) -> Response {
    let value = error.to_string();
    let (status, code) = match value.as_str() {
        "attachment_empty" | "attachment_invalid_image" | "attachment_invalid_ids" => {
            (StatusCode::BAD_REQUEST, value.as_str())
        }
        "attachment_too_large" | "attachment_prompt_too_large" | "attachment_quota_exceeded" => {
            (StatusCode::PAYLOAD_TOO_LARGE, value.as_str())
        }
        "attachment_not_found" => (StatusCode::NOT_FOUND, value.as_str()),
        "attachment_already_sent" => (StatusCode::CONFLICT, value.as_str()),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "attachment_storage_unavailable",
        ),
    };
    json_response(status, json!({"error":code}))
}

// Legacy browser mutations return a state snapshot, whereas AIS mutations keep
// their receipt contract. Read the watch state only after the actor replies so
// settings and queue changes are already published.
async fn ui_call(app: &App, method: &str, body: Value, limit: usize) -> Response {
    match app.runtime.call(method, body).await {
        Ok(result) => {
            let mut snapshot = limited(&app.runtime.state.borrow(), limit);
            if method == "submit" {
                snapshot["submission"] = result["submission"].clone();
            }
            json_response(StatusCode::OK, snapshot)
        }
        Err(_) => failure(StatusCode::BAD_REQUEST, "operation_refused"),
    }
}
async fn call(app: &App, method: &str, body: Value, status: StatusCode) -> Response {
    match app.runtime.call(method, body).await {
        Ok(value) if value["error"]["code"] == "submission_not_queued" => {
            json_response(StatusCode::CONFLICT, value)
        }
        Ok(value) if value.is_null() => failure(StatusCode::NOT_FOUND, "submission_not_found"),
        Ok(value) => json_response(status, value),
        Err(_) => failure(StatusCode::BAD_REQUEST, "operation_refused"),
    }
}
fn transcript(app: &App, snapshot: &Value, query: &str) -> Value {
    let value = |name: &str, default: usize| {
        query
            .split('&')
            .find_map(|s| {
                s.split_once('=')
                    .filter(|(k, _)| *k == name)
                    .and_then(|(_, v)| v.parse::<usize>().ok())
            })
            .unwrap_or(default)
    };
    let page = value("page", value("cursor", 0));
    let limit = value("limit", 1).clamp(1, 1000);
    let empty = vec![];
    let turns = snapshot["thread"]["turns"].as_array().unwrap_or(&empty);
    let entries=turns.iter().skip(page).take(limit).flat_map(|t|t["items"].as_array().into_iter().flatten().map(|i|json!({"id":i["id"],"kind":i["kind"],"role":if i["kind"]=="userMessage"{json!("user")}else if i["kind"]=="agentMessage"{json!("assistant")}else{Value::Null},"content":i["text"],"created_at":t["startedAt"]}))).collect::<Vec<_>>();
    let next = if page.saturating_add(limit) < turns.len() {
        Some(page + limit)
    } else {
        None
    };
    json!({"agent_id":app.agent,"interface_instance_id":app.instance,"page":page,"page_count":turns.len(),"next_page":next,"cursor":page.to_string(),"next_cursor":next.map(|n|n.to_string()),"entries":entries})
}
async fn static_file(app: &App, path: &str) -> Response {
    if path.split('/').any(|p| p == "..") || path.contains('%') || path.contains('\\') {
        return failure(StatusCode::FORBIDDEN, "invalid_asset_path");
    }
    let candidate = app.public.join(if path == "/" {
        "index.html"
    } else {
        path.trim_start_matches('/')
    });
    for ancestor in candidate
        .ancestors()
        .take_while(|p| p.starts_with(&app.public))
    {
        if tokio::fs::symlink_metadata(ancestor)
            .await
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return failure(StatusCode::FORBIDDEN, "linked_asset");
        }
    }
    let Ok(file) = tokio::fs::canonicalize(&candidate).await else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    if !file.starts_with(&app.public) {
        return failure(StatusCode::FORBIDDEN, "invalid_asset_path");
    }
    if !tokio::fs::metadata(&file)
        .await
        .is_ok_and(|m| m.is_file() && m.len() <= 8 * 1024 * 1024)
    {
        return failure(StatusCode::NOT_FOUND, "not_found");
    }
    let Ok(data) = tokio::fs::read(&file).await else {
        return failure(StatusCode::NOT_FOUND, "not_found");
    };
    let mime = match file.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let mut response = (
        StatusCode::OK,
        [
            ("content-type", mime),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
        ],
        data,
    )
        .into_response();
    if mime.starts_with("text/html") {
        response.headers_mut().insert("content-security-policy", "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self'; script-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'none'".parse().unwrap());
        response.headers_mut().insert("permissions-policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=(), display-capture=(), clipboard-read=(), bluetooth=(), hid=(), serial=(), midi=(), accelerometer=(), gyroscope=(), magnetometer=(), screen-wake-lock=()".parse().unwrap());
    }
    response
}

// Controller percent-encodes non-alphanumeric bytes, including UUID hyphens.
// Decode only this opaque segment once, never the complete route/static path.
fn submission_segment(encoded: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    if decoded.is_empty()
        || decoded.len() > 128
        || !decoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some(decoded.into_owned())
}

async fn private_status(agent: &str) -> Result<()> {
    let url = env("TREER_AGENT_SERVER_URL")
        .ok_or_else(|| anyhow::anyhow!("private state unavailable"))?;
    let base = reqwest::Url::parse(&url)?;
    if base.scheme() != "http"
        || !matches!(base.host_str(), Some("127.0.0.1" | "[::1]"))
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        bail!("private state unavailable")
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let mut response = client
        .get(base.join("api/private-state")?)
        .header("x-treer-agent-id", agent)
        .header(
            "x-treer-workload-credential",
            env("TREER_WORKLOAD_CREDENTIAL")
                .ok_or_else(|| anyhow::anyhow!("private state unavailable"))?,
        )
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("private state unavailable")
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > 65536 - body.len() {
            bail!("private state unavailable")
        }
        body.extend_from_slice(&chunk);
    }
    let data: Value = serde_json::from_slice(&body)?;
    if data["feature"] != "agent.private-state.v1" || data["status"] != "available" {
        bail!("private state unavailable")
    }
    Ok(())
}
