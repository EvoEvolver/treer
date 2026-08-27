use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use treer_protocol::{
    AgentCommand, AgentServerMessage, AgentServerSnapshot, CommandEnvelope, CommandResult,
    NetworkBinaryFrame, ProtocolError, ProxyMessage, ServerInfo, ServerStatus, TerminalBinaryFrame,
    TerminalBinaryKind, TerminalCursor, PROTOCOL_VERSION,
};
use url::Url;

use crate::controller::{ControllerRuntime, TerminalSnapshot};
use crate::network::NetworkRuntime;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const RESULT_CACHE_LIMIT: usize = 256;

#[derive(Clone)]
struct TerminalRelay {
    process_id: String,
    stream_epoch: String,
    last_revision: u64,
}

struct PendingClose {
    deadline: Instant,
    final_revision: u64,
    exit_code: Option<i32>,
}

#[derive(Clone)]
pub struct ProxyClient {
    pub proxy_ws: Url,
    pub machine_token: Option<String>,
    pub server: ServerInfo,
    pub runtime: ControllerRuntime,
    pub network: NetworkRuntime,
    controller_instance_id: String,
    command_cache: Arc<Mutex<HashMap<String, CommandResult>>>,
    advertised_agents: Arc<Mutex<Vec<String>>>,
}

impl ProxyClient {
    pub fn new(
        proxy_ws: Url,
        machine_token: Option<String>,
        server: ServerInfo,
        runtime: ControllerRuntime,
        network: NetworkRuntime,
    ) -> Self {
        let advertised_agents = server.available_agents.clone().unwrap_or_default();
        Self {
            proxy_ws,
            machine_token,
            server,
            runtime,
            network,
            controller_instance_id: format!("ctl_{}", uuid::Uuid::new_v4().simple()),
            command_cache: Arc::new(Mutex::new(HashMap::new())),
            advertised_agents: Arc::new(Mutex::new(advertised_agents)),
        }
    }

    fn advertised_server(&self) -> ServerInfo {
        let kinds = self.runtime.available_agent_kinds();
        let mut server = self.server.clone();
        server.available_agents = Some(kinds.clone());
        if let Ok(mut advertised) = self.advertised_agents.lock() {
            *advertised = kinds;
        }
        server
    }

    pub async fn run_forever(self) {
        let mut delay = Duration::from_millis(300);
        loop {
            match self.run_connection().await {
                Ok(ConnectionDisposition::Reconnect) => warn!("proxy connection closed"),
                Ok(ConnectionDisposition::StopDuplicate) => {
                    self.network.reset_all().await;
                    warn!(
                        controller_instance_id = %self.controller_instance_id,
                        "another Controller claimed this machine identity; stopping Proxy reconnects"
                    );
                    break;
                }
                Err(err) => warn!(error = %format_args!("{err:#}"), "proxy connection failed"),
            }
            self.network.reset_all().await;
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    }

    async fn run_connection(&self) -> anyhow::Result<ConnectionDisposition> {
        let mut request = self
            .proxy_ws
            .as_str()
            .into_client_request()
            .context("failed to create proxy websocket request")?;
        if let Some(token) = &self.machine_token {
            request.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("invalid machine token")?,
            );
        }
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("failed to connect to {}", self.proxy_ws))?;
        let (mut outgoing, mut incoming) = socket.split();
        send(
            &mut outgoing,
            &AgentServerMessage::Register {
                protocol: PROTOCOL_VERSION,
                controller_instance_id: self.controller_instance_id.clone(),
                server: self.advertised_server(),
            },
        )
        .await?;
        send(
            &mut outgoing,
            &AgentServerMessage::Snapshot {
                snapshot: AgentServerSnapshot {
                    server: self.advertised_server(),
                    agents: self.runtime.list(),
                },
            },
        )
        .await?;

        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut events = self.runtime.subscribe();
        let mut terminal_events = self.runtime.subscribe_terminal();
        let mut process_events = self.runtime.subscribe_processes();
        let mut terminal_sessions = HashMap::<String, TerminalRelay>::new();
        let mut pending_closes = HashMap::<String, PendingClose>::new();
        let mut close_tick = tokio::time::interval(Duration::from_millis(25));
        close_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        info!(proxy = %self.proxy_ws, "connected to proxy");

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    send(&mut outgoing, &AgentServerMessage::Heartbeat { sent_at: Utc::now() }).await?;
                    let kinds = self.runtime.available_agent_kinds();
                    let changed = self
                        .advertised_agents
                        .lock()
                        .map(|advertised| *advertised != kinds)
                        .unwrap_or(false);
                    if changed {
                        send(
                            &mut outgoing,
                            &AgentServerMessage::Snapshot {
                                snapshot: AgentServerSnapshot {
                                    server: self.advertised_server(),
                                    agents: self.runtime.list(),
                                },
                            },
                        )
                        .await?;
                    }
                }
                event = events.recv() => match event {
                    Ok(agent) => send(&mut outgoing, &AgentServerMessage::AgentEvent { agent }).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        send(&mut outgoing, &AgentServerMessage::Snapshot {
                            snapshot: AgentServerSnapshot {
                                server: self.advertised_server(),
                                agents: self.runtime.list(),
                            },
                        }).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("runtime event channel closed"));
                    }
                },
                event = terminal_events.recv() => match event {
                    Ok(event) => {
                        let frames = terminal_sessions
                            .iter_mut()
                            .filter_map(|(session_id, relay)| {
                                if relay.process_id != event.process_id || event.revision <= relay.last_revision {
                                    return None;
                                }
                                relay.last_revision = event.revision;
                                Some(TerminalBinaryFrame {
                                    kind: TerminalBinaryKind::Output,
                                    session_id: session_id.clone(),
                                    revision: event.revision,
                                    payload: event.data.clone(),
                                })
                            })
                            .collect::<Vec<_>>();
                        for frame in frames {
                            send_binary(&mut outgoing, frame).await?;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "terminal output relay lagged; resyncing from host");
                        let sessions = terminal_sessions
                            .iter()
                            .map(|(session_id, relay)| (session_id.clone(), relay.clone()))
                            .collect::<Vec<_>>();
                        for (session_id, relay) in sessions {
                            let cursor = TerminalCursor {
                                stream_epoch: relay.stream_epoch,
                                revision: relay.last_revision,
                            };
                            match self
                                .runtime
                                .terminal_snapshot(&relay.process_id, Some(&cursor))
                                .await
                            {
                                Ok(snapshot) => {
                                    if snapshot.data.is_empty() && !snapshot.gap {
                                        continue;
                                    }
                                    if let Some(relay) = terminal_sessions.get_mut(&session_id) {
                                        relay.stream_epoch = snapshot.stream_epoch.clone();
                                        relay.last_revision = snapshot.revision;
                                    }
                                    publish_terminal_replay(&mut outgoing, session_id, snapshot)
                                        .await?;
                                }
                                Err(error) => warn!(
                                    code = %error.code,
                                    message = %error.message,
                                    %session_id,
                                    "failed to resync lagged terminal"
                                ),
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("terminal event channel closed"));
                    }
                },
                event = process_events.recv() => match event {
                    Ok(process) if !process.running => {
                        let deadline = Instant::now() + Duration::from_secs(1);
                        let final_revision = process.next_revision.saturating_sub(1);
                        for (session_id, relay) in &terminal_sessions {
                            if relay.process_id == process.process_id {
                                pending_closes.insert(session_id.clone(), PendingClose {
                                    deadline,
                                    final_revision,
                                    exit_code: process.exit_code,
                                });
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "process event relay lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("process event channel closed"));
                    }
                },
                _ = close_tick.tick() => {
                    let now = Instant::now();
                    let due = pending_closes
                        .iter()
                        .filter(|(session_id, close)| {
                            terminal_sessions
                                .get(*session_id)
                                .is_none_or(|relay| relay.last_revision >= close.final_revision)
                                || close.deadline <= now
                        })
                        .map(|(session_id, _)| session_id.clone())
                        .collect::<Vec<_>>();
                    for session_id in due {
                        let Some(close) = pending_closes.remove(&session_id) else { continue };
                        let Some(_) = terminal_sessions.remove(&session_id) else { continue };
                        send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                            session_id,
                            reason: Some("remote process exited".to_string()),
                            exit_code: close.exit_code,
                        }).await?;
                    }
                },
                frame = self.network.next_outgoing() => {
                    let frame = frame.ok_or_else(|| anyhow!("network runtime stopped"))?;
                    send_network_binary(&mut outgoing, frame).await?;
                }
                message = incoming.next() => {
                    let Some(message) = message else {
                        return Err(anyhow!("proxy closed the websocket"));
                    };
                    let message = message.context("failed to read proxy websocket")?;
                    let Message::Text(text) = message else {
                        match message {
                            Message::Binary(encoded) => {
                                if NetworkBinaryFrame::is_network_frame(&encoded) {
                                    let frame = NetworkBinaryFrame::decode(&encoded)
                                        .map_err(|error| anyhow!("{}: {}", error.code, error.message))?;
                                    if let Err(error) = self.network.handle_incoming(frame).await {
                                        warn!(%error, "failed to handle network frame");
                                    }
                                    continue;
                                }
                                let frame = TerminalBinaryFrame::decode(&encoded)
                                    .map_err(|error| anyhow!("{}: {}", error.code, error.message))?;
                                if frame.kind != TerminalBinaryKind::Input {
                                    return Err(anyhow!("proxy sent unexpected {:?} terminal frame", frame.kind));
                                }
                                let result = match terminal_sessions.get(&frame.session_id) {
                                    Some(relay) => {
                                        let operation_id = format!(
                                            "{}:input:{}",
                                            frame.session_id,
                                            uuid::Uuid::new_v4().simple()
                                        );
                                        self.runtime
                                            .write_raw(&operation_id, &relay.process_id, &frame.payload)
                                            .await
                                            .map(|_| ())
                                            .map_err(|error| anyhow!(error.message))
                                    }
                                    None => Err(anyhow!("unknown terminal session {}", frame.session_id)),
                                };
                                if let Err(error) = result {
                                    terminal_sessions.remove(&frame.session_id);
                                    pending_closes.remove(&frame.session_id);
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id: frame.session_id,
                                        reason: Some(error.to_string()),
                                        exit_code: None,
                                    }).await?;
                                }
                            }
                            Message::Close(_) => return Ok(ConnectionDisposition::Reconnect),
                            _ => {}
                        }
                        continue;
                    };
                    let message: ProxyMessage = serde_json::from_str(&text)
                        .context("failed to decode proxy message")?;
                    match message {
                        ProxyMessage::Registered { .. } => {
                            if let Err(error) = self.runtime.reset_virtual_hosts() {
                                warn!(code = %error.code, message = %error.message, "failed to reset virtual-host snapshot");
                            }
                        }
                        ProxyMessage::VirtualNetworkHosts { snapshot } => {
                            let revision = snapshot.revision;
                            let count = snapshot.hosts.len();
                            match self.runtime.replace_virtual_hosts(snapshot) {
                                Ok(true) => info!(revision, count, "virtual hosts refreshed"),
                                Ok(false) => tracing::debug!(revision, "ignored stale virtual-host snapshot"),
                                Err(error) => warn!(code = %error.code, message = %error.message, "rejected virtual-host snapshot"),
                            }
                        }
                        ProxyMessage::Error { error } => {
                            warn!(code = %error.code, message = %error.message, "proxy rejected a message");
                            if let Some(disposition) = connection_error_disposition(&error.code) {
                                return Ok(disposition);
                            }
                        }
                        ProxyMessage::Command { envelope } => {
                            let result = self.execute(envelope).await;
                            send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                        }
                        ProxyMessage::TerminalAttach { session_id, agent_id, cols, rows, cursor } => {
                            let operation_id = format!("{session_id}:attach");
                            match self.runtime.terminal_snapshot(&agent_id, cursor.as_ref()).await {
                                Ok(snapshot) => {
                                    match self.runtime.resize(&operation_id, &agent_id, cols, rows).await {
                                        Ok(()) => {
                                            terminal_sessions.insert(
                                                session_id.clone(),
                                                TerminalRelay {
                                                    process_id: agent_id,
                                                    stream_epoch: snapshot.stream_epoch.clone(),
                                                    last_revision: snapshot.revision,
                                                },
                                            );
                                            publish_terminal_replay(&mut outgoing, session_id, snapshot).await?;
                                        }
                                        Err(error) => {
                                            send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                                session_id,
                                                reason: Some(error.message),
                                                exit_code: None,
                                            }).await?;
                                        }
                                    }
                                }
                                Err(error) => {
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id,
                                        reason: Some(error.message),
                                        exit_code: None,
                                    }).await?;
                                }
                            }
                        }
                        ProxyMessage::TerminalResize { session_id, cols, rows } => {
                            if let Some(relay) = terminal_sessions.get(&session_id) {
                                let operation_id = format!("{session_id}:resize:{}", uuid::Uuid::new_v4().simple());
                                if let Err(error) = self.runtime.resize(&operation_id, &relay.process_id, cols, rows).await {
                                    terminal_sessions.remove(&session_id);
                                    pending_closes.remove(&session_id);
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id,
                                        reason: Some(error.message),
                                        exit_code: None,
                                    }).await?;
                                }
                            }
                        }
                        ProxyMessage::TerminalDetach { session_id } => {
                            terminal_sessions.remove(&session_id);
                            pending_closes.remove(&session_id);
                        }
                    }
                }
            }
        }
    }

    async fn execute(&self, envelope: CommandEnvelope) -> CommandResult {
        if envelope.workspace_id != self.server.workspace_id {
            return CommandResult::failure(
                envelope.command_id,
                ProtocolError::new("workspace_mismatch", "command targets another workspace"),
            );
        }
        if let Some(result) = self
            .command_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&envelope.command_id).cloned())
        {
            return result;
        }
        let command_id = envelope.command_id;
        let result = match envelope.command {
            AgentCommand::Create {
                agent_id,
                workload_credential,
                request,
            } => self
                .runtime
                .create(&command_id, agent_id, workload_credential, request)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Prompt { agent_id, text } => self
                .runtime
                .prompt(&command_id, &agent_id, &text)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Input { agent_id, data } => self
                .runtime
                .write_raw(&command_id, &agent_id, &data)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::Read { agent_id, lines } => self
                .runtime
                .read(&agent_id, lines)
                .map(|read| CommandResult::success(command_id.clone(), read))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Transcript {
                agent_id,
                cursor,
                limit,
            } => self
                .runtime
                .transcript(&agent_id, cursor.as_deref(), limit)
                .await
                .map(|transcript| CommandResult::success(command_id.clone(), transcript))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Stop { agent_id } => self
                .runtime
                .stop(&command_id, &agent_id)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::ProbeNetwork {
                host,
                port,
                timeout_ms,
                target_agent_id,
            } => CommandResult::success(
                command_id.clone(),
                self.network
                    .probe(host, port, target_agent_id, timeout_ms)
                    .await,
            ),
            AgentCommand::ShutdownMachine => {
                schedule_machine_shutdown(self.server.workspace_id.clone());
                CommandResult::success(command_id.clone(), serde_json::json!({ "accepted": true }))
            }
        };
        if let Ok(mut cache) = self.command_cache.lock() {
            if cache.len() >= RESULT_CACHE_LIMIT {
                if let Some(key) = cache.keys().next().cloned() {
                    cache.remove(&key);
                }
            }
            cache.insert(command_id, result.clone());
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionDisposition {
    Reconnect,
    StopDuplicate,
}

fn connection_error_disposition(code: &str) -> Option<ConnectionDisposition> {
    match code {
        "duplicate_machine_connection" => Some(ConnectionDisposition::StopDuplicate),
        "stale_connection" => Some(ConnectionDisposition::Reconnect),
        _ => None,
    }
}

fn schedule_machine_shutdown(workspace: String) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let shutdown_workspace = workspace.clone();
        match tokio::task::spawn_blocking(move || {
            crate::service::stop_remotely(&shutdown_workspace)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(workspace, %error, "failed to stop machine service"),
            Err(error) => warn!(workspace, %error, "machine service stop task failed"),
        }
    });
}

pub fn server_info(
    server_id: String,
    workspace_id: String,
    hostname: String,
    root: String,
    host_build: treer_protocol::BuildInfo,
    available_agents: Vec<String>,
) -> ServerInfo {
    let now = Utc::now();
    ServerInfo {
        server_id,
        workspace_id,
        name: hostname.clone(),
        hostname,
        root,
        controller_build: treer_protocol::BuildInfo {
            version: treer_build_info::VERSION.to_string(),
            git_commit: treer_build_info::GIT_COMMIT.to_string(),
        },
        host_build,
        labels: std::collections::BTreeMap::from([
            ("os".to_string(), std::env::consts::OS.to_string()),
            ("arch".to_string(), std::env::consts::ARCH.to_string()),
            ("treer.network".to_string(), "1".to_string()),
            ("treer.shutdown".to_string(), "1".to_string()),
        ]),
        available_agents: Some(available_agents),
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    }
}

async fn publish_terminal_replay<S>(
    outgoing: &mut S,
    session_id: String,
    snapshot: TerminalSnapshot,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    send(
        outgoing,
        &AgentServerMessage::TerminalReady {
            session_id: session_id.clone(),
            stream_epoch: snapshot.stream_epoch,
            revision: snapshot.revision,
            gap: snapshot.gap,
        },
    )
    .await?;
    send_binary(
        outgoing,
        TerminalBinaryFrame {
            kind: TerminalBinaryKind::Ready,
            session_id,
            revision: snapshot.revision,
            payload: snapshot.data,
        },
    )
    .await
}

async fn send<S>(outgoing: &mut S, message: &AgentServerMessage) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let encoded = serde_json::to_string(message).context("failed to encode agent message")?;
    outgoing
        .send(Message::Text(encoded.into()))
        .await
        .context("failed to send agent message")
}

async fn send_binary<S>(outgoing: &mut S, frame: TerminalBinaryFrame) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let encoded = frame
        .encode()
        .map_err(|error| anyhow!("{}: {}", error.code, error.message))?;
    outgoing
        .send(Message::Binary(encoded.into()))
        .await
        .context("failed to send terminal binary frame")
}

async fn send_network_binary<S>(outgoing: &mut S, frame: NetworkBinaryFrame) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let encoded = frame
        .encode()
        .map_err(|error| anyhow!("{}: {}", error.code, error.message))?;
    outgoing
        .send(Message::Binary(encoded.into()))
        .await
        .context("failed to send network binary frame")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    #[test]
    fn server_advertises_remote_shutdown_support() {
        let server = server_info(
            "server-a".to_string(),
            "default".to_string(),
            "machine-a".to_string(),
            "/workspace".to_string(),
            treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "0123456789abcdef".to_string(),
            },
            vec!["claude".to_string()],
        );
        assert_eq!(server.available_agents, Some(vec!["claude".into()]));
        assert_eq!(
            server.labels.get("treer.shutdown").map(String::as_str),
            Some("1")
        );
        assert_eq!(server.controller_build.version, treer_build_info::VERSION);
        assert_eq!(
            server.controller_build.git_commit,
            treer_build_info::GIT_COMMIT
        );
        assert_eq!(server.host_build.version, "0.1.2");
        assert_eq!(server.host_build.git_commit, "0123456789abcdef");
    }

    #[test]
    fn stale_cluster_ownership_reconnects_but_a_duplicate_controller_stops() {
        assert_eq!(
            connection_error_disposition("stale_connection"),
            Some(ConnectionDisposition::Reconnect)
        );
        assert_eq!(
            connection_error_disposition("duplicate_machine_connection"),
            Some(ConnectionDisposition::StopDuplicate)
        );
        assert_eq!(connection_error_disposition("machine_revoked"), None);
    }

    #[tokio::test]
    async fn network_probe_runs_from_the_controller_network() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind service");
        let port = listener.local_addr().expect("service address").port();
        let runtime =
            NetworkRuntime::bind_near(listener.local_addr().expect("listener address"), false)
                .await
                .expect("network runtime");
        let healthy = runtime
            .probe(Ipv4Addr::LOCALHOST.to_string(), port, None, 500)
            .await;
        assert_eq!(healthy["healthy"], true);
    }
}
