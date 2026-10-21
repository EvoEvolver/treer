use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use chrono::Utc;
use futures_util::{stream::FuturesUnordered, SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use treer_protocol::{
    AgentCommand, AgentServerMessage, AgentServerSnapshot, CommandEnvelope, CommandResult,
    NetworkBinaryFrame, ProtocolError, ProxyMessage, ServerInfo, ServerStatus, TerminalBinaryFrame,
    TerminalBinaryKind, TerminalCursor, ValidateAgentStartupRequest, ValidateAgentStartupResponse,
    PROTOCOL_MIN_VERSION, PROTOCOL_VERSION,
};
use url::Url;

use crate::controller::{ControllerRuntime, ProxyLinkStatus, TerminalSnapshot};
use crate::network::NetworkRuntime;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const PROXY_DEAD_INTERVAL: Duration = Duration::from_secs(60);
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

enum DecodedProxyMessage {
    Known(Box<ProxyMessage>),
    Unknown { message_type: String },
    UnsupportedCommand { command_id: String, action: String },
}

#[derive(Deserialize)]
struct ProxyMessageHeader {
    #[serde(rename = "type")]
    message_type: String,
}

#[derive(Deserialize)]
struct RawCommandMessage {
    envelope: RawCommandEnvelope,
}

#[derive(Deserialize)]
struct RawCommandEnvelope {
    command_id: String,
    workspace_id: String,
    command: Value,
}

fn decode_proxy_message(text: &str) -> anyhow::Result<DecodedProxyMessage> {
    let value: Value = serde_json::from_str(text).context("failed to decode proxy message JSON")?;
    let header: ProxyMessageHeader =
        serde_json::from_value(value.clone()).context("proxy message is missing a valid type")?;
    if header.message_type == "command" {
        let raw: RawCommandMessage =
            serde_json::from_value(value).context("failed to decode proxy command envelope")?;
        let action = raw
            .envelope
            .command
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let command = match serde_json::from_value(raw.envelope.command) {
            Ok(command) => command,
            Err(_) => {
                return Ok(DecodedProxyMessage::UnsupportedCommand {
                    command_id: raw.envelope.command_id,
                    action,
                })
            }
        };
        return Ok(DecodedProxyMessage::Known(Box::new(
            ProxyMessage::Command {
                envelope: CommandEnvelope {
                    command_id: raw.envelope.command_id,
                    workspace_id: raw.envelope.workspace_id,
                    command,
                },
            },
        )));
    }
    if !matches!(
        header.message_type.as_str(),
        "registered"
            | "virtual_network_hosts"
            | "terminal_attach"
            | "terminal_resize"
            | "terminal_detach"
            | "error"
    ) {
        return Ok(DecodedProxyMessage::Unknown {
            message_type: header.message_type,
        });
    }
    serde_json::from_value(value)
        .map(Box::new)
        .map(DecodedProxyMessage::Known)
        .context("failed to decode proxy message")
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
    last_activity: Arc<Mutex<Instant>>,
    continue_signal: Arc<Notify>,
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
        let continue_signal = Arc::new(Notify::new());
        #[cfg(unix)]
        {
            let signal = continue_signal.clone();
            tokio::spawn(async move {
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(
                    libc::SIGCONT,
                )) {
                    Ok(mut stream) => {
                        while stream.recv().await.is_some() {
                            signal.notify_waiters();
                        }
                    }
                    Err(error) => warn!(%error, "failed to listen for SIGCONT"),
                }
            });
        }
        Self {
            proxy_ws,
            machine_token,
            server,
            runtime,
            network,
            controller_instance_id: format!("ctl_{}", uuid::Uuid::new_v4().simple()),
            command_cache: Arc::new(Mutex::new(HashMap::new())),
            advertised_agents: Arc::new(Mutex::new(advertised_agents)),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            continue_signal,
        }
    }

    fn touch_activity(&self) {
        if let Ok(mut last_activity) = self.last_activity.lock() {
            *last_activity = Instant::now();
        }
    }

    fn mark_proxy_link(&self, connected: bool, code: Option<&str>, message: Option<&str>) {
        let previous = self.runtime.proxy_link_status();
        self.runtime.set_proxy_link_status(ProxyLinkStatus {
            connected,
            last_error: message
                .map(str::to_string)
                .or_else(|| previous.last_error.filter(|_| !connected)),
            last_error_code: code
                .map(str::to_string)
                .or_else(|| previous.last_error_code.filter(|_| !connected)),
        });
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
                Ok(ConnectionDisposition::Reconnect) => {
                    self.mark_proxy_link(false, None, None);
                    warn!("proxy connection closed");
                }
                Ok(ConnectionDisposition::Stop) => {
                    self.network.reset_all().await;
                    warn!(
                        controller_instance_id = %self.controller_instance_id,
                        "stopping Proxy reconnects"
                    );
                    break;
                }
                Err(err) => {
                    self.mark_proxy_link(false, None, Some(&format!("{err:#}")));
                    warn!(error = %format_args!("{err:#}"), "proxy connection failed");
                }
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
        self.touch_activity();
        self.mark_proxy_link(true, None, None);
        send(
            &mut outgoing,
            &AgentServerMessage::Register {
                protocol: PROTOCOL_VERSION,
                supported_protocols: (PROTOCOL_MIN_VERSION..=PROTOCOL_VERSION).collect(),
                capabilities: self.runtime.advertised_capabilities(),
                controller_instance_id: self.controller_instance_id.clone(),
                server: self.advertised_server(),
            },
        )
        .await?;
        let candidates = self.runtime.automatic_startup_candidate_ids();
        let validation_ids = self.runtime.startup_validation_ids();
        if !validation_ids.is_empty() {
            match self.validate_startup_agents(validation_ids).await {
                Ok(active) => {
                    let active: Vec<_> = active
                        .into_iter()
                        .filter(|id| candidates.contains(id))
                        .collect();
                    for (agent_id, result) in self.runtime.restore_startup_agents(&active).await {
                        match result {
                            Ok(_) => info!(%agent_id, "restored Agent startup command"),
                            Err(error) => warn!(
                                %agent_id,
                                code = %error.code,
                                message = %error.message,
                                "failed to restore Agent startup command"
                            ),
                        }
                    }
                }
                Err(error) => {
                    warn!(error = %format!("{error:#}"), retry_seconds = HEARTBEAT_INTERVAL.as_secs(), "Agent startup validation failed; deferring restore until heartbeat retry")
                }
            }
        }
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

        // Open frames may only follow this connection's machine registration
        // and Agent snapshot. Offline requests fail locally instead of being
        // replayed as new connections after sleep/reconnect.
        self.network.set_proxy_connected();

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
        let mut exec_commands = FuturesUnordered::new();
        let mut exec_ids = std::collections::HashSet::new();

        loop {
            tokio::select! {
                Some(result) = exec_commands.next(), if !exec_commands.is_empty() => {
                    let result: CommandResult = result;
                    exec_ids.remove(&result.command_id);
                    send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                }
                _ = heartbeat.tick() => {
                    send(&mut outgoing, &AgentServerMessage::Heartbeat { sent_at: Utc::now() }).await?;
                    let candidates = self.runtime.automatic_startup_candidate_ids();
                    let validation_ids = self.runtime.startup_validation_ids();
                    if !validation_ids.is_empty() {
                        match self.validate_startup_agents(validation_ids).await {
                            Ok(active) => {
                                let active: Vec<_> = active.into_iter().filter(|id| candidates.contains(id)).collect();
                                for (agent_id, result) in self.runtime.restore_startup_agents(&active).await {
                                    match result {
                                        Ok(_) => info!(%agent_id, "restored deferred Agent startup command"),
                                        Err(error) => warn!(%agent_id, code = %error.code, "workload recovery failed"),
                                    }
                                }
                            }
                            Err(error) => warn!(error = %format!("{error:#}"), retry_seconds = HEARTBEAT_INTERVAL.as_secs(), "Agent startup validation failed; restore will retry"),
                        }
                    }
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
                _ = self.continue_signal.notified() => {
                    let last = self
                        .last_activity
                        .lock()
                        .map(|value| *value)
                        .unwrap_or_else(|_| Instant::now());
                    if should_reconnect_after_continue(last, Instant::now()) {
                        warn!("SIGCONT with a stale proxy socket; reconnecting");
                        return Ok(ConnectionDisposition::Reconnect);
                    }
                }
                message = incoming.next() => {
                    let Some(message) = message else {
                        return Err(anyhow!("proxy closed the websocket"));
                    };
                    let message = message.context("failed to read proxy websocket")?;
                    self.touch_activity();
                    let Message::Text(text) = message else {
                        match message {
                            Message::Ping(payload) => {
                                outgoing.send(Message::Pong(payload)).await?;
                                continue;
                            }
                            Message::Pong(_) => continue,
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
                    let message = match decode_proxy_message(&text)? {
                        DecodedProxyMessage::Known(message) => *message,
                        DecodedProxyMessage::Unknown { message_type } => {
                            warn!(%message_type, "ignoring unsupported proxy message");
                            continue;
                        }
                        DecodedProxyMessage::UnsupportedCommand { command_id, action } => {
                            let result = CommandResult::failure(
                                command_id,
                                ProtocolError::new(
                                    "unsupported_command",
                                    format!("Controller does not support command {action}"),
                                ),
                            );
                            send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                            continue;
                        }
                    };
                    match message {
                        ProxyMessage::Registered { protocol, .. } => {
                            if !(PROTOCOL_MIN_VERSION..=PROTOCOL_VERSION).contains(&protocol) {
                                return Err(anyhow!(
                                    "proxy selected unsupported protocol {protocol}; Controller supports {PROTOCOL_MIN_VERSION}..={PROTOCOL_VERSION}"
                                ));
                            }
                            if let Err(error) = self.runtime.reset_virtual_hosts() {
                                warn!(code = %error.code, message = %error.message, "failed to reset virtual-host snapshot");
                            }
                        }
                        ProxyMessage::VirtualNetworkHosts { snapshot } => {
                            let revision = snapshot.revision;
                            let count = snapshot.hosts.len();
                            match self.runtime.replace_virtual_hosts(snapshot) {
                                Ok(true) => {
                                    info!(revision, count, "virtual hosts refreshed");
                                }
                                Ok(false) => tracing::debug!(revision, "ignored stale virtual-host snapshot"),
                                Err(error) => warn!(code = %error.code, message = %error.message, "rejected virtual-host snapshot"),
                            }
                        }
                        ProxyMessage::Error { error } => {
                            warn!(code = %error.code, message = %error.message, "proxy rejected a message");
                            if let Some(disposition) = connection_error_disposition(&error.code) {
                                self.mark_proxy_link(
                                    false,
                                    Some(&error.code),
                                    Some(&error.message),
                                );
                                return Ok(disposition);
                            }
                        }
                        ProxyMessage::Command { envelope } => {
                            // Exec children may call Treer themselves. Awaiting them here
                            // prevents this socket from receiving their nested requests.
                            // Keep a bounded, connection-owned set: disconnect drops the
                            // futures and kill_on_drop terminates their child processes.
                            // Update queries also perform bounded image probes, so must
                            // not block heartbeat/GET delivery. Dispatch explicitly owns
                            // a detached Controller task; dropping this response future
                            // cannot cancel an accepted native submission.
                            if matches!(
                                envelope.command,
                                AgentCommand::Exec { .. }
                                    | AgentCommand::ProviderCliCheck { .. }
                                    | AgentCommand::ControllerUpdateInspect {}
                                    | AgentCommand::ControllerUpdateDispatch { .. }
                                    | AgentCommand::ControllerUpdateOperation { .. }
                            ) {
                                if exec_ids.contains(&envelope.command_id) {
                                    continue;
                                }
                                if exec_commands.len() >= 16 {
                                    let result = CommandResult::failure(envelope.command_id, ProtocolError::new("machine_exec_busy", "Controller already has 16 pending exec commands"));
                                    send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                                } else {
                                    exec_ids.insert(envelope.command_id.clone());
                                    exec_commands.push(self.execute(envelope));
                                }
                                continue;
                            }
                            let result = self.execute(envelope).await;
                            send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                        }
                        ProxyMessage::TerminalAttach { session_id, agent_id, cols, rows, cursor } => {
                            let operation_id = format!("{session_id}:attach");
                            match self.runtime.resize(&operation_id, &agent_id, cols, rows).await {
                                Ok(()) => {
                                    match self.runtime.terminal_snapshot(&agent_id, cursor.as_ref()).await {
                                        Ok(snapshot) => {
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

    async fn validate_startup_agents(&self, agent_ids: Vec<String>) -> anyhow::Result<Vec<String>> {
        let mut url = self.proxy_ws.clone();
        url.set_scheme(match url.scheme() {
            "ws" => "http",
            "wss" => "https",
            scheme => return Err(anyhow!("unsupported Proxy websocket scheme {scheme}")),
        })
        .map_err(|_| anyhow!("invalid Proxy websocket URL"))?;
        url.set_path(&format!(
            "/agent/workspaces/{}/machines/{}/startup/validate",
            self.server.workspace_id, self.server.server_id
        ));
        url.set_query(None);
        let response = validate_startup_batches(
            &reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()?,
            url,
            self.machine_token.as_deref(),
            &agent_ids,
        )
        .await?;
        self.runtime
            .apply_stopped_startups(&response.stopped_agent_ids)
            .await
            .map_err(|error| anyhow!("{}: {}", error.code, error.message))?;
        // Permanent deletion confirmed by the Proxy tombstone: remove private
        // state now or on a later pass; the directory is the pending record.
        for (agent_id, result) in self
            .runtime
            .purge_deleted_private_state(&response.deleted_agent_ids)
            .await
        {
            match result {
                Ok(true) => info!(%agent_id, "removed private state of a deleted Agent"),
                Ok(false) => {}
                Err(error) => warn!(%agent_id, code = %error.code, "private state purge deferred"),
            }
        }
        let mut active_agent_ids = response.active_agent_ids;
        for startup in response.app_startups {
            if let Err(error) = self
                .runtime
                .update_app_startup(
                    &format!("app-reconcile:{}:{}", startup.agent_id, startup.revision),
                    &startup.agent_id,
                    startup.request,
                    startup.revision,
                )
                .await
            {
                // One broken App must not defer every other workload forever.
                // Do not fall back to its old command; retry on the next pass.
                active_agent_ids.retain(|id| id != &startup.agent_id);
                warn!(agent_id = %startup.agent_id, code = %error.code, "App startup reconciliation failed; will retry");
            }
        }
        Ok(active_agent_ids
            .into_iter()
            .filter(|id| !response.stopped_agent_ids.contains(id))
            .collect())
    }

    async fn execute(&self, envelope: CommandEnvelope) -> CommandResult {
        if envelope.workspace_id != self.server.workspace_id {
            return CommandResult::failure(
                envelope.command_id,
                ProtocolError::new("workspace_mismatch", "command targets another workspace"),
            );
        }
        // The Controller's own capability set is the authority (#166): a
        // command this Controller does not advertise is refused before any
        // handler runs, whatever a Proxy's cached view says.
        if !command_permitted(&self.runtime.advertised_capabilities(), &envelope.command) {
            let required = envelope.command.required_capability().unwrap_or("unknown");
            return CommandResult::failure(
                envelope.command_id,
                ProtocolError::new(
                    "unsupported_command",
                    format!("this Controller does not advertise capability {required}"),
                ),
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
            AgentCommand::AppEnsureLifecycle {
                agent_id,
                workload_credential,
                request,
                enabled,
                start_missing,
                revision,
            } => self
                .runtime
                .ensure_app_lifecycle(
                    &command_id,
                    agent_id,
                    workload_credential,
                    request,
                    enabled,
                    start_missing,
                    Some(revision),
                )
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::EnsureLifecycle {
                agent_id,
                workload_credential,
                request,
                enabled,
                start_missing,
            } => self
                .runtime
                .ensure_lifecycle(
                    &command_id,
                    agent_id,
                    workload_credential,
                    request,
                    enabled,
                    start_missing,
                )
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Start { agent_id } => self
                .runtime
                .start(&command_id, &agent_id, false)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::Restart { agent_id } => self
                .runtime
                .start(&command_id, &agent_id, true)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::CreationPlacementCheck {} => {
                CommandResult::success(command_id.clone(), serde_json::json!({"supported":true}))
            }
            AgentCommand::FenceAbsentAgentReservation { agent_id } => self
                .runtime
                .fence_absent_agent_reservation(&agent_id)
                .await
                .map(|()| {
                    CommandResult::success(command_id.clone(), serde_json::json!({"fenced":true}))
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
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
            AgentCommand::SubmitPrompt {
                agent_id,
                request,
                sender,
            } => self
                .runtime
                .submit_prompt_delivery(&command_id, &agent_id, request, sender)
                .await
                .map(|data| CommandResult::success(command_id.clone(), data))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::InterfaceStatus { agent_id } => self
                .runtime
                .interface_status(&agent_id)
                .await
                .map(|data| CommandResult::success(command_id.clone(), data))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::QueueRead { agent_id } => self
                .runtime
                .prompt_queue_request(&agent_id, None, false)
                .await
                .map(|data| CommandResult::success(command_id.clone(), data))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::SubmissionRead {
                agent_id,
                submission_id,
            } => self
                .runtime
                .prompt_queue_request(&agent_id, Some(&submission_id), false)
                .await
                .map(|data| CommandResult::success(command_id.clone(), data))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::QueueCancel {
                agent_id,
                submission_id,
            } => self
                .runtime
                .prompt_queue_request(&agent_id, Some(&submission_id), true)
                .await
                .map(|data| CommandResult::success(command_id.clone(), data))
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
            AgentCommand::Abort { agent_id } => self
                .runtime
                .abort(&command_id, &agent_id)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
            AgentCommand::RestoreRuntime {} => {
                match self
                    .validate_startup_agents(self.runtime.startup_candidate_ids())
                    .await
                {
                    Ok(active) => CommandResult::success(
                        command_id.clone(),
                        self.runtime.restore_runtime(&active).await,
                    ),
                    Err(error) => CommandResult::failure(
                        command_id.clone(),
                        ProtocolError::new("restore_waiting_for_proxy", error.to_string()),
                    ),
                }
            }
            AgentCommand::StartupAdminGet { agent_id } => self
                .runtime
                .get_admin_startup(&agent_id)
                .map(|spec| CommandResult::success(command_id.clone(), spec))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::AppStartupCheck { agent_id, request } => self
                .runtime
                .check_app_startup(&agent_id, request)
                .await
                .map(|()| {
                    CommandResult::success(command_id.clone(), serde_json::json!({"valid": true}))
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::AppStartupUpdate {
                agent_id,
                request,
                revision,
            } => self
                .runtime
                .update_app_startup(&command_id, &agent_id, request, revision)
                .await
                .map(|spec| CommandResult::success(command_id.clone(), spec))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::StartupAdminSet { agent_id, request } => self
                .runtime
                .replace_startup(&agent_id, request)
                .await
                .map(|spec| CommandResult::success(command_id.clone(), spec))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::RestartWithStartup { agent_id, request } => self
                .runtime
                .restart_with_startup(&command_id, &agent_id, request)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::StartupSet { agent_id, request } => self
                .runtime
                .set_startup(&agent_id, request)
                .await
                .map(|spec| CommandResult::success(command_id.clone(), spec))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::StartupGet { agent_id } => self
                .runtime
                .get_startup(&agent_id)
                .map(|spec| CommandResult::success(command_id.clone(), spec))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::StartupClear { agent_id } => self
                .runtime
                .clear_startup(&agent_id)
                .map(|cleared| {
                    CommandResult::success(
                        command_id.clone(),
                        serde_json::json!({ "agent_id": agent_id, "cleared": cleared }),
                    )
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
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
            AgentCommand::FetchServiceHttp {
                policy_schema_version: _,
                port,
                path,
                max_bytes,
                timeout_ms,
            } => fetch_service_json(port, &path, max_bytes, timeout_ms)
                .await
                .map(|value| CommandResult::success(command_id.clone(), value))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::Exec { request } => self
                .runtime
                .exec_machine(request)
                .await
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            // The typed web path must not fall back to Exec or the legacy
            // updater before native whole-worker/launcher acceptance is available.
            AgentCommand::ControllerUpdateInspect {} => {
                crate::controller_update::inspect(&self.server.workspace_id, &self.server.server_id)
                    .await
                    .map(|report| CommandResult::success(command_id.clone(), report))
                    .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error))
            }
            AgentCommand::ControllerUpdateDispatch { binding } => {
                crate::controller_update::dispatch(
                    &self.server.workspace_id,
                    &self.server.server_id,
                    binding,
                )
                .await
                .map(|operation| CommandResult::success(command_id.clone(), operation))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error))
            }
            AgentCommand::ControllerUpdateOperation { operation_id } => {
                #[cfg(unix)]
                {
                    let workspace = self.server.workspace_id.clone();
                    let server = self.server.server_id.clone();
                    match tokio::task::spawn_blocking(move || {
                        crate::self_update::web_adapter::operation(
                            &workspace,
                            &server,
                            &operation_id,
                        )
                    })
                    .await
                    {
                        Ok(Ok(operation)) => CommandResult::success(command_id.clone(), operation),
                        _ => CommandResult::failure(
                            command_id.clone(),
                            ProtocolError::new(
                                "controller_update_receipt_invalid",
                                "Native update evidence is unavailable",
                            ),
                        ),
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = operation_id;
                    CommandResult::failure(
                        command_id.clone(),
                        ProtocolError::new(
                            "controller_update_unsupported",
                            "Native Controller update adapter is unavailable",
                        ),
                    )
                }
            }
            AgentCommand::ProviderCliRecipes {} => self
                .runtime
                .provider_cli()
                .recipes()
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::ProviderCliCheck { recipe } => self
                .runtime
                .provider_cli()
                .check(&recipe)
                .await
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::ProviderCliInstall {
                operation_id,
                recipe,
                table_hash,
                argv_hash,
                consent,
            } => self
                .runtime
                .provider_cli()
                .install(&operation_id, &recipe, &table_hash, &argv_hash, &consent)
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::ProviderCliOperation { operation_id } => self
                .runtime
                .provider_cli()
                .operation(&operation_id)
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::UploadBegin {
                upload_id,
                directory,
                file_name,
                overwrite,
                size,
            } => self
                .runtime
                .begin_upload(&upload_id, &directory, &file_name, overwrite, size)
                .map(|()| {
                    CommandResult::success(
                        command_id.clone(),
                        serde_json::json!({ "accepted": true }),
                    )
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::FileTransfer {
                actor,
                authorized_path,
                request,
            } => self
                .runtime
                .transfer_machine_file(actor, authorized_path, request)
                .await
                .map(|value| CommandResult::success(command_id.clone(), value))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::FileTransferInfo {} => CommandResult::success(
                command_id.clone(),
                self.runtime.machine_file_transfer_info(),
            ),
            AgentCommand::UploadChunk {
                upload_id,
                content_base64,
                offset,
            } => self
                .runtime
                .append_upload_chunk(&upload_id, offset, &content_base64)
                .map(|bytes_written| {
                    CommandResult::success(
                        command_id.clone(),
                        serde_json::json!({ "bytes_written": bytes_written }),
                    )
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::UploadPartCommit {
                upload_id,
                offset,
                length,
                sha256,
            } => self
                .runtime
                .commit_upload_part(&upload_id, offset, length, &sha256)
                .map(|value| CommandResult::success(command_id.clone(), value))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::UploadResult { upload_id } => self
                .runtime
                .upload_result(&upload_id)
                .map(|value| CommandResult::success(command_id.clone(), value))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::UploadCommit { upload_id } => self
                .runtime
                .commit_upload(&upload_id)
                .map(|response| CommandResult::success(command_id.clone(), response))
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
            AgentCommand::UploadAbort { upload_id } => self
                .runtime
                .abort_upload(&upload_id)
                .map(|aborted| {
                    CommandResult::success(
                        command_id.clone(),
                        serde_json::json!({ "aborted": aborted }),
                    )
                })
                .unwrap_or_else(|error| CommandResult::failure(command_id.clone(), error)),
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

async fn fetch_service_json(
    port: u16,
    path: &str,
    max_bytes: u32,
    timeout_ms: u64,
) -> Result<Value, ProtocolError> {
    if port == 0
        || !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 2_048
        || path.contains('#')
    {
        return Err(ProtocolError::new(
            "invalid_service_http_request",
            "service HTTP fetch requires a bounded origin-relative path",
        ));
    }
    if max_bytes == 0 || max_bytes > 1024 * 1024 || !(100..=10_000).contains(&timeout_ms) {
        return Err(ProtocolError::new(
            "invalid_service_http_request",
            "service HTTP fetch limits are invalid",
        ));
    }
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::builder()
        .no_proxy()
        // A service must not turn this loopback-only fetch into a request to
        // another port or host.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|error| ProtocolError::new("service_http_failed", error.to_string()))?;
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| ProtocolError::new("service_http_unavailable", error.to_string()))?;
    if !response.status().is_success() {
        return Err(ProtocolError::new(
            "service_http_status",
            format!("service returned HTTP {}", response.status()),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > u64::from(max_bytes))
    {
        return Err(ProtocolError::new(
            "service_http_response_too_large",
            "service JSON response exceeds its declared limit",
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ProtocolError::new("service_http_failed", error.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes as usize {
            return Err(ProtocolError::new(
                "service_http_response_too_large",
                "service JSON response exceeds its declared limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|error| ProtocolError::new("invalid_service_json", error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionDisposition {
    Reconnect,
    Stop,
}

async fn validate_startup_batches(
    client: &reqwest::Client,
    url: Url,
    machine_token: Option<&str>,
    agent_ids: &[String],
) -> anyhow::Result<ValidateAgentStartupResponse> {
    let mut validated = ValidateAgentStartupResponse {
        app_startups: vec![],
        active_agent_ids: vec![],
        stopped_agent_ids: vec![],
        deleted_agent_ids: vec![],
    };
    for batch in agent_ids.chunks(256) {
        let mut request = client.post(url.clone()).json(&ValidateAgentStartupRequest {
            agent_ids: batch.to_vec(),
        });
        if let Some(token) = machine_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .context("startup validation request failed")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("startup validation returned {status}: {body}"));
        }
        let response = response
            .json::<ValidateAgentStartupResponse>()
            .await
            .context("invalid startup validation response")?;
        validated.active_agent_ids.extend(response.active_agent_ids);
        validated.app_startups.extend(response.app_startups);
        validated
            .stopped_agent_ids
            .extend(response.stopped_agent_ids);
        validated
            .deleted_agent_ids
            .extend(response.deleted_agent_ids);
    }
    Ok(validated)
}

fn connection_error_disposition(code: &str) -> Option<ConnectionDisposition> {
    match code {
        "duplicate_machine_connection" | "stale_connection" => {
            Some(ConnectionDisposition::Reconnect)
        }
        "machine_revoked" | "server_deleted" => Some(ConnectionDisposition::Stop),
        _ => None,
    }
}

fn should_reconnect_after_continue(last_activity: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_activity) >= PROXY_DEAD_INTERVAL
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
    supervision: Option<treer_protocol::MachineSupervision>,
) -> ServerInfo {
    let now = Utc::now();
    let labels = std::collections::BTreeMap::from([
        ("os".to_string(), std::env::consts::OS.to_string()),
        ("arch".to_string(), std::env::consts::ARCH.to_string()),
        ("treer.network".to_string(), "1".to_string()),
        ("treer.shutdown".to_string(), "1".to_string()),
    ]);
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
        supervision,
        labels,
        available_agents: Some(available_agents),
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
        capabilities: vec![],
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

/// Whether `command` may run on a Controller that advertises exactly
/// `advertised`: commands without a required capability always may; others
/// only when their capability is advertised.
pub(crate) fn command_permitted(advertised: &[String], command: &AgentCommand) -> bool {
    match command.required_capability() {
        None => true,
        Some(required) => advertised.iter().any(|value| value == required),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn exec_child_nested_startup_request_is_serviced_before_exec_finishes() {
        use crate::controller::ControllerConfig;
        use crate::host_client::HostClient;
        use treer_host_protocol::{HostBuildInfo, HostResponse};

        #[cfg(unix)]
        let base = std::path::PathBuf::from("/tmp");
        #[cfg(windows)]
        let base = std::env::temp_dir();
        let root = base.join(format!("trr-nested-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        #[cfg(unix)]
        let endpoint = root.join("host.sock");
        #[cfg(windows)]
        let endpoint = treer_platform::endpoint_name(
            &uuid::Uuid::new_v4().to_string(),
            &treer_platform::current_user_sid().unwrap(),
        );
        let mut listener = treer_platform::LocalListener::bind(&endpoint).unwrap();
        let host_task = tokio::spawn(async move {
            let _stream = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let (host, events) = HostClient::connect(&endpoint).await.unwrap();
        let (runtime, _) = ControllerRuntime::from_sync(
            host,
            HostResponse::Synced {
                host_epoch: "fixture".into(),
                host_build: HostBuildInfo {
                    version: "fixture".into(),
                    git_commit: "fixture".into(),
                },
                processes: vec![],
                replay: vec![],
            },
            events,
            ControllerConfig {
                root: root.clone(),
                protected_paths: vec![],
                workspace_id: "fixture".into(),
                server_id: "machine".into(),
                agent_server_url: "http://127.0.0.1:1".into(),
                proxy_public_url: "https://proxy.invalid/".into(),
                treer_binary: None,
                interface_cache_path: root.join("interface.json"),
                startup_store_path: root.join("startup.json"),
                private_state_root: root.join("agent-private"),
                provider_cli_root: root.join("provider-cli"),
            },
        )
        .unwrap();

        // The actual exec child blocks on this HTTP reply. Only the nested
        // startup result on the SAME Proxy socket releases it: no sleeps/race.
        let child_started = Arc::new(Notify::new());
        let release_child = Arc::new(Notify::new());
        let child_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let http = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let url = format!("http://{}/", http.local_addr().unwrap());
        let started = child_started.clone();
        let release = release_child.clone();
        let requests = child_requests.clone();
        let http_task = tokio::spawn(async move {
            axum::serve(
                http,
                Router::new().route(
                    "/",
                    get(move || {
                        let started = started.clone();
                        let release = release.clone();
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            started.notify_one();
                            release.notified().await;
                            "complete"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let ws = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let server = server_info(
            "machine".into(),
            "fixture".into(),
            "fixture".into(),
            root.to_string_lossy().into_owned(),
            treer_protocol::BuildInfo {
                version: "fixture".into(),
                git_commit: "fixture".into(),
            },
            vec![],
            None,
        );
        let client = ProxyClient::new(
            Url::parse(&format!("ws://{}/", ws.local_addr().unwrap())).unwrap(),
            None,
            server,
            runtime,
            NetworkRuntime::new(),
        );
        let proxy_task = tokio::spawn(async move {
            let (stream, _) = ws.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            loop {
                let message = socket.next().await.unwrap().unwrap();
                if let Message::Text(text) = message {
                    if matches!(
                        serde_json::from_str::<AgentServerMessage>(&text).unwrap(),
                        AgentServerMessage::Snapshot { .. }
                    ) {
                        break;
                    }
                }
            }
            #[cfg(unix)]
            let curl = "/usr/bin/curl";
            #[cfg(windows)]
            let curl = "curl.exe";
            let exec = ProxyMessage::Command {
                envelope: CommandEnvelope {
                    command_id: "outer-exec".into(),
                    workspace_id: "fixture".into(),
                    command: AgentCommand::Exec {
                        request: treer_protocol::MachineExecRequest {
                            cwd: ".".into(),
                            command: curl.into(),
                            args: vec!["--silent".into(), "--noproxy".into(), "*".into(), url],
                            timeout_ms: 10_000,
                        },
                    },
                },
            };
            socket
                .send(Message::Text(serde_json::to_string(&exec).unwrap().into()))
                .await
                .unwrap();
            child_started.notified().await;
            // A replay while exec is pending shares the original execution.
            socket
                .send(Message::Text(serde_json::to_string(&exec).unwrap().into()))
                .await
                .unwrap();
            let nested = ProxyMessage::Command {
                envelope: CommandEnvelope {
                    command_id: "nested-startup".into(),
                    workspace_id: "fixture".into(),
                    command: AgentCommand::StartupAdminGet {
                        agent_id: "missing".into(),
                    },
                },
            };
            socket
                .send(Message::Text(
                    serde_json::to_string(&nested).unwrap().into(),
                ))
                .await
                .unwrap();
            loop {
                if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                    if let AgentServerMessage::CommandResult { result } =
                        serde_json::from_str(&text).unwrap()
                    {
                        assert_eq!(
                            result.command_id, "nested-startup",
                            "nested request must finish while exec is still blocked"
                        );
                        assert!(result.error.is_some());
                        break;
                    }
                }
            }
            release_child.notify_one();
            loop {
                if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                    if let AgentServerMessage::CommandResult { result } =
                        serde_json::from_str(&text).unwrap()
                    {
                        assert_eq!(result.command_id, "outer-exec");
                        assert!(result.error.is_none());
                        assert_eq!(result.data.unwrap()["exit_code"], 0);
                        break;
                    }
                }
            }
            // After completion, replay gets the cached result, never a new child.
            socket
                .send(Message::Text(serde_json::to_string(&exec).unwrap().into()))
                .await
                .unwrap();
            loop {
                if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                    if let AgentServerMessage::CommandResult { result } =
                        serde_json::from_str(&text).unwrap()
                    {
                        assert_eq!(result.command_id, "outer-exec");
                        assert!(result.error.is_none());
                        assert_eq!(result.data.unwrap()["exit_code"], 0);
                        break;
                    }
                }
            }
            assert_eq!(child_requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        });
        let connection = tokio::spawn(async move { client.run_connection().await });
        let outcome = tokio::time::timeout(Duration::from_secs(5), proxy_task).await;
        connection.abort();
        http_task.abort();
        host_task.abort();
        let _ = connection.await;
        let _ = http_task.await;
        let _ = host_task.await;
        outcome
            .expect("Proxy stopped receiving while exec was pending")
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn recovery_deleted_declaration_and_unavailable_proxy_never_spawn_or_reregister() {
        use crate::controller::private_state_tests::{create, fixture};
        use crate::startup_store::StartupStore;
        use axum::{http::StatusCode, routing::post};
        use std::sync::atomic::{AtomicBool, Ordering};

        let fixture = fixture().await;
        let config = || fixture.config("srv_a", fixture.state.join("agent-private"));
        let runtime = fixture.runtime(config()).await.unwrap();
        for id in ["ag_deleted", "ag_keep"] {
            create(&runtime, &fixture, id, "repo").await;
            runtime.stop(&format!("stop-{id}"), id).await.unwrap();
        }
        // Model a Controller that was offline when the Proxy deleted an
        // enabled declaration. The replacement reads the original disk state.
        let store = StartupStore::load(fixture.root.join("startup.json")).unwrap();
        for id in ["ag_deleted", "ag_keep"] {
            store
                .update(id, |entry| {
                    entry.spec.enabled = true;
                    // Newer explicit intent than the old successful exit.
                    entry.spec.generation += 1;
                })
                .unwrap();
        }
        let replacement = fixture.runtime(config()).await.unwrap();
        let unavailable = Arc::new(AtomicBool::new(true));
        let failing = unavailable.clone();
        let app = Router::new().route(
            "/agent/workspaces/workspace/machines/srv_a/startup/validate",
            post(move |Json(request): Json<ValidateAgentStartupRequest>| {
                let failing = failing.clone();
                async move {
                    assert!(request.agent_ids.contains(&"ag_deleted".into()));
                    if failing.load(Ordering::SeqCst) {
                        return Err(StatusCode::SERVICE_UNAVAILABLE);
                    }
                    Ok(Json(ValidateAgentStartupResponse {
                        active_agent_ids: vec!["ag_keep".into()],
                        stopped_agent_ids: vec![],
                        deleted_agent_ids: vec!["ag_deleted".into()],
                        app_startups: vec![],
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let make_client = |runtime| {
            ProxyClient::new(
                Url::parse(&format!("ws://{address}/")).unwrap(),
                None,
                server_info(
                    "srv_a".into(),
                    "workspace".into(),
                    "fixture".into(),
                    fixture.root.to_string_lossy().into_owned(),
                    treer_protocol::BuildInfo {
                        version: "test".into(),
                        git_commit: "test".into(),
                    },
                    vec![],
                    None,
                ),
                runtime,
                NetworkRuntime::new(),
            )
        };
        let client = make_client(replacement.clone());
        let restore = |id: &str| CommandEnvelope {
            command_id: id.into(),
            workspace_id: "workspace".into(),
            command: AgentCommand::RestoreRuntime {},
        };
        let failed = client.execute(restore("unavailable")).await;
        assert_eq!(failed.error.unwrap().code, "restore_waiting_for_proxy");
        assert_eq!(fixture.spawns.lock().unwrap().len(), 2);
        assert!(replacement.get_startup("ag_deleted").is_ok());

        unavailable.store(false, Ordering::SeqCst);
        let restored = client.execute(restore("confirmed-deleted")).await;
        assert!(restored.error.is_none(), "{restored:?}");
        assert_eq!(fixture.spawns.lock().unwrap().len(), 3);
        assert!(!replacement
            .list()
            .iter()
            .any(|agent| agent.agent_id == "ag_deleted"));
        assert!(replacement.get_startup("ag_deleted").is_err());
        // Another replacement cannot materialize or register the removed ID.
        let next = fixture.runtime(config()).await.unwrap();
        assert!(!next
            .list()
            .iter()
            .any(|agent| agent.agent_id == "ag_deleted"));
        assert!(next.get_startup("ag_deleted").is_err());
        assert!(!next.startup_candidate_ids().contains(&"ag_deleted".into()));
        http.abort();
    }

    #[tokio::test]
    async fn recovery_validation_batches_at_proxy_boundary_and_preserves_stopped_ids() {
        use axum::{routing::post, Json, Router};
        let sizes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = sizes.clone();
        let app = Router::new().route(
            "/validate",
            post(
                move |headers: axum::http::HeaderMap,
                      Json(request): Json<ValidateAgentStartupRequest>| {
                    let observed = observed.clone();
                    async move {
                        assert_eq!(
                            headers.get("authorization").unwrap(),
                            "Bearer machine-secret"
                        );
                        assert!(request.agent_ids.len() <= 256);
                        observed.lock().unwrap().push(request.agent_ids.len());
                        let (stopped_agent_ids, active_agent_ids) = request
                            .agent_ids
                            .into_iter()
                            .partition(|id| id == "agent-256");
                        Json(ValidateAgentStartupResponse {
                            app_startups: vec![],
                            active_agent_ids,
                            stopped_agent_ids,
                            deleted_agent_ids: vec![],
                        })
                    }
                },
            ),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/validate",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        for (count, expected) in [
            (0, vec![]),
            (256, vec![256]),
            (257, vec![256, 1]),
            (513, vec![256, 256, 1]),
        ] {
            sizes.lock().unwrap().clear();
            let ids: Vec<_> = (0..count).map(|index| format!("agent-{index}")).collect();
            let response = validate_startup_batches(
                &reqwest::Client::new(),
                url.clone(),
                Some("machine-secret"),
                &ids,
            )
            .await
            .unwrap();
            assert_eq!(*sizes.lock().unwrap(), expected);
            assert_eq!(
                response.active_agent_ids.len() + response.stopped_agent_ids.len(),
                count
            );
            assert_eq!(
                response.stopped_agent_ids,
                if count > 256 {
                    vec!["agent-256".to_string()]
                } else {
                    vec![]
                }
            );
        }
        server.abort();
    }

    #[test]
    fn unknown_proxy_messages_are_ignored_without_dropping_the_connection() {
        let decoded = decode_proxy_message(r#"{"type":"future_control","payload":{}}"#)
            .expect("decode unknown message");
        assert!(matches!(
            decoded,
            DecodedProxyMessage::Unknown { message_type } if message_type == "future_control"
        ));
    }

    #[test]
    fn unknown_commands_return_a_correlated_unsupported_result() {
        let decoded = decode_proxy_message(
            r#"{"type":"command","envelope":{"command_id":"cmd_future","workspace_id":"default","command":{"action":"future_action","value":1}}}"#,
        )
        .expect("decode future command");
        assert!(matches!(
            decoded,
            DecodedProxyMessage::UnsupportedCommand { command_id, action }
                if command_id == "cmd_future" && action == "future_action"
        ));
    }

    #[test]
    fn known_commands_keep_the_existing_wire_shape() {
        let decoded = decode_proxy_message(
            r#"{"type":"command","envelope":{"command_id":"cmd_stop","workspace_id":"default","command":{"action":"stop","agent_id":"ag_1"}}}"#,
        )
        .expect("decode stop command");
        let DecodedProxyMessage::Known(message) = decoded else {
            panic!("expected known command");
        };
        assert!(matches!(
            *message,
            ProxyMessage::Command {
                envelope: CommandEnvelope {
                    command_id,
                    command: AgentCommand::Stop { agent_id },
                    ..
                }
            } if command_id == "cmd_stop" && agent_id == "ag_1"
        ));
    }

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
            Some(treer_protocol::MachineSupervision {
                mode: treer_protocol::MachineSupervisionMode::SystemdUser,
                fallback_reason: None,
            }),
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
        assert_eq!(
            server.supervision.map(|value| value.mode),
            Some(treer_protocol::MachineSupervisionMode::SystemdUser)
        );
    }

    #[test]
    fn duplicate_and_stale_connections_reconnect_until_revoked() {
        assert_eq!(
            connection_error_disposition("stale_connection"),
            Some(ConnectionDisposition::Reconnect)
        );
        assert_eq!(
            connection_error_disposition("duplicate_machine_connection"),
            Some(ConnectionDisposition::Reconnect)
        );
        assert_eq!(
            connection_error_disposition("machine_revoked"),
            Some(ConnectionDisposition::Stop)
        );
        assert_eq!(
            connection_error_disposition("server_deleted"),
            Some(ConnectionDisposition::Stop)
        );
    }

    #[test]
    fn sleep_wake_reconnects_when_the_socket_is_past_the_dead_interval() {
        let now = Instant::now();
        assert!(!should_reconnect_after_continue(now, now));
        assert!(should_reconnect_after_continue(
            now,
            now + PROXY_DEAD_INTERVAL
        ));
        assert!(!should_reconnect_after_continue(
            now,
            now + PROXY_DEAD_INTERVAL - Duration::from_secs(1)
        ));
    }

    #[tokio::test]
    async fn network_probe_runs_from_the_controller_network() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind service");
        let port = listener.local_addr().expect("service address").port();
        let runtime = NetworkRuntime::new();
        let healthy = runtime
            .probe(Ipv4Addr::LOCALHOST.to_string(), port, None, 500)
            .await;
        assert_eq!(healthy["healthy"], true);
    }

    #[tokio::test]
    async fn service_http_fetch_is_loopback_bounded_json() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind policy service");
        let port = listener.local_addr().expect("service address").port();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/manifest",
                    get(|| async { Json(serde_json::json!({"protocol": "test/v1"})) }),
                ),
            )
            .await
            .expect("serve policy fixture");
        });
        let value = fetch_service_json(port, "/v1/manifest", 4_096, 1_000)
            .await
            .expect("fetch JSON");
        assert_eq!(value["protocol"], "test/v1");
        let error = fetch_service_json(port, "//external.test/path", 4_096, 1_000)
            .await
            .expect_err("reject authority-relative path");
        assert_eq!(error.code, "invalid_service_http_request");
    }

    #[tokio::test]
    async fn service_http_fetch_never_follows_redirects() {
        let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind redirect target");
        let target_port = target.local_addr().expect("target address").port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            axum::serve(
                target,
                Router::new().route(
                    "/secret",
                    get(move || {
                        let counted = counted.clone();
                        async move {
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            Json(serde_json::json!({"leaked": true}))
                        }
                    }),
                ),
            )
            .await
            .expect("serve target");
        });
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind redirecting service");
        let port = listener.local_addr().expect("service address").port();
        let location = format!("http://127.0.0.1:{target_port}/secret");
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/manifest",
                    get(move || {
                        let location = location.clone();
                        async move { axum::response::Redirect::temporary(&location) }
                    }),
                ),
            )
            .await
            .expect("serve redirecting service");
        });
        let error = fetch_service_json(port, "/v1/manifest", 4_096, 1_000)
            .await
            .expect_err("redirect must not be followed");
        assert_eq!(error.code, "service_http_status");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
