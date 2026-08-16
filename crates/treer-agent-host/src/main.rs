use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
use treer_agent_runtime::{HostRuntime, RuntimeError};
use treer_host_protocol::{
    HostCommand, HostDaemonConfig, HostError, HostMessage, HostRequest, HostResponse, OutputCursor,
    HOST_PROTOCOL_VERSION,
};
use uuid::Uuid;

const RESULT_CACHE_LIMIT: usize = 1024;

#[derive(Debug, Parser)]
#[command(name = "treer-agent-host", about = "Stable Treer PTY and process host")]
struct Args {
    #[command(subcommand)]
    command: CommandArgs,
}

#[derive(Debug, Subcommand)]
enum CommandArgs {
    #[command(hide = true)]
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Request a hot restart of the Controller")]
    RestartController {
        #[arg(long)]
        socket: PathBuf,
    },
}

struct HostState {
    runtime: HostRuntime,
    results: Mutex<ResultCache>,
    operation_gate: Mutex<()>,
    restart_controller: mpsc::UnboundedSender<()>,
}

struct ResultCache {
    values: HashMap<String, Result<HostResponse, HostError>>,
    order: VecDeque<String>,
}

impl ResultCache {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn insert(&mut self, operation_id: String, response: Result<HostResponse, HostError>) {
        if self.values.contains_key(&operation_id) {
            return;
        }
        while self.order.len() >= RESULT_CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
        self.order.push_back(operation_id.clone());
        self.values.insert(operation_id, response);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "treer_agent_host=info".into()),
        )
        .init();
    match Args::parse().command {
        CommandArgs::Run { config } => run_daemon(load_config(&config)?).await,
        CommandArgs::RestartController { socket } => request_controller_restart(&socket).await,
    }
}

fn load_config(path: &Path) -> Result<HostDaemonConfig> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read host config {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid host config {}", path.display()))
}

async fn run_daemon(config: HostDaemonConfig) -> Result<()> {
    let runtime = HostRuntime::new(&config.root).map_err(anyhow::Error::from)?;
    prepare_socket(&config.socket_path)?;
    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("failed to bind {}", config.socket_path.display()))?;
    set_socket_permissions(&config.socket_path)?;
    let (restart_tx, mut restart_rx) = mpsc::unbounded_channel();
    let state = Arc::new(HostState {
        runtime,
        results: Mutex::new(ResultCache::new()),
        operation_gate: Mutex::new(()),
        restart_controller: restart_tx,
    });
    let server_state = Arc::clone(&state);
    let socket_path = config.socket_path.clone();
    let server = tokio::spawn(async move {
        if let Err(error) = serve(listener, server_state).await {
            warn!(%error, "host socket server stopped");
        }
    });
    info!(
        socket = %config.socket_path.display(),
        root = %config.root.display(),
        host_epoch = %state.runtime.host_epoch(),
        "treer agent host started"
    );

    let mut controller = spawn_controller(&config).await?;
    loop {
        tokio::select! {
            result = controller.wait() => {
                match result {
                    Ok(status) => warn!(%status, "controller exited; restarting"),
                    Err(error) => warn!(%error, "failed to wait for controller; restarting"),
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                controller = spawn_controller(&config).await?;
            }
            restart = restart_rx.recv() => {
                if restart.is_none() {
                    bail!("controller restart channel closed");
                }
                info!("hot restarting controller");
                if let Err(error) = controller.kill().await {
                    warn!(%error, "failed to stop old controller");
                }
                let _ = controller.wait().await;
                controller = spawn_controller(&config).await?;
            }
            signal = shutdown_signal() => {
                signal?;
                let _ = controller.kill().await;
                let _ = controller.wait().await;
                server.abort();
                remove_socket(&socket_path)?;
                return Ok(());
            }
        }
    }
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("failed to listen for SIGTERM")?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.context("failed to listen for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("failed to listen for Ctrl-C")
    }
}

async fn spawn_controller(config: &HostDaemonConfig) -> Result<Child> {
    info!(controller = %config.controller_path.display(), "starting controller");
    Command::new(&config.controller_path)
        .arg("run")
        .arg("--config")
        .arg(&config.controller_config_path)
        .spawn()
        .with_context(|| {
            format!(
                "failed to start controller {}",
                config.controller_path.display()
            )
        })
}

async fn serve(listener: UnixListener, state: Arc<HostState>) -> Result<()> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept host client")?;
        let connection_state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, connection_state).await {
                warn!(%error, "host client disconnected");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<HostState>) -> Result<()> {
    let mut output_events = state.runtime.subscribe_output();
    let mut process_events = state.runtime.subscribe_processes();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<HostMessage>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            let mut encoded = match serde_json::to_vec(&message) {
                Ok(encoded) => encoded,
                Err(error) => {
                    warn!(%error, "failed to encode host message");
                    continue;
                }
            };
            encoded.push(b'\n');
            if writer.write_all(&encoded).await.is_err() {
                break;
            }
        }
    });
    let mut synced = false;
    let mut live_after = HashMap::<String, OutputCursor>::new();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line.context("failed to read host request")? else {
                    break;
                };
                let request: HostRequest = serde_json::from_str(&line)
                    .context("failed to decode host request")?;
                if request.protocol != HOST_PROTOCOL_VERSION {
                    send_response(
                        &outgoing_tx,
                        request.request_id,
                        Err(HostError::new(
                            "protocol_mismatch",
                            format!("host protocol is {HOST_PROTOCOL_VERSION}"),
                        )),
                    );
                    continue;
                }
                if let HostCommand::Sync { cursors } = &request.command {
                    let _operation_guard = state.operation_gate.lock().await;
                    let response = sync_runtime(&state.runtime, cursors)?;
                    if let HostResponse::Synced { replay, .. } = &response {
                        live_after = replay
                            .iter()
                            .map(|replay| {
                                (
                                    replay.process_id.clone(),
                                    OutputCursor {
                                        stream_epoch: replay.stream_epoch.clone(),
                                        revision: replay.next_revision.saturating_sub(1),
                                    },
                                )
                            })
                            .collect();
                    }
                    synced = true;
                    send_response(&outgoing_tx, request.request_id, Ok(response));
                    continue;
                }
                let result = execute_request(&state, &request).await;
                send_response(&outgoing_tx, request.request_id, result);
            }
            event = output_events.recv(), if synced => match event {
                Ok(chunk) => {
                    let should_send = live_after.get(&chunk.process_id).is_none_or(|cursor| {
                        cursor.stream_epoch != chunk.stream_epoch || chunk.revision > cursor.revision
                    });
                    if should_send {
                        live_after.insert(
                            chunk.process_id.clone(),
                            OutputCursor {
                                stream_epoch: chunk.stream_epoch.clone(),
                                revision: chunk.revision,
                            },
                        );
                        let _ = outgoing_tx.send(HostMessage::Output { chunk });
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    return Err(anyhow!("host output subscriber lagged by {count} chunks"));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            event = process_events.recv(), if synced => match event {
                Ok(process) => {
                    let _ = outgoing_tx.send(HostMessage::Process { process });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    return Err(anyhow!("host process subscriber lagged by {count} events"));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    writer_task.abort();
    Ok(())
}

fn sync_runtime(
    runtime: &HostRuntime,
    cursors: &BTreeMap<String, OutputCursor>,
) -> Result<HostResponse> {
    let processes = runtime.list();
    let replay = processes
        .iter()
        .map(|process| runtime.read(&process.process_id, cursors.get(&process.process_id)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)?;
    Ok(HostResponse::Synced {
        host_epoch: runtime.host_epoch().to_string(),
        processes,
        replay,
    })
}

async fn execute_request(
    state: &HostState,
    request: &HostRequest,
) -> Result<HostResponse, HostError> {
    let _operation_guard = if request.command.is_mutating() {
        Some(state.operation_gate.lock().await)
    } else {
        None
    };
    if request.command.is_mutating() {
        let operation_id = request.operation_id.as_ref().ok_or_else(|| {
            HostError::new(
                "operation_id_required",
                "mutating commands need operation_id",
            )
        })?;
        if let Some(response) = state.results.lock().await.values.get(operation_id).cloned() {
            return response;
        }
    }
    let response: Result<HostResponse, HostError> = (|| match &request.command {
        HostCommand::Sync { .. } => unreachable!("sync is handled by the connection"),
        HostCommand::Spawn { request } => Ok(HostResponse::Process {
            process: state
                .runtime
                .spawn(request.clone())
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Read { process_id, cursor } => Ok(HostResponse::Output {
            replay: state
                .runtime
                .read(process_id, cursor.as_ref())
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Write { process_id, writes } => Ok(HostResponse::Process {
            process: state
                .runtime
                .write(process_id, writes)
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Resize {
            process_id,
            cols,
            rows,
        } => Ok(HostResponse::Process {
            process: state
                .runtime
                .resize(process_id, *cols, *rows)
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Stop { process_id } => Ok(HostResponse::Process {
            process: state.runtime.stop(process_id).map_err(host_runtime_error)?,
        }),
        HostCommand::RestartController => {
            state.restart_controller.send(()).map_err(|_| {
                HostError::new("supervisor_stopped", "controller supervisor stopped")
            })?;
            Ok(HostResponse::Ack)
        }
    })();
    if let Some(operation_id) = &request.operation_id {
        state
            .results
            .lock()
            .await
            .insert(operation_id.clone(), response.clone());
    }
    response
}

fn host_runtime_error(error: RuntimeError) -> HostError {
    HostError::new(error.code(), error.to_string())
}

fn send_response(
    outgoing: &mpsc::UnboundedSender<HostMessage>,
    request_id: String,
    result: Result<HostResponse, HostError>,
) {
    let message = match result {
        Ok(response) => HostMessage::Response {
            request_id,
            response: Some(response),
            error: None,
        },
        Err(error) => HostMessage::Response {
            request_id,
            response: None,
            error: Some(error),
        },
    };
    let _ = outgoing.send(message);
}

async fn request_controller_restart(socket: &Path) -> Result<()> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let request = HostRequest {
        protocol: HOST_PROTOCOL_VERSION,
        request_id: request_id.clone(),
        operation_id: Some(format!("restart_{}", Uuid::new_v4().simple())),
        command: HostCommand::RestartController,
    };
    let mut encoded = serde_json::to_vec(&request).context("failed to encode restart request")?;
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .context("failed to send restart request")?;
    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .context("timed out waiting for host")??
        .context("host closed without a response")?;
    let message: HostMessage = serde_json::from_str(&line).context("invalid host response")?;
    match message {
        HostMessage::Response {
            request_id: response_id,
            response: Some(HostResponse::Ack),
            error: None,
        } if response_id == request_id => {
            println!("treer: controller restart requested");
            Ok(())
        }
        HostMessage::Response {
            error: Some(error), ..
        } => bail!("{}: {}", error.code, error.message),
        _ => bail!("unexpected host response"),
    }
}

fn prepare_socket(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    remove_socket(path)
}

fn remove_socket(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_socket_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use treer_host_protocol::HostSpawnRequest;

    #[tokio::test]
    async fn repeated_operation_id_spawns_once() {
        let root = std::env::temp_dir().join(format!(
            "treer-host-idempotency-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create test root");
        let runtime = HostRuntime::new(&root).expect("create runtime");
        let (restart_controller, _) = mpsc::unbounded_channel();
        let state = HostState {
            runtime,
            results: Mutex::new(ResultCache::new()),
            operation_gate: Mutex::new(()),
            restart_controller,
        };
        let request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "req_1".to_string(),
            operation_id: Some("op_1".to_string()),
            command: HostCommand::Spawn {
                request: HostSpawnRequest {
                    process_id: "p1".to_string(),
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 5".to_string()],
                    cwd: ".".to_string(),
                    env: BTreeMap::new(),
                    cols: 80,
                    rows: 24,
                    metadata: json!({"test": true}),
                },
            },
        };
        let first = execute_request(&state, &request)
            .await
            .expect("first spawn");
        let second = execute_request(&state, &request)
            .await
            .expect("replayed spawn");
        assert_eq!(first, second);
        assert_eq!(state.runtime.list().len(), 1);
    }

    #[tokio::test]
    async fn repeated_operation_id_replays_failure() {
        let root = std::env::temp_dir().join(format!(
            "treer-host-failed-idempotency-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create test root");
        let runtime = HostRuntime::new(&root).expect("create runtime");
        let (restart_controller, _) = mpsc::unbounded_channel();
        let state = HostState {
            runtime,
            results: Mutex::new(ResultCache::new()),
            operation_gate: Mutex::new(()),
            restart_controller,
        };
        let mut request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "req_1".to_string(),
            operation_id: Some("op_1".to_string()),
            command: HostCommand::Spawn {
                request: HostSpawnRequest {
                    process_id: "p1".to_string(),
                    command: "/bin/sh".to_string(),
                    args: Vec::new(),
                    cwd: "../outside".to_string(),
                    env: BTreeMap::new(),
                    cols: 80,
                    rows: 24,
                    metadata: json!({"test": true}),
                },
            },
        };
        let first = execute_request(&state, &request)
            .await
            .expect_err("first spawn should fail");
        let HostCommand::Spawn { request: spawn } = &mut request.command else {
            unreachable!("test request is spawn");
        };
        spawn.cwd = ".".to_string();
        let second = execute_request(&state, &request)
            .await
            .expect_err("cached failure should be replayed");
        assert_eq!(first, second);
        assert!(state.runtime.list().is_empty());
    }
}
