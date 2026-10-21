use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tracing::{info, warn};
use treer_agent_runtime::{HostRuntime, RuntimeError};
use treer_host_protocol::{
    decode_message, encode_message, HostBuildInfo, HostCommand, HostDaemonConfig, HostError,
    HostMessage, HostRequest, HostResponse, OutputCursor, HOST_PROTOCOL_VERSION,
    MAX_HOST_FRAME_BYTES,
};
use treer_platform::{connect_local, LocalListener, LocalStream};
use uuid::Uuid;

const RESULT_CACHE_LIMIT: usize = 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);

fn operation_diagnostic(
    kind: &str,
    request_id: &str,
    phase: &str,
    elapsed_ms: u64,
    error_code: Option<&str>,
) {
    static RATE: std::sync::Mutex<Option<(std::time::Instant, u32)>> = std::sync::Mutex::new(None);
    let Ok(mut window) = RATE.try_lock() else {
        return;
    };
    let (since, count) = window.get_or_insert((std::time::Instant::now(), 0));
    if since.elapsed() >= Duration::from_secs(1) {
        *since = std::time::Instant::now();
        *count = 0;
    }
    if *count >= 20 {
        return;
    }
    *count += 1;
    drop(window);
    info!(
        kind,
        request_id, phase, elapsed_ms, error_code, "host operation"
    );
}

#[derive(Debug, Parser)]
#[command(
    name = "treer-agent-host",
    about = "Stable Treer PTY and process host",
    version = treer_build_info::DISPLAY
)]
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

#[derive(Clone)]
struct HostState {
    runtime: HostRuntime,
    results: Arc<Mutex<ResultCache>>,
    operations: Arc<Semaphore>,
    controls: Arc<Semaphore>,
    restart_controller: mpsc::Sender<()>,
}

struct ResultCache {
    values: HashMap<String, Result<HostResponse, HostError>>,
    order: VecDeque<String>,
    bindings: HashMap<String, [u8; 32]>,
}

impl ResultCache {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, operation_id: String, response: Result<HostResponse, HostError>) {
        if self.values.contains_key(&operation_id) {
            return;
        }
        while self.order.len() >= RESULT_CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
                self.bindings.remove(&oldest);
            }
        }
        // Unknown effects are tombstones for this Host epoch, not replayable
        // work. Never make a timed-out mutation runnable by cache eviction.
        if !response.as_ref().err().is_some_and(|error| {
            matches!(error.code.as_str(), "input_unknown" | "operation_unknown")
        }) {
            self.order.push_back(operation_id.clone());
        }
        self.values.insert(operation_id, response);
    }
}

fn main() -> Result<()> {
    treer_platform::clean_process_environment_before_threads()?;
    async_main()
}

#[tokio::main]
async fn async_main() -> Result<()> {
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
    #[cfg(windows)]
    let _process_tree = treer_platform::ProcessTreeGuard::new()
        .context("failed to establish Host process ownership")?;
    let runtime = HostRuntime::new(&config.root).map_err(anyhow::Error::from)?;
    prepare_socket(&config.socket_path)?;
    let listener = LocalListener::bind(&config.socket_path)
        .with_context(|| format!("failed to bind {}", config.socket_path.display()))?;
    set_socket_permissions(&config.socket_path)?;
    let (restart_tx, mut restart_rx) = mpsc::channel(1);
    let state = Arc::new(HostState {
        runtime,
        results: Arc::new(Mutex::new(ResultCache::new())),
        operations: Arc::new(Semaphore::new(16)),
        controls: Arc::new(Semaphore::new(8)),
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
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
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
            signal = &mut shutdown => {
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
    let mut command = Command::new(&config.controller_path);
    command
        .env_clear()
        .envs(treer_platform::inherited_environment());
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW; Agent terminals use ConPTY.
    command
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

async fn serve(mut listener: LocalListener, state: Arc<HostState>) -> Result<()> {
    let connections = Arc::new(Semaphore::new(32));
    loop {
        let stream = listener
            .accept()
            .await
            .context("failed to accept host client")?;
        let connection_state = Arc::clone(&state);
        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
            continue;
        };
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_connection(stream, connection_state).await {
                warn!(%error, "host client disconnected");
            }
        });
    }
}

async fn handle_connection(stream: LocalStream, state: Arc<HostState>) -> Result<()> {
    struct AbortWriter(tokio::task::AbortHandle);
    impl Drop for AbortWriter {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut output_events = state.runtime.subscribe_output();
    let mut process_events = state.runtime.subscribe_processes();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<HostMessage>(8);
    let requests = Arc::new(Semaphore::new(2));
    let control_requests = Arc::new(Semaphore::new(2));
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if let Err(error) = write_frame(&mut writer, &message).await {
                warn!(%error, "failed to write host message");
                break;
            }
        }
    });
    // All exits, including queue overflow and malformed frames, must release
    // the socket writer instead of leaving a detached slow-reader task alive.
    let _writer_guard = AbortWriter(writer_task.abort_handle());
    let mut synced = false;
    let mut live_after = HashMap::<String, OutputCursor>::new();

    'frames: loop {
        // read_u32/read_exact consume bytes before returning Pending. Keep the
        // same future across event branches so an event cannot discard a
        // partially read frame and reinterpret its payload as another request.
        let request = read_frame::<_, HostRequest>(&mut reader);
        tokio::pin!(request);
        loop {
            tokio::select! {
                request = &mut request => {
                    let Some(request) = request.context("failed to read host request")? else {
                        break 'frames;
                    };
                    if request.protocol != HOST_PROTOCOL_VERSION {
                        send_response(
                            &outgoing_tx,
                            request.request_id,
                            Err(HostError::new(
                                "protocol_mismatch",
                                format!("host protocol is {HOST_PROTOCOL_VERSION}"),
                            )),
                        );
                        bail!("host protocol mismatch");
                    }
                    if let HostCommand::Sync { .. } = &request.command {
                        let response = execute_request(&state, &request).await;
                        let response = match response {
                            Ok(response) => response,
                            Err(error) => {
                                send_response(&outgoing_tx, request.request_id, Err(error));
                                continue 'frames;
                            }
                        };
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
                        continue 'frames;
                    }
                    let lane = match request.command {
                        HostCommand::Stop { .. } | HostCommand::Read { .. } | HostCommand::RestartController => &control_requests,
                        _ => &requests,
                    };
                    let Ok(permit) = Arc::clone(lane).try_acquire_owned() else {
                        send_response(&outgoing_tx, request.request_id, Err(HostError::new("host_busy", "connection request limit; not admitted")));
                        continue 'frames;
                    };
                    let response_state = Arc::clone(&state);
                    let responses = outgoing_tx.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let result = execute_request(&response_state, &request).await;
                        send_response(&responses, request.request_id, result);
                    });
                    continue 'frames;
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
                            outgoing_tx.try_send(HostMessage::Output { chunk }).map_err(|_| anyhow!("host output queue full or closed"))?;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        return Err(anyhow!("host output subscriber lagged by {count} chunks"));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break 'frames,
                },
                event = process_events.recv(), if synced => match event {
                    Ok(process) => {
                        outgoing_tx.try_send(HostMessage::Process { process }).map_err(|_| anyhow!("host process queue full or closed"))?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        return Err(anyhow!("host process subscriber lagged by {count} events"));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break 'frames,
                }
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
        host_build: HostBuildInfo {
            version: treer_build_info::VERSION.to_string(),
            git_commit: treer_build_info::GIT_COMMIT.to_string(),
        },
        processes,
        replay,
    })
}

async fn execute_request(
    state: &HostState,
    request: &HostRequest,
) -> Result<HostResponse, HostError> {
    if request.request_id.len() > 128
        || request
            .operation_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128)
    {
        return Err(HostError::new(
            "invalid_request",
            "request or operation id exceeds limit",
        ));
    }
    // Admission is nonwaiting. Keep a separate control lane so stuck spawn,
    // write or resize calls cannot consume Sync/Read/Stop/restart capacity.
    let lane = match request.command {
        HostCommand::Spawn { .. } | HostCommand::Write { .. } | HostCommand::Resize { .. } => {
            &state.operations
        }
        _ => &state.controls,
    };
    let admit = || {
        Arc::clone(lane)
            .try_acquire_owned()
            .map_err(|_| HostError::new("host_busy", "host capacity busy; request not admitted"))
    };
    let permit;
    let mutating = request.command.is_mutating();
    if mutating {
        let operation_id = request.operation_id.as_ref().ok_or_else(|| {
            HostError::new(
                "operation_id_required",
                "mutating commands need operation_id",
            )
        })?;
        let fingerprint: [u8; 32] = Sha256::digest(
            encode_message(&request.command)
                .map_err(|_| HostError::new("invalid_request", "could not encode command"))?,
        )
        .into();
        let mut cache = state.results.lock().await;
        if let Some(binding) = cache.bindings.get(operation_id) {
            if binding != &fingerprint {
                return Err(HostError::new(
                    "operation_conflict",
                    "operation id belongs to another command",
                ));
            }
            if let Some(response) = cache.values.get(operation_id).cloned() {
                return response;
            }
            return Err(operation_unknown());
        }
        permit = admit()?;
        // In-flight entries are never evicted. Completed entries retain the
        // existing bounded replay cache contract, and cannot hide pending work.
        if cache.bindings.len() >= RESULT_CACHE_LIMIT {
            if let Some(oldest) = cache.order.pop_front() {
                cache.values.remove(&oldest);
                cache.bindings.remove(&oldest);
            } else {
                return Err(HostError::new(
                    "host_busy",
                    "operation ledger full; request not admitted",
                ));
            }
        }
        cache.bindings.insert(operation_id.clone(), fingerprint);
    } else {
        permit = admit()?;
    }
    let state = state.clone();
    let request = request.clone();
    let kind = match request.command {
        HostCommand::Sync { .. } => "sync",
        HostCommand::Spawn { .. } => "spawn",
        HostCommand::Read { .. } => "read",
        HostCommand::Write { .. } => "write",
        HostCommand::Resize { .. } => "resize",
        HostCommand::Stop { .. } => "stop",
        HostCommand::RestartController => "restart_controller",
    };
    let diagnostic_id = request.request_id.clone();
    // The detached task owns admission and the operation ledger through actual
    // completion, even when the socket disconnects or the response times out.
    let task = tokio::spawn(async move {
        let started = std::time::Instant::now();
        operation_diagnostic(kind, &request.request_id, "admitted", 0, None);
        let runtime = state.runtime.clone();
        let restart = state.restart_controller.clone();
        let command = request.command;
        let response = if let HostCommand::Write { process_id, writes } = command {
            let _permit = permit;
            runtime
                .write_async(&process_id, &writes)
                .await
                .map(|process| HostResponse::Process { process })
                .map_err(host_runtime_error)
        } else {
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                execute_runtime(&runtime, &restart, &command)
            })
            .await
            .unwrap_or_else(|_| Err(operation_unknown()))
        };
        if let Some(operation_id) = request.operation_id.filter(|_| mutating) {
            state
                .results
                .lock()
                .await
                .insert(operation_id, response.clone());
        }
        operation_diagnostic(
            kind,
            &request.request_id,
            "completed",
            started.elapsed().as_millis() as u64,
            response.as_ref().err().map(|error| error.code.as_str()),
        );
        response
    });
    tokio::time::timeout(OPERATION_TIMEOUT, task)
        .await
        .map_err(|_| {
            operation_diagnostic(
                kind,
                &diagnostic_id,
                "response_deadline_unknown",
                OPERATION_TIMEOUT.as_millis() as u64,
                Some("operation_unknown"),
            );
            operation_unknown()
        })?
        .unwrap_or_else(|_| Err(operation_unknown()))
}

fn operation_unknown() -> HostError {
    HostError::new(
        "operation_unknown",
        "operation outcome unknown; do not retry with a new id; reconcile the owned process",
    )
}

fn execute_runtime(
    runtime: &HostRuntime,
    restart: &mpsc::Sender<()>,
    command: &HostCommand,
) -> Result<HostResponse, HostError> {
    match command {
        HostCommand::Sync { cursors } => sync_runtime(runtime, cursors)
            .map_err(|_| HostError::new("sync_failed", "could not snapshot host runtime")),
        HostCommand::Spawn { request } => Ok(HostResponse::Process {
            process: runtime.spawn(request.clone()).map_err(host_runtime_error)?,
        }),
        HostCommand::Read { process_id, cursor } => Ok(HostResponse::Output {
            replay: runtime
                .read(process_id, cursor.as_ref())
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Write { process_id, writes } => Ok(HostResponse::Process {
            process: runtime
                .write(process_id, writes)
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Resize {
            process_id,
            cols,
            rows,
        } => Ok(HostResponse::Process {
            process: runtime
                .resize(process_id, *cols, *rows)
                .map_err(host_runtime_error)?,
        }),
        HostCommand::Stop { process_id } => Ok(HostResponse::Process {
            process: runtime.stop(process_id).map_err(host_runtime_error)?,
        }),
        HostCommand::RestartController => {
            restart.try_send(()).map_err(|_| {
                HostError::new(
                    "restart_busy",
                    "controller supervisor busy or stopped; not admitted",
                )
            })?;
            Ok(HostResponse::Ack)
        }
    }
}

fn host_runtime_error(error: RuntimeError) -> HostError {
    HostError::new(error.code(), error.to_string())
}

fn send_response(
    outgoing: &mpsc::Sender<HostMessage>,
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
    let _ = outgoing.try_send(message);
}

async fn request_controller_restart(socket: &Path) -> Result<()> {
    let mut stream = connect_local(socket)
        .await
        .with_context(|| format!("failed to connect to {}", socket.display()))?;
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let request = HostRequest {
        protocol: HOST_PROTOCOL_VERSION,
        request_id: request_id.clone(),
        operation_id: Some(format!("restart_{}", Uuid::new_v4().simple())),
        command: HostCommand::RestartController,
    };
    write_frame(&mut stream, &request)
        .await
        .context("failed to send restart request")?;
    let message = tokio::time::timeout(
        Duration::from_secs(5),
        read_frame::<_, HostMessage>(&mut stream),
    )
    .await
    .context("timed out waiting for host")??
    .context("host closed without a response")?;
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

async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let encoded = encode_message(value).map_err(anyhow::Error::msg)?;
    if encoded.len() > MAX_HOST_FRAME_BYTES {
        bail!("host frame exceeds {MAX_HOST_FRAME_BYTES} bytes");
    }
    let length = u32::try_from(encoded.len()).context("host frame length exceeds u32")?;
    writer.write_u32(length).await?;
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let length = match reader.read_u32().await {
        Ok(length) => usize::try_from(length).context("invalid host frame length")?,
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if length > MAX_HOST_FRAME_BYTES {
        bail!("host frame exceeds {MAX_HOST_FRAME_BYTES} bytes");
    }
    let mut encoded = vec![0_u8; length];
    reader.read_exact(&mut encoded).await?;
    decode_message(&encoded)
        .map(Some)
        .map_err(anyhow::Error::msg)
}

fn prepare_socket(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        remove_socket(path)
    }
}

fn remove_socket(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(unix)]
    {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to remove {}", path.display()))
            }
        }
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

    // Observe consumption, rather than relying on sleeps to interrupt a frame
    // at its exact length/payload boundary.
    struct ObservedStream {
        stream: tokio::io::DuplexStream,
        consumed: usize,
        boundary: usize,
        reached: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl AsyncRead for ObservedStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let before = buf.filled().len();
            let result = std::pin::Pin::new(&mut self.stream).poll_read(cx, buf);
            self.consumed += buf.filled().len() - before;
            if self.consumed >= self.boundary {
                if let Some(reached) = self.reached.take() {
                    let _ = reached.send(());
                }
            }
            result
        }
    }

    impl AsyncWrite for ObservedStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.stream).poll_write(cx, bytes)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    async fn fragmented_request_with_process_event(smuggled: bool) {
        let root =
            std::env::temp_dir().join(format!("treer-host-fragment-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let (restart_controller, mut restarts) = mpsc::channel(1);
        let state = Arc::new(HostState {
            runtime: HostRuntime::new(&root).unwrap(),
            results: Arc::new(Mutex::new(ResultCache::new())),
            operations: Arc::new(Semaphore::new(16)),
            controls: Arc::new(Semaphore::new(8)),
            restart_controller,
        });
        let sync = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "sync".into(),
            operation_id: None,
            command: HostCommand::Sync {
                cursors: Default::default(),
            },
        };
        let (mut client, stream) = tokio::io::duplex(4096);
        let (reached, boundary) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(handle_connection(
            Box::new(ObservedStream {
                stream,
                consumed: 0,
                boundary: 4 + encode_message(&sync).unwrap().len() + 4,
                reached: Some(reached),
            }),
            Arc::clone(&state),
        ));
        write_frame(&mut client, &sync).await.unwrap();
        assert!(matches!(
            read_frame::<_, HostMessage>(&mut client).await.unwrap(),
            Some(HostMessage::Response {
                response: Some(HostResponse::Synced { .. }),
                ..
            })
        ));
        let payload = if smuggled {
            let forged = HostRequest {
                protocol: HOST_PROTOCOL_VERSION,
                request_id: "forged".into(),
                operation_id: Some("forged-op".into()),
                command: HostCommand::RestartController,
            };
            let encoded = encode_message(&forged).unwrap();
            let mut nested = (encoded.len() as u32).to_be_bytes().to_vec();
            nested.extend(encoded);
            nested
        } else {
            encode_message(&sync).unwrap()
        };
        client.write_u32(payload.len() as u32).await.unwrap();
        boundary.await.unwrap(); // the length has been consumed, payload withheld
        #[cfg(unix)]
        let (command, args) = ("/bin/sh", vec!["-c".into(), "printf fragment-event".into()]);
        #[cfg(windows)]
        let (command, args) = (
            "cmd.exe",
            vec!["/d".into(), "/c".into(), "echo fragment-event".into()],
        );
        state
            .runtime
            .spawn(HostSpawnRequest {
                process_id: "event".into(),
                command: command.into(),
                args,
                cwd: ".".into(),
                env: Default::default(),
                rows: 24,
                cols: 80,
                metadata: "{}".into(),
            })
            .unwrap();
        // Receiving the event proves select chose another branch while the
        // frame payload was incomplete; no timing/retry assumptions.
        let (mut process_seen, mut output_seen) = (false, false);
        while !process_seen || !output_seen {
            match read_frame::<_, HostMessage>(&mut client)
                .await
                .unwrap()
                .unwrap()
            {
                HostMessage::Process { .. } => process_seen = true,
                HostMessage::Output { .. } => output_seen = true,
                other => panic!("unexpected message during partial frame: {other:?}"),
            }
        }
        client.write_all(&payload).await.unwrap();
        if smuggled {
            let result = tokio::select! {
                result = server => result.unwrap(),
                _ = restarts.recv() => panic!("nested request executed from invalid outer frame"),
            };
            assert!(
                result.is_err(),
                "invalid outer payload must close the connection"
            );
            assert!(
                restarts.try_recv().is_err(),
                "nested request must never execute"
            );
        } else {
            loop {
                match read_frame::<_, HostMessage>(&mut client)
                    .await
                    .unwrap()
                    .expect("connection remains open")
                {
                    HostMessage::Response {
                        request_id,
                        response: Some(HostResponse::Synced { .. }),
                        ..
                    } => {
                        assert_eq!(request_id, "sync");
                        break;
                    }
                    HostMessage::Output { .. } | HostMessage::Process { .. } => {}
                    other => panic!("unexpected response: {other:?}"),
                }
            }
            drop(client);
            server.await.unwrap().unwrap();
        }
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn partial_frame_survives_process_and_output_events() {
        tokio::time::timeout(
            Duration::from_secs(10),
            fragmented_request_with_process_event(false),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn invalid_outer_frame_never_executes_embedded_request() {
        tokio::time::timeout(
            Duration::from_secs(10),
            fragmented_request_with_process_event(true),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn invalid_length_decode_or_protocol_closes_without_parsing_next_frame() {
        tokio::time::timeout(Duration::from_secs(10), async {
            for variant in 0..3 {
                let root = std::env::temp_dir()
                    .join(format!("treer-host-invalid-{}", Uuid::new_v4().simple()));
                std::fs::create_dir_all(&root).unwrap();
                let (restart_controller, mut restarts) = mpsc::channel(1);
                let state = Arc::new(HostState {
                    runtime: HostRuntime::new(&root).unwrap(),
                    results: Arc::new(Mutex::new(ResultCache::new())),
                    operations: Arc::new(Semaphore::new(16)),
                    controls: Arc::new(Semaphore::new(8)),
                    restart_controller,
                });
                let (mut client, stream) = tokio::io::duplex(4096);
                let server = tokio::spawn(handle_connection(Box::new(stream), state));
                match variant {
                    0 => client
                        .write_u32(MAX_HOST_FRAME_BYTES as u32 + 1)
                        .await
                        .unwrap(),
                    1 => {
                        client.write_u32(1).await.unwrap();
                        client.write_all(&[255]).await.unwrap();
                    }
                    _ => write_frame(
                        &mut client,
                        &HostRequest {
                            protocol: HOST_PROTOCOL_VERSION + 1,
                            request_id: "bad-protocol".into(),
                            operation_id: None,
                            command: HostCommand::Sync {
                                cursors: Default::default(),
                            },
                        },
                    )
                    .await
                    .unwrap(),
                }
                write_frame(
                    &mut client,
                    &HostRequest {
                        protocol: HOST_PROTOCOL_VERSION,
                        request_id: "following".into(),
                        operation_id: Some("following-op".into()),
                        command: HostCommand::RestartController,
                    },
                )
                .await
                .unwrap();
                assert!(
                    server.await.unwrap().is_err(),
                    "variant {variant} must close"
                );
                assert!(
                    restarts.try_recv().is_err(),
                    "variant {variant} executed trailing request"
                );
                let _ = std::fs::remove_dir_all(root);
            }
        })
        .await
        .unwrap();
    }

    fn test_command() -> (String, Vec<String>) {
        #[cfg(windows)]
        return (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "Start-Sleep -Seconds 5".into(),
            ],
        );
        #[cfg(unix)]
        ("/bin/sh".into(), vec!["-c".into(), "sleep 5".into()])
    }

    #[tokio::test]
    async fn repeated_operation_id_spawns_once() {
        let root = std::env::temp_dir().join(format!(
            "treer-host-idempotency-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create test root");
        let runtime = HostRuntime::new(&root).expect("create runtime");
        let (restart_controller, _) = mpsc::channel(1);
        let state = HostState {
            runtime,
            results: Arc::new(Mutex::new(ResultCache::new())),
            operations: Arc::new(Semaphore::new(16)),
            controls: Arc::new(Semaphore::new(8)),
            restart_controller,
        };
        let request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "req_1".to_string(),
            operation_id: Some("op_1".to_string()),
            command: HostCommand::Spawn {
                request: HostSpawnRequest {
                    process_id: "p1".to_string(),
                    command: test_command().0,
                    args: test_command().1,
                    cwd: ".".to_string(),
                    env: BTreeMap::new(),
                    cols: 80,
                    rows: 24,
                    metadata: json!({"test": true}).to_string(),
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
        let (restart_controller, _) = mpsc::channel(1);
        let state = HostState {
            runtime,
            results: Arc::new(Mutex::new(ResultCache::new())),
            operations: Arc::new(Semaphore::new(16)),
            controls: Arc::new(Semaphore::new(8)),
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
                    metadata: json!({"test": true}).to_string(),
                },
            },
        };
        let first = execute_request(&state, &request)
            .await
            .expect_err("first spawn should fail");
        assert_eq!(first, execute_request(&state, &request).await.unwrap_err());
        let HostCommand::Spawn { request: spawn } = &mut request.command else {
            unreachable!("test request is spawn");
        };
        spawn.cwd = ".".to_string();
        let second = execute_request(&state, &request)
            .await
            .expect_err("changed command must conflict");
        assert_eq!(second.code, "operation_conflict");
        assert!(state.runtime.list().is_empty());
    }
}

#[cfg(test)]
mod isolation_tests;
