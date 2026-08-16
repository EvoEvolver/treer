use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use treer_protocol::{
    AgentCommand, AgentInfo, AgentServerSnapshot, CommandEnvelope, CommandResult, ProtocolError,
    ProxyMessage, ServerInfo, ServerStatus, TerminalBinaryFrame, TerminalBinaryKind,
    TerminalServerMessage, WorkspaceEvent, WorkspaceInfo, WorkspaceSnapshot,
};
use uuid::Uuid;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketFrame {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    workspaces: RwLock<HashMap<String, WorkspaceState>>,
    connections: RwLock<HashMap<ServerKey, ServerConnection>>,
    pending: Mutex<HashMap<String, PendingCommand>>,
    terminal_sessions: Mutex<HashMap<String, TerminalSession>>,
    events: broadcast::Sender<WorkspaceEvent>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ServerKey {
    workspace_id: String,
    server_id: String,
}

#[derive(Clone)]
struct ServerConnection {
    connection_id: Uuid,
    outgoing: mpsc::UnboundedSender<SocketFrame>,
}

struct PendingCommand {
    server: ServerKey,
    encoded: String,
    result: oneshot::Sender<CommandResult>,
}

struct TerminalSession {
    workspace_id: String,
    server_id: String,
    outgoing: mpsc::UnboundedSender<SocketFrame>,
    last_revision: Option<u64>,
}

struct WorkspaceState {
    info: WorkspaceInfo,
    revision: u64,
    servers: HashMap<String, ServerInfo>,
    agents: HashMap<String, AgentInfo>,
}

impl WorkspaceState {
    fn snapshot(&self) -> WorkspaceSnapshot {
        let mut servers: Vec<_> = self.servers.values().cloned().collect();
        servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        let mut agents: Vec<_> = self.agents.values().cloned().collect();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        WorkspaceSnapshot {
            revision: self.revision,
            workspace: self.info.clone(),
            servers,
            agents,
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            inner: std::sync::Arc::new(Inner {
                workspaces: RwLock::new(HashMap::new()),
                connections: RwLock::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                terminal_sessions: Mutex::new(HashMap::new()),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceEvent> {
        self.inner.events.subscribe()
    }

    pub async fn ensure_workspace(&self, workspace_id: &str, name: &str) -> WorkspaceInfo {
        let mut workspaces = self.inner.workspaces.write().await;
        workspaces
            .entry(workspace_id.to_string())
            .or_insert_with(|| WorkspaceState {
                info: WorkspaceInfo {
                    workspace_id: workspace_id.to_string(),
                    name: name.to_string(),
                    created_at: Utc::now(),
                },
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
            })
            .info
            .clone()
    }

    pub async fn create_workspace(
        &self,
        workspace_id: String,
        name: String,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        let mut workspaces = self.inner.workspaces.write().await;
        if workspaces.contains_key(&workspace_id) {
            return Err(ProtocolError::new(
                "workspace_exists",
                format!("workspace {workspace_id} already exists"),
            ));
        }
        let info = WorkspaceInfo {
            workspace_id: workspace_id.clone(),
            name,
            created_at: Utc::now(),
        };
        workspaces.insert(
            workspace_id,
            WorkspaceState {
                info: info.clone(),
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
            },
        );
        Ok(info)
    }

    pub async fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut result: Vec<_> = workspaces.values().map(|item| item.info.clone()).collect();
        result.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        result
    }

    pub async fn snapshot(&self, workspace_id: &str) -> Result<WorkspaceSnapshot, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        workspaces
            .get(workspace_id)
            .map(WorkspaceState::snapshot)
            .ok_or_else(|| {
                ProtocolError::new(
                    "workspace_not_found",
                    format!("workspace {workspace_id} does not exist"),
                )
            })
    }

    pub async fn register_server(
        &self,
        mut server: ServerInfo,
        connection_id: Uuid,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<u64, ProtocolError> {
        self.ensure_workspace(&server.workspace_id, &server.workspace_id)
            .await;
        let now = Utc::now();
        server.status = ServerStatus::Online;
        server.connected_at = now;
        server.last_seen_at = now;

        let key = ServerKey {
            workspace_id: server.workspace_id.clone(),
            server_id: server.server_id.clone(),
        };
        self.inner.connections.write().await.insert(
            key,
            ServerConnection {
                connection_id,
                outgoing,
            },
        );

        let event = self
            .mutate_workspace(
                &server.workspace_id,
                "server.updated",
                &server,
                |workspace| {
                    workspace
                        .servers
                        .insert(server.server_id.clone(), server.clone());
                },
            )
            .await?;
        Ok(event.revision)
    }

    pub async fn apply_snapshot(
        &self,
        connection_id: Uuid,
        snapshot: AgentServerSnapshot,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(
            &snapshot.server.workspace_id,
            &snapshot.server.server_id,
            connection_id,
        )
        .await?;
        let mut server = snapshot.server;
        server.status = ServerStatus::Online;
        server.last_seen_at = Utc::now();
        let workspace_id = server.workspace_id.clone();
        let snapshot_workspace_id = workspace_id.clone();
        let server_id = server.server_id.clone();
        let agents = snapshot.agents;
        let event_server = server.clone();
        self.mutate_workspace(
            &workspace_id,
            "server.snapshot",
            &event_server,
            move |workspace| {
                workspace.servers.insert(server_id.clone(), server);
                workspace
                    .agents
                    .retain(|_, agent| agent.server_id != server_id);
                for agent in agents {
                    if agent.workspace_id == snapshot_workspace_id && agent.server_id == server_id {
                        workspace.agents.insert(agent.agent_id.clone(), agent);
                    }
                }
            },
        )
        .await?;
        self.resend_pending(&workspace_id, &event_server.server_id)
            .await;
        Ok(())
    }

    pub async fn heartbeat(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        let mut workspaces = self.inner.workspaces.write().await;
        let workspace = workspaces
            .get_mut(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        let server = workspace
            .servers
            .get_mut(server_id)
            .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
        server.last_seen_at = Utc::now();
        server.status = ServerStatus::Online;
        Ok(())
    }

    pub async fn apply_agent_event(
        &self,
        connection_id: Uuid,
        agent: AgentInfo,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(&agent.workspace_id, &agent.server_id, connection_id)
            .await?;
        let workspace_id = agent.workspace_id.clone();
        let event_agent = agent.clone();
        self.mutate_workspace(
            &workspace_id,
            "agent.updated",
            &event_agent,
            move |workspace| {
                workspace.agents.insert(agent.agent_id.clone(), agent);
            },
        )
        .await?;
        Ok(())
    }

    pub async fn resolve_agent_server(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<String, ProtocolError> {
        Ok(self.resolve_agent(workspace_id, target).await?.server_id)
    }

    pub async fn resolve_agent(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(agent) = workspace.agents.get(target) {
            return Ok(agent.clone());
        }
        let mut matches = workspace
            .agents
            .values()
            .filter(|agent| agent.name == target);
        let Some(agent) = matches.next() else {
            return Err(ProtocolError::new("agent_not_found", target));
        };
        if matches.next().is_some() {
            return Err(ProtocolError::new(
                "agent_ambiguous",
                format!("more than one agent is named {target}; use an agent id"),
            ));
        }
        Ok(agent.clone())
    }

    pub async fn select_server(
        &self,
        workspace_id: &str,
        requested: Option<&str>,
    ) -> Result<String, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(server_id) = requested {
            let server = workspace
                .servers
                .get(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            if server.status != ServerStatus::Online {
                return Err(ProtocolError::new("server_offline", server_id));
            }
            return Ok(server_id.to_string());
        }
        let mut candidates: Vec<_> = workspace
            .servers
            .values()
            .filter(|server| server.status == ServerStatus::Online)
            .map(|server| server.server_id.clone())
            .collect();
        candidates.sort();
        candidates.into_iter().next().ok_or_else(|| {
            ProtocolError::new(
                "no_online_server",
                format!("workspace {workspace_id} has no online agent server"),
            )
        })
    }

    pub async fn send_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let outgoing = self
            .inner
            .connections
            .read()
            .await
            .get(&key)
            .map(|connection| connection.outgoing.clone())
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;

        let command_id = format!("cmd_{}", Uuid::new_v4().simple());
        let envelope = CommandEnvelope {
            command_id: command_id.clone(),
            workspace_id: workspace_id.to_string(),
            command,
        };
        let encoded =
            serde_json::to_string(&ProxyMessage::Command { envelope }).map_err(|err| {
                ProtocolError::new("encode_error", format!("failed to encode command: {err}"))
            })?;
        let (result_tx, result_rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(
            command_id.clone(),
            PendingCommand {
                server: key,
                encoded: encoded.clone(),
                result: result_tx,
            },
        );
        if outgoing.send(SocketFrame::Text(encoded)).is_err() {
            self.inner.pending.lock().await.remove(&command_id);
            return Err(ProtocolError::new("server_offline", server_id));
        }

        let result = match tokio::time::timeout(COMMAND_TIMEOUT, result_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.inner.pending.lock().await.remove(&command_id);
                return Err(ProtocolError::new(
                    "command_cancelled",
                    "agent server disconnected before returning a result",
                ));
            }
            Err(_) => {
                self.inner.pending.lock().await.remove(&command_id);
                return Err(ProtocolError::new(
                    "command_timeout",
                    format!("command {command_id} timed out"),
                ));
            }
        };
        if let Some(error) = result.error {
            Err(error)
        } else {
            Ok(result.data.unwrap_or(Value::Null))
        }
    }

    pub async fn complete_command(&self, result: CommandResult) {
        if let Some(pending) = self.inner.pending.lock().await.remove(&result.command_id) {
            let _ = pending.result.send(result);
        }
    }

    async fn resend_pending(&self, workspace_id: &str, server_id: &str) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let Some(outgoing) = self
            .inner
            .connections
            .read()
            .await
            .get(&key)
            .map(|connection| connection.outgoing.clone())
        else {
            return;
        };
        let commands = self
            .inner
            .pending
            .lock()
            .await
            .values()
            .filter(|pending| pending.server == key)
            .map(|pending| pending.encoded.clone())
            .collect::<Vec<_>>();
        for command in commands {
            let _ = outgoing.send(SocketFrame::Text(command));
        }
    }

    pub async fn attach_terminal(
        &self,
        workspace_id: &str,
        agent_id: &str,
        cols: u16,
        rows: u16,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<String, ProtocolError> {
        let agent = self.resolve_agent(workspace_id, agent_id).await?;
        let server_id = agent.server_id;
        let server_outgoing = self
            .server_outgoing(workspace_id, &server_id)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", &server_id))?;
        let session_id = format!("term_{}", Uuid::new_v4().simple());
        self.inner.terminal_sessions.lock().await.insert(
            session_id.clone(),
            TerminalSession {
                workspace_id: workspace_id.to_string(),
                server_id,
                outgoing,
                last_revision: None,
            },
        );
        let message = ProxyMessage::TerminalAttach {
            session_id: session_id.clone(),
            agent_id: agent.agent_id,
            cols: cols.max(1),
            rows: rows.max(1),
        };
        if let Err(error) = send_proxy_message(&server_outgoing, &message) {
            self.inner
                .terminal_sessions
                .lock()
                .await
                .remove(&session_id);
            return Err(error);
        }
        Ok(session_id)
    }

    pub async fn terminal_input(
        &self,
        session_id: &str,
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let session = self.terminal_session_route(session_id).await?;
        let encoded = TerminalBinaryFrame {
            kind: TerminalBinaryKind::Input,
            session_id: session_id.to_string(),
            revision: 0,
            payload: data,
        }
        .encode()?;
        session
            .send(SocketFrame::Binary(encoded))
            .map_err(|_| ProtocolError::new("server_offline", "agent server disconnected"))
    }

    pub async fn terminal_resize(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), ProtocolError> {
        let session = self.terminal_session_route(session_id).await?;
        send_proxy_message(
            &session,
            &ProxyMessage::TerminalResize {
                session_id: session_id.to_string(),
                cols: cols.max(1),
                rows: rows.max(1),
            },
        )
    }

    pub async fn detach_terminal(&self, session_id: &str) {
        let session = self.inner.terminal_sessions.lock().await.remove(session_id);
        let Some(session) = session else {
            return;
        };
        if let Some(outgoing) = self
            .server_outgoing(&session.workspace_id, &session.server_id)
            .await
        {
            let _ = send_proxy_message(
                &outgoing,
                &ProxyMessage::TerminalDetach {
                    session_id: session_id.to_string(),
                },
            );
        }
    }

    pub async fn terminal_ready(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        revision: u64,
        replay: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        let outgoing = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return Ok(());
            };
            if session.workspace_id != workspace_id || session.server_id != server_id {
                return Err(ProtocolError::new("terminal_identity_mismatch", session_id));
            }
            session.last_revision = Some(revision);
            session.outgoing.clone()
        };
        send_terminal_to_browser(
            &outgoing,
            &TerminalServerMessage::Ready {
                session_id: session_id.to_string(),
            },
        );
        let _ = outgoing.send(SocketFrame::Binary(replay));
        Ok(())
    }

    pub async fn terminal_output(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        revision: u64,
        data: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        let outgoing = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return Ok(());
            };
            if session.workspace_id != workspace_id || session.server_id != server_id {
                return Err(ProtocolError::new("terminal_identity_mismatch", session_id));
            }
            if session
                .last_revision
                .is_some_and(|last_revision| revision <= last_revision)
            {
                return Ok(());
            }
            session.last_revision = Some(revision);
            session.outgoing.clone()
        };
        let _ = outgoing.send(SocketFrame::Binary(data));
        Ok(())
    }

    pub async fn terminal_closed(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        reason: Option<String>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_terminal(
            workspace_id,
            server_id,
            session_id,
            TerminalServerMessage::Closed { reason },
            true,
        )
        .await
    }

    pub async fn disconnect_server(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let removed = {
            let mut connections = self.inner.connections.write().await;
            let is_current = connections
                .get(&key)
                .is_some_and(|connection| connection.connection_id == connection_id);
            is_current
                .then(|| connections.remove(&key))
                .flatten()
                .is_some()
        };
        if !removed {
            return;
        }
        let payload = serde_json::json!({ "server_id": server_id, "status": "offline" });
        let _ = self
            .mutate_workspace(workspace_id, "server.offline", &payload, |workspace| {
                if let Some(server) = workspace.servers.get_mut(server_id) {
                    server.status = ServerStatus::Offline;
                    server.last_seen_at = Utc::now();
                }
            })
            .await;

        let disconnected = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let session_ids: Vec<_> = sessions
                .iter()
                .filter_map(|(session_id, session)| {
                    (session.workspace_id == workspace_id && session.server_id == server_id)
                        .then_some(session_id.clone())
                })
                .collect();
            session_ids
                .into_iter()
                .filter_map(|session_id| sessions.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for session in disconnected {
            send_terminal_to_browser(
                &session.outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some("agent server disconnected".to_string()),
                },
            );
        }
    }

    async fn server_outgoing(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Option<mpsc::UnboundedSender<SocketFrame>> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        self.inner
            .connections
            .read()
            .await
            .get(&key)
            .map(|connection| connection.outgoing.clone())
    }

    async fn terminal_session_route(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedSender<SocketFrame>, ProtocolError> {
        let route = self
            .inner
            .terminal_sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| (session.workspace_id.clone(), session.server_id.clone()))
            .ok_or_else(|| ProtocolError::new("terminal_not_found", session_id))?;
        self.server_outgoing(&route.0, &route.1)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", route.1))
    }

    async fn relay_terminal(
        &self,
        workspace_id: &str,
        server_id: &str,
        session_id: &str,
        message: TerminalServerMessage,
        close: bool,
    ) -> Result<(), ProtocolError> {
        let outgoing = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let Some(session) = sessions.get(session_id) else {
                return Ok(());
            };
            if session.workspace_id != workspace_id || session.server_id != server_id {
                return Err(ProtocolError::new("terminal_identity_mismatch", session_id));
            }
            let outgoing = session.outgoing.clone();
            if close {
                sessions.remove(session_id);
            }
            outgoing
        };
        send_terminal_to_browser(&outgoing, &message);
        Ok(())
    }

    async fn require_current_connection(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
    ) -> Result<(), ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let connections = self.inner.connections.read().await;
        match connections.get(&key) {
            Some(connection) if connection.connection_id == connection_id => Ok(()),
            _ => Err(ProtocolError::new(
                "stale_connection",
                format!("connection for {server_id} is no longer current"),
            )),
        }
    }

    async fn mutate_workspace<T: Serialize>(
        &self,
        workspace_id: &str,
        event_name: &str,
        payload: &T,
        mutation: impl FnOnce(&mut WorkspaceState),
    ) -> Result<WorkspaceEvent, ProtocolError> {
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            mutation(workspace);
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: event_name.to_string(),
                data: serde_json::to_value(payload).map_err(|err| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {err}"))
                })?,
            }
        };
        let _ = self.inner.events.send(event.clone());
        Ok(event)
    }
}

fn send_proxy_message(
    outgoing: &mpsc::UnboundedSender<SocketFrame>,
    message: &ProxyMessage,
) -> Result<(), ProtocolError> {
    let encoded = serde_json::to_string(message).map_err(|error| {
        ProtocolError::new(
            "encode_error",
            format!("failed to encode terminal message: {error}"),
        )
    })?;
    outgoing
        .send(SocketFrame::Text(encoded))
        .map_err(|_| ProtocolError::new("server_offline", "agent server disconnected"))
}

fn send_terminal_to_browser(
    outgoing: &mpsc::UnboundedSender<SocketFrame>,
    message: &TerminalServerMessage,
) {
    if let Ok(encoded) = serde_json::to_string(message) {
        let _ = outgoing.send(SocketFrame::Text(encoded));
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_protocol::AgentStatus;

    fn expect_text(frame: SocketFrame) -> String {
        match frame {
            SocketFrame::Text(text) => text,
            SocketFrame::Binary(_) => panic!("expected text socket frame"),
        }
    }

    fn test_server() -> ServerInfo {
        let now = Utc::now();
        ServerInfo {
            server_id: "server".to_string(),
            workspace_id: "alpha".to_string(),
            hostname: "test-host".to_string(),
            root: "/tmp".to_string(),
            labels: Default::default(),
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        }
    }

    fn test_agent(agent_id: &str, name: &str) -> AgentInfo {
        let now = Utc::now();
        AgentInfo {
            agent_id: agent_id.to_string(),
            workspace_id: "alpha".to_string(),
            server_id: "server".to_string(),
            kind: "command".to_string(),
            name: name.to_string(),
            cwd: ".".to_string(),
            status: AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
        }
    }

    #[tokio::test]
    async fn workspace_snapshots_are_isolated() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        state.ensure_workspace("beta", "Beta").await;

        let alpha = state.snapshot("alpha").await.expect("alpha snapshot");
        let beta = state.snapshot("beta").await.expect("beta snapshot");
        assert_eq!(alpha.workspace.workspace_id, "alpha");
        assert_eq!(beta.workspace.workspace_id, "beta");
        assert!(alpha.servers.is_empty());
        assert!(beta.agents.is_empty());
    }

    #[tokio::test]
    async fn agent_targets_accept_ids_and_unique_names() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        {
            let mut workspaces = state.inner.workspaces.write().await;
            let workspace = workspaces.get_mut("alpha").expect("workspace");
            workspace
                .agents
                .insert("agent-1".to_string(), test_agent("agent-1", "reviewer"));
        }

        assert_eq!(
            state
                .resolve_agent("alpha", "agent-1")
                .await
                .expect("id target")
                .name,
            "reviewer"
        );
        assert_eq!(
            state
                .resolve_agent("alpha", "reviewer")
                .await
                .expect("name target")
                .agent_id,
            "agent-1"
        );
    }

    #[tokio::test]
    async fn duplicate_agent_names_are_ambiguous() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        {
            let mut workspaces = state.inner.workspaces.write().await;
            let workspace = workspaces.get_mut("alpha").expect("workspace");
            workspace
                .agents
                .insert("agent-1".to_string(), test_agent("agent-1", "reviewer"));
            workspace
                .agents
                .insert("agent-2".to_string(), test_agent("agent-2", "reviewer"));
        }

        let error = state
            .resolve_agent("alpha", "reviewer")
            .await
            .expect_err("duplicate names must fail");
        assert_eq!(error.code, "agent_ambiguous");
    }

    #[tokio::test]
    async fn pending_command_is_resent_after_controller_snapshot() {
        let state = AppState::new();
        let server = test_server();
        let first_connection = Uuid::new_v4();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), first_connection, first_tx)
            .await
            .expect("register first controller");

        let waiting_state = state.clone();
        let waiting = tokio::spawn(async move {
            waiting_state
                .send_command(
                    "alpha",
                    "server",
                    AgentCommand::Read {
                        agent_id: "agent-1".to_string(),
                        lines: None,
                    },
                )
                .await
        });
        let first: ProxyMessage = serde_json::from_str(&expect_text(
            first_rx.recv().await.expect("first command should be sent"),
        ))
        .expect("decode first command");
        let ProxyMessage::Command {
            envelope: first_envelope,
        } = first
        else {
            panic!("expected command message");
        };

        state
            .disconnect_server("alpha", "server", first_connection)
            .await;
        let second_connection = Uuid::new_v4();
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), second_connection, second_tx)
            .await
            .expect("register replacement controller");
        state
            .apply_snapshot(
                second_connection,
                AgentServerSnapshot {
                    server,
                    agents: Vec::new(),
                },
            )
            .await
            .expect("apply replacement snapshot");

        let second: ProxyMessage = serde_json::from_str(&expect_text(
            second_rx
                .recv()
                .await
                .expect("pending command should be resent"),
        ))
        .expect("decode resent command");
        let ProxyMessage::Command {
            envelope: second_envelope,
        } = second
        else {
            panic!("expected resent command message");
        };
        assert_eq!(first_envelope.command_id, second_envelope.command_id);

        state
            .complete_command(CommandResult::success(
                second_envelope.command_id,
                serde_json::json!({"replayed": true}),
            ))
            .await;
        assert_eq!(
            waiting
                .await
                .expect("join command task")
                .expect("command result"),
            serde_json::json!({"replayed": true})
        );
    }

    #[tokio::test]
    async fn terminal_routes_raw_binary_and_deduplicates_revisions() {
        let state = AppState::new();
        let server = test_server();
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server, connection_id, server_tx)
            .await
            .expect("register controller");
        {
            let mut workspaces = state.inner.workspaces.write().await;
            workspaces
                .get_mut("alpha")
                .expect("workspace")
                .agents
                .insert("agent-1".to_string(), test_agent("agent-1", "shell"));
        }
        let (browser_tx, mut browser_rx) = mpsc::unbounded_channel();
        let session_id = state
            .attach_terminal("alpha", "agent-1", 120, 40, browser_tx)
            .await
            .expect("attach terminal");
        let attach: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("terminal attach message"),
        ))
        .expect("decode attach");
        assert!(matches!(
            attach,
            ProxyMessage::TerminalAttach { session_id: ref attached, .. } if attached == &session_id
        ));

        state
            .terminal_ready(
                "alpha",
                "server",
                connection_id,
                &session_id,
                7,
                b"replay".to_vec(),
            )
            .await
            .expect("terminal ready");
        let ready: TerminalServerMessage =
            serde_json::from_str(&expect_text(browser_rx.recv().await.expect("ready frame")))
                .expect("decode ready");
        assert_eq!(
            ready,
            TerminalServerMessage::Ready {
                session_id: session_id.clone()
            }
        );
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(b"replay".to_vec()))
        );

        state
            .terminal_output(
                "alpha",
                "server",
                connection_id,
                &session_id,
                7,
                b"duplicate".to_vec(),
            )
            .await
            .expect("ignore duplicate output");
        assert!(browser_rx.try_recv().is_err());
        state
            .terminal_output(
                "alpha",
                "server",
                connection_id,
                &session_id,
                8,
                b"live".to_vec(),
            )
            .await
            .expect("relay live output");
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(b"live".to_vec()))
        );

        state
            .terminal_input(&session_id, vec![0, 0xff, b'\r'])
            .await
            .expect("relay browser input");
        let input = match server_rx.recv().await.expect("terminal input frame") {
            SocketFrame::Binary(encoded) => {
                TerminalBinaryFrame::decode(&encoded).expect("decode terminal input")
            }
            SocketFrame::Text(_) => panic!("expected binary terminal input"),
        };
        assert_eq!(input.kind, TerminalBinaryKind::Input);
        assert_eq!(input.session_id, session_id);
        assert_eq!(input.payload, vec![0, 0xff, b'\r']);
    }
}
