use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    ProtocolError, ProxyMessage, ServerInfo, ServerStatus, TerminalBinaryFrame, TerminalBinaryKind,
    PROTOCOL_VERSION,
};
use url::Url;

use crate::controller::ControllerRuntime;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const RESULT_CACHE_LIMIT: usize = 256;

struct TerminalRelay {
    agent_id: String,
    last_revision: u64,
}

#[derive(Clone)]
pub struct ProxyClient {
    pub proxy_ws: Url,
    pub machine_token: Option<String>,
    pub server: ServerInfo,
    pub runtime: ControllerRuntime,
    command_cache: Arc<Mutex<HashMap<String, CommandResult>>>,
}

impl ProxyClient {
    pub fn new(
        proxy_ws: Url,
        machine_token: Option<String>,
        server: ServerInfo,
        runtime: ControllerRuntime,
    ) -> Self {
        Self {
            proxy_ws,
            machine_token,
            server,
            runtime,
            command_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run_forever(self) {
        let mut delay = Duration::from_millis(300);
        loop {
            match self.run_connection().await {
                Ok(()) => warn!("proxy connection closed"),
                Err(err) => warn!(%err, "proxy connection failed"),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    }

    async fn run_connection(&self) -> anyhow::Result<()> {
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
                server: self.server.clone(),
            },
        )
        .await?;
        send(
            &mut outgoing,
            &AgentServerMessage::Snapshot {
                snapshot: AgentServerSnapshot {
                    server: self.server.clone(),
                    agents: self.runtime.list(),
                },
            },
        )
        .await?;

        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut events = self.runtime.subscribe();
        let mut terminal_events = self.runtime.subscribe_terminal();
        let mut terminal_sessions = HashMap::<String, TerminalRelay>::new();
        info!(proxy = %self.proxy_ws, "connected to proxy");

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    send(&mut outgoing, &AgentServerMessage::Heartbeat { sent_at: Utc::now() }).await?;
                }
                event = events.recv() => match event {
                    Ok(agent) => send(&mut outgoing, &AgentServerMessage::AgentEvent { agent }).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        send(&mut outgoing, &AgentServerMessage::Snapshot {
                            snapshot: AgentServerSnapshot {
                                server: self.server.clone(),
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
                                if relay.agent_id != event.agent_id || event.revision <= relay.last_revision {
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
                        warn!(count, "terminal output relay lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("terminal event channel closed"));
                    }
                },
                message = incoming.next() => {
                    let Some(message) = message else {
                        return Err(anyhow!("proxy closed the websocket"));
                    };
                    let message = message.context("failed to read proxy websocket")?;
                    let Message::Text(text) = message else {
                        match message {
                            Message::Binary(encoded) => {
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
                                            .write_raw(&operation_id, &relay.agent_id, &frame.payload)
                                            .await
                                            .map(|_| ())
                                            .map_err(|error| anyhow!(error.message))
                                    }
                                    None => Err(anyhow!("unknown terminal session {}", frame.session_id)),
                                };
                                if let Err(error) = result {
                                    terminal_sessions.remove(&frame.session_id);
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id: frame.session_id,
                                        reason: Some(error.to_string()),
                                    }).await?;
                                }
                            }
                            Message::Close(_) => return Ok(()),
                            _ => {}
                        }
                        continue;
                    };
                    let message: ProxyMessage = serde_json::from_str(&text)
                        .context("failed to decode proxy message")?;
                    match message {
                        ProxyMessage::Registered { .. } => {}
                        ProxyMessage::Error { error } => warn!(code = %error.code, message = %error.message, "proxy rejected a message"),
                        ProxyMessage::Command { envelope } => {
                            let result = self.execute(envelope).await;
                            send(&mut outgoing, &AgentServerMessage::CommandResult { result }).await?;
                        }
                        ProxyMessage::TerminalAttach { session_id, agent_id, cols, rows } => {
                            let operation_id = format!("{session_id}:attach");
                            match self.runtime.terminal_snapshot(&agent_id).await {
                                Ok(replay) => {
                                    match self.runtime.resize(&operation_id, &agent_id, cols, rows).await {
                                        Ok(()) => {
                                            terminal_sessions.insert(
                                                session_id.clone(),
                                                TerminalRelay {
                                                    agent_id,
                                                    last_revision: replay.revision,
                                                },
                                            );
                                            send_binary(&mut outgoing, TerminalBinaryFrame {
                                                kind: TerminalBinaryKind::Ready,
                                                session_id,
                                                revision: replay.revision,
                                                payload: replay.data,
                                            }).await?;
                                        }
                                        Err(error) => {
                                            send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                                session_id,
                                                reason: Some(error.message),
                                            }).await?;
                                        }
                                    }
                                }
                                Err(error) => {
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id,
                                        reason: Some(error.message),
                                    }).await?;
                                }
                            }
                        }
                        ProxyMessage::TerminalResize { session_id, cols, rows } => {
                            if let Some(relay) = terminal_sessions.get(&session_id) {
                                let operation_id = format!("{session_id}:resize:{}", uuid::Uuid::new_v4().simple());
                                if let Err(error) = self.runtime.resize(&operation_id, &relay.agent_id, cols, rows).await {
                                    terminal_sessions.remove(&session_id);
                                    send(&mut outgoing, &AgentServerMessage::TerminalClosed {
                                        session_id,
                                        reason: Some(error.message),
                                    }).await?;
                                }
                            }
                        }
                        ProxyMessage::TerminalDetach { session_id } => {
                            terminal_sessions.remove(&session_id);
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
            AgentCommand::Create { agent_id, request } => self
                .runtime
                .create(&command_id, agent_id, request)
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
            AgentCommand::Stop { agent_id } => self
                .runtime
                .stop(&command_id, &agent_id)
                .await
                .map(|agent| CommandResult::success(command_id.clone(), agent))
                .unwrap_or_else(|err| CommandResult::failure(command_id.clone(), err)),
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

pub fn server_info(
    server_id: String,
    workspace_id: String,
    hostname: String,
    root: String,
) -> ServerInfo {
    let now = Utc::now();
    ServerInfo {
        server_id,
        workspace_id,
        hostname,
        root,
        labels: std::collections::BTreeMap::from([
            ("os".to_string(), std::env::consts::OS.to_string()),
            ("arch".to_string(), std::env::consts::ARCH.to_string()),
        ]),
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    }
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
