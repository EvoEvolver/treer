use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use treer_protocol::{
    AgentCommand, AgentInfo, AgentServerSnapshot, CommandEnvelope, CommandResult,
    NetworkBinaryFrame, NetworkBinaryKind, ProtocolError, ProxyMessage, ServerInfo, ServerStatus,
    TerminalBinaryFrame, TerminalBinaryKind, TerminalServerMessage, TransferBinaryFrame,
    TransferServerMessage, TransferStats, WorkspaceEvent, WorkspaceInfo, WorkspaceSnapshot,
};
use uuid::Uuid;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketFrame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

pub struct ShellOptions {
    pub cwd: String,
    pub command: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

pub struct TransferOptions {
    pub path: String,
    pub recursive: bool,
    pub direction: TransferDirection,
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
    transfer_sessions: Mutex<HashMap<String, TransferSession>>,
    network_streams: Mutex<HashMap<String, NetworkStream>>,
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
    process_id: String,
    transient: bool,
    outgoing: mpsc::UnboundedSender<SocketFrame>,
    last_revision: Option<u64>,
}

struct TransferSession {
    workspace_id: String,
    server_id: String,
    direction: TransferDirection,
    outgoing: mpsc::Sender<SocketFrame>,
}

struct NetworkStream {
    workspace_id: String,
    source_server_id: String,
    destination_server_id: String,
    source_closed: bool,
    destination_closed: bool,
}

struct WorkspaceState {
    info: WorkspaceInfo,
    revision: u64,
    servers: HashMap<String, ServerInfo>,
    agents: HashMap<String, AgentInfo>,
    deleted_servers: HashSet<String>,
    deleted_agents: HashSet<String>,
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
                transfer_sessions: Mutex::new(HashMap::new()),
                network_streams: Mutex::new(HashMap::new()),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceEvent> {
        self.inner.events.subscribe()
    }

    pub async fn ensure_workspace(&self, workspace_id: &str, name: &str) -> WorkspaceInfo {
        self.ensure_workspace_info(WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            created_at: Utc::now(),
        })
        .await
    }

    pub async fn ensure_workspace_info(&self, info: WorkspaceInfo) -> WorkspaceInfo {
        let mut workspaces = self.inner.workspaces.write().await;
        workspaces
            .entry(info.workspace_id.clone())
            .or_insert_with(|| WorkspaceState {
                info,
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
                deleted_servers: HashSet::new(),
                deleted_agents: HashSet::new(),
            })
            .info
            .clone()
    }

    pub async fn create_workspace_info(
        &self,
        info: WorkspaceInfo,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        let mut workspaces = self.inner.workspaces.write().await;
        if workspaces.contains_key(&info.workspace_id) {
            return Err(ProtocolError::new(
                "workspace_exists",
                format!("workspace {} already exists", info.workspace_id),
            ));
        }
        workspaces.insert(
            info.workspace_id.clone(),
            WorkspaceState {
                info: info.clone(),
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
                deleted_servers: HashSet::new(),
                deleted_agents: HashSet::new(),
            },
        );
        Ok(info)
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
        if self
            .inner
            .workspaces
            .read()
            .await
            .get(&server.workspace_id)
            .is_some_and(|workspace| workspace.deleted_servers.contains(&server.server_id))
        {
            return Err(ProtocolError::new(
                "server_deleted",
                format!("server {} was deleted", server.server_id),
            ));
        }
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
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", &workspace_id))?;
            if workspace.deleted_servers.contains(&server_id) {
                return Err(ProtocolError::new("server_deleted", &server_id));
            }
            if let Some(current) = workspace.servers.get(&server_id) {
                server.name.clone_from(&current.name);
            }
            workspace.servers.insert(server_id.clone(), server.clone());
            let names: HashMap<_, _> = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| (agent.agent_id.clone(), agent.name.clone()))
                .collect();
            workspace
                .agents
                .retain(|_, agent| agent.server_id != server_id);
            for mut agent in agents {
                if agent.workspace_id == snapshot_workspace_id && agent.server_id == server_id {
                    if workspace.deleted_agents.contains(&agent.agent_id) {
                        continue;
                    }
                    if let Some(name) = names.get(&agent.agent_id) {
                        agent.name.clone_from(name);
                    }
                    workspace.agents.insert(agent.agent_id.clone(), agent);
                }
            }
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.clone(),
                event: "server.snapshot".to_string(),
                data: serde_json::to_value(&server).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            }
        };
        let _ = self.inner.events.send(event);
        self.resend_pending(&workspace_id, &server_id).await;
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
        mut agent: AgentInfo,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(&agent.workspace_id, &agent.server_id, connection_id)
            .await?;
        let workspace_id = agent.workspace_id.clone();
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", &workspace_id))?;
            if workspace.deleted_servers.contains(&agent.server_id) {
                return Err(ProtocolError::new("server_deleted", &agent.server_id));
            }
            if workspace.deleted_agents.contains(&agent.agent_id) {
                return Ok(());
            }
            if let Some(current) = workspace.agents.get(&agent.agent_id) {
                agent.name.clone_from(&current.name);
            }
            workspace
                .agents
                .insert(agent.agent_id.clone(), agent.clone());
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id,
                event: "agent.updated".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            }
        };
        let _ = self.inner.events.send(event);
        Ok(())
    }

    pub async fn restore_deleted_agents(
        &self,
        workspace_id: &str,
        agent_ids: impl IntoIterator<Item = String>,
    ) -> Result<(), ProtocolError> {
        let mut workspaces = self.inner.workspaces.write().await;
        let workspace = workspaces
            .get_mut(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        for agent_id in agent_ids {
            workspace.agents.remove(&agent_id);
            workspace.deleted_agents.insert(agent_id);
        }
        Ok(())
    }

    pub async fn delete_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
    ) -> Result<AgentInfo, ProtocolError> {
        let (agent, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let agent = workspace
                .agents
                .remove(agent_id)
                .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?;
            workspace.deleted_agents.insert(agent_id.to_string());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "agent.deleted".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (agent, event)
        };
        let _ = self.inner.events.send(event);
        self.close_agent_terminals(workspace_id, agent_id).await;
        Ok(agent)
    }

    pub async fn resolve_server(
        &self,
        workspace_id: &str,
        target: &str,
    ) -> Result<ServerInfo, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        if let Some(server) = workspace.servers.get(target) {
            return Ok(server.clone());
        }
        let mut matches = workspace
            .servers
            .values()
            .filter(|server| server.name == target);
        let Some(server) = matches.next() else {
            return Err(ProtocolError::new("server_not_found", target));
        };
        if matches.next().is_some() {
            return Err(ProtocolError::new(
                "server_ambiguous",
                format!("more than one machine is named {target}; use a server id"),
            ));
        }
        Ok(server.clone())
    }

    pub async fn rename_server(
        &self,
        workspace_id: &str,
        server_id: &str,
        name: String,
    ) -> Result<ServerInfo, ProtocolError> {
        let (server, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let server = workspace
                .servers
                .get_mut(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            server.name = name;
            let server = server.clone();
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.renamed".to_string(),
                data: serde_json::to_value(&server).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (server, event)
        };
        let _ = self.inner.events.send(event);
        Ok(server)
    }

    pub async fn delete_server(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<(ServerInfo, Vec<AgentInfo>), ProtocolError> {
        let (server, agents, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let server = workspace
                .servers
                .remove(server_id)
                .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
            workspace.deleted_servers.insert(server_id.to_string());
            let agent_ids = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| agent.agent_id.clone())
                .collect::<Vec<_>>();
            let agents = agent_ids
                .iter()
                .filter_map(|agent_id| workspace.agents.remove(agent_id))
                .collect::<Vec<_>>();
            workspace.deleted_agents.extend(agent_ids.iter().cloned());
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.deleted".to_string(),
                data: serde_json::json!({
                    "server": server,
                    "agent_ids": agent_ids,
                }),
            };
            (server, agents, event)
        };
        let _ = self.inner.events.send(event);

        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        if let Some(connection) = self.inner.connections.write().await.remove(&key) {
            let _ = connection.outgoing.send(SocketFrame::Close);
        }

        let cancelled = {
            let mut pending = self.inner.pending.lock().await;
            let command_ids = pending
                .iter()
                .filter(|(_, command)| command.server == key)
                .map(|(command_id, _)| command_id.clone())
                .collect::<Vec<_>>();
            command_ids
                .into_iter()
                .filter_map(|command_id| {
                    pending
                        .remove(&command_id)
                        .map(|command| (command_id, command))
                })
                .collect::<Vec<_>>()
        };
        for (command_id, command) in cancelled {
            let _ = command.result.send(CommandResult::failure(
                command_id,
                ProtocolError::new("server_deleted", server_id),
            ));
        }

        let terminals = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let session_ids = sessions
                .iter()
                .filter(|(_, session)| {
                    session.workspace_id == workspace_id && session.server_id == server_id
                })
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            session_ids
                .into_iter()
                .filter_map(|session_id| sessions.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for terminal in terminals {
            send_terminal_to_browser(
                &terminal.outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some("machine deleted".to_string()),
                    exit_code: None,
                },
            );
        }
        self.close_server_transfers(workspace_id, server_id, "machine deleted")
            .await;
        self.close_server_network_streams(workspace_id, server_id)
            .await;

        Ok((server, agents))
    }

    pub async fn rename_agent(
        &self,
        workspace_id: &str,
        agent_id: &str,
        name: String,
    ) -> Result<AgentInfo, ProtocolError> {
        let (agent, event) = {
            let mut workspaces = self.inner.workspaces.write().await;
            let workspace = workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
            let agent = workspace
                .agents
                .get_mut(agent_id)
                .ok_or_else(|| ProtocolError::new("agent_not_found", agent_id))?;
            agent.name = name;
            agent.updated_at = Utc::now();
            let agent = agent.clone();
            workspace.revision = workspace.revision.saturating_add(1);
            let event = WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "agent.renamed".to_string(),
                data: serde_json::to_value(&agent).map_err(|error| {
                    ProtocolError::new("encode_error", format!("failed to encode event: {error}"))
                })?,
            };
            (agent, event)
        };
        let _ = self.inner.events.send(event);
        Ok(agent)
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
                process_id: agent.agent_id.clone(),
                transient: false,
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

    pub async fn attach_shell(
        &self,
        workspace_id: &str,
        server_target: &str,
        options: ShellOptions,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<String, ProtocolError> {
        let server = self.resolve_server(workspace_id, server_target).await?;
        if server.status != ServerStatus::Online {
            return Err(ProtocolError::new("server_offline", server.server_id));
        }
        if server.labels.get("treer.ssh").map(String::as_str) != Some("1") {
            return Err(ProtocolError::new(
                "ssh_unsupported",
                "target machine must update treer-agent-server before it can accept remote shells",
            ));
        }
        let server_outgoing = self
            .server_outgoing(workspace_id, &server.server_id)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", &server.server_id))?;
        let session_id = format!("ssh_{}", Uuid::new_v4().simple());
        self.inner.terminal_sessions.lock().await.insert(
            session_id.clone(),
            TerminalSession {
                workspace_id: workspace_id.to_string(),
                server_id: server.server_id,
                process_id: session_id.clone(),
                transient: true,
                outgoing,
                last_revision: None,
            },
        );
        let message = ProxyMessage::ShellOpen {
            session_id: session_id.clone(),
            cols: options.cols.max(1),
            rows: options.rows.max(1),
            cwd: options.cwd,
            command: options.command,
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

    pub async fn attach_transfer(
        &self,
        workspace_id: &str,
        server_target: &str,
        options: TransferOptions,
        outgoing: mpsc::Sender<SocketFrame>,
    ) -> Result<String, ProtocolError> {
        let server = self.resolve_server(workspace_id, server_target).await?;
        if server.status != ServerStatus::Online {
            return Err(ProtocolError::new("server_offline", server.server_id));
        }
        if server.labels.get("treer.scp").map(String::as_str) != Some("1") {
            return Err(ProtocolError::new(
                "scp_unsupported",
                "target machine must update treer-agent-server before it can transfer files",
            ));
        }
        let server_outgoing = self
            .server_outgoing(workspace_id, &server.server_id)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", &server.server_id))?;
        let session_id = format!("copy_{}", Uuid::new_v4().simple());
        self.inner.transfer_sessions.lock().await.insert(
            session_id.clone(),
            TransferSession {
                workspace_id: workspace_id.to_string(),
                server_id: server.server_id,
                direction: options.direction,
                outgoing,
            },
        );
        let message = match options.direction {
            TransferDirection::Upload => ProxyMessage::TransferUpload {
                session_id: session_id.clone(),
                destination: options.path,
                recursive: options.recursive,
            },
            TransferDirection::Download => ProxyMessage::TransferDownload {
                session_id: session_id.clone(),
                source: options.path,
                recursive: options.recursive,
            },
        };
        if let Err(error) = send_proxy_message(&server_outgoing, &message) {
            self.inner
                .transfer_sessions
                .lock()
                .await
                .remove(&session_id);
            return Err(error);
        }
        Ok(session_id)
    }

    pub async fn transfer_input(
        &self,
        session_id: &str,
        encoded: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let frame = TransferBinaryFrame::decode(&encoded)?;
        if frame.session_id != session_id {
            return Err(ProtocolError::new(
                "transfer_identity_mismatch",
                "transfer frame belongs to another session",
            ));
        }
        let (workspace_id, server_id, direction) = self
            .inner
            .transfer_sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| {
                (
                    session.workspace_id.clone(),
                    session.server_id.clone(),
                    session.direction,
                )
            })
            .ok_or_else(|| ProtocolError::new("transfer_not_found", session_id))?;
        if direction != TransferDirection::Upload {
            return Err(ProtocolError::new(
                "invalid_transfer_direction",
                "download sessions do not accept client data",
            ));
        }
        self.server_outgoing(&workspace_id, &server_id)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?
            .send(SocketFrame::Binary(encoded))
            .map_err(|_| ProtocolError::new("server_offline", "agent server disconnected"))
    }

    pub async fn detach_transfer(&self, session_id: &str) {
        let session = self.inner.transfer_sessions.lock().await.remove(session_id);
        let Some(session) = session else { return };
        if let Some(outgoing) = self
            .server_outgoing(&session.workspace_id, &session.server_id)
            .await
        {
            let _ = send_proxy_message(
                &outgoing,
                &ProxyMessage::TransferCancel {
                    session_id: session_id.to_string(),
                },
            );
        }
    }

    pub async fn transfer_ready(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_transfer(
            workspace_id,
            server_id,
            session_id,
            TransferServerMessage::Ready {
                session_id: session_id.to_string(),
            },
            false,
        )
        .await
    }

    pub async fn transfer_progress(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_transfer(
            workspace_id,
            server_id,
            session_id,
            TransferServerMessage::Progress {
                session_id: session_id.to_string(),
            },
            false,
        )
        .await
    }

    pub async fn transfer_output(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        frame: TransferBinaryFrame,
        encoded: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        let outgoing = {
            let sessions = self.inner.transfer_sessions.lock().await;
            let Some(session) = sessions.get(&frame.session_id) else {
                return Ok(());
            };
            if session.workspace_id != workspace_id || session.server_id != server_id {
                return Err(ProtocolError::new(
                    "transfer_identity_mismatch",
                    &frame.session_id,
                ));
            }
            if session.direction != TransferDirection::Download {
                return Err(ProtocolError::new(
                    "invalid_transfer_direction",
                    "upload sessions do not accept server data",
                ));
            }
            session.outgoing.clone()
        };
        outgoing
            .send(SocketFrame::Binary(encoded))
            .await
            .map_err(|_| ProtocolError::new("transfer_cancelled", "transfer client disconnected"))
    }

    pub async fn transfer_complete(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        stats: TransferStats,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_transfer(
            workspace_id,
            server_id,
            session_id,
            TransferServerMessage::Complete {
                session_id: session_id.to_string(),
                stats,
            },
            true,
        )
        .await
    }

    pub async fn transfer_failed(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        error: ProtocolError,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_transfer(
            workspace_id,
            server_id,
            session_id,
            TransferServerMessage::Error { error },
            true,
        )
        .await
    }

    async fn relay_transfer(
        &self,
        workspace_id: &str,
        server_id: &str,
        session_id: &str,
        message: TransferServerMessage,
        close: bool,
    ) -> Result<(), ProtocolError> {
        let outgoing = {
            let mut sessions = self.inner.transfer_sessions.lock().await;
            let Some(session) = sessions.get(session_id) else {
                return Ok(());
            };
            if session.workspace_id != workspace_id || session.server_id != server_id {
                return Err(ProtocolError::new("transfer_identity_mismatch", session_id));
            }
            let outgoing = session.outgoing.clone();
            if close {
                sessions.remove(session_id);
            }
            outgoing
        };
        send_transfer_to_client(&outgoing, &message).await
    }

    async fn close_agent_terminals(&self, workspace_id: &str, agent_id: &str) {
        let sessions = self
            .inner
            .terminal_sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| {
                session.workspace_id == workspace_id && session.process_id == agent_id
            })
            .map(|(session_id, session)| (session_id.clone(), session.outgoing.clone()))
            .collect::<Vec<_>>();
        for (session_id, outgoing) in sessions {
            send_terminal_to_browser(
                &outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some("agent deleted".to_string()),
                    exit_code: None,
                },
            );
            self.detach_terminal(&session_id).await;
        }
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
            let message = if session.transient {
                ProxyMessage::ShellDetach {
                    session_id: session_id.to_string(),
                }
            } else {
                ProxyMessage::TerminalDetach {
                    session_id: session_id.to_string(),
                }
            };
            let _ = send_proxy_message(&outgoing, &message);
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
        exit_code: Option<i32>,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        self.relay_terminal(
            workspace_id,
            server_id,
            session_id,
            TerminalServerMessage::Closed { reason, exit_code },
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
                    exit_code: None,
                },
            );
        }
        self.close_server_transfers(workspace_id, server_id, "agent server disconnected")
            .await;
        self.close_server_network_streams(workspace_id, server_id)
            .await;
    }

    pub async fn open_network_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        connection_id: Uuid,
        destination_server_id: &str,
        frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, source_server_id, connection_id)
            .await?;
        if frame.kind != NetworkBinaryKind::Open {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "new network stream must begin with an open frame",
            ));
        }
        let outgoing = self
            .server_outgoing(workspace_id, destination_server_id)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", destination_server_id))?;
        {
            let mut streams = self.inner.network_streams.lock().await;
            if streams.contains_key(&frame.stream_id) {
                return Err(ProtocolError::new(
                    "network_stream_exists",
                    "network stream ID is already in use",
                ));
            }
            streams.insert(
                frame.stream_id.clone(),
                NetworkStream {
                    workspace_id: workspace_id.to_string(),
                    source_server_id: source_server_id.to_string(),
                    destination_server_id: destination_server_id.to_string(),
                    source_closed: false,
                    destination_closed: false,
                },
            );
        }
        let encoded = frame.encode()?;
        if outgoing.send(SocketFrame::Binary(encoded)).is_err() {
            self.inner
                .network_streams
                .lock()
                .await
                .remove(&frame.stream_id);
            return Err(ProtocolError::new("server_offline", destination_server_id));
        }
        Ok(())
    }

    pub async fn relay_network_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        if frame.kind == NetworkBinaryKind::Open {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "open frame cannot be relayed as an existing stream",
            ));
        }
        let (peer, remove) = {
            let mut streams = self.inner.network_streams.lock().await;
            let stream = streams
                .get_mut(&frame.stream_id)
                .ok_or_else(|| ProtocolError::new("network_stream_not_found", &frame.stream_id))?;
            if stream.workspace_id != workspace_id {
                return Err(ProtocolError::new(
                    "network_stream_identity_mismatch",
                    &frame.stream_id,
                ));
            }
            let from_source = stream.source_server_id == server_id;
            let from_destination = stream.destination_server_id == server_id;
            if !from_source && !from_destination {
                return Err(ProtocolError::new(
                    "network_stream_identity_mismatch",
                    &frame.stream_id,
                ));
            }
            if frame.kind == NetworkBinaryKind::Opened && !from_destination {
                return Err(ProtocolError::new(
                    "invalid_network_frame",
                    "only the destination can open a network stream",
                ));
            }
            if frame.kind == NetworkBinaryKind::HalfClose {
                if from_source {
                    stream.source_closed = true;
                } else {
                    stream.destination_closed = true;
                }
            }
            let remove = frame.kind == NetworkBinaryKind::Reset
                || (stream.source_closed && stream.destination_closed);
            let peer = if from_source {
                stream.destination_server_id.clone()
            } else {
                stream.source_server_id.clone()
            };
            if remove {
                streams.remove(&frame.stream_id);
            }
            (peer, remove)
        };
        let outgoing = self
            .server_outgoing(workspace_id, &peer)
            .await
            .ok_or_else(|| ProtocolError::new("server_offline", &peer))?;
        if outgoing.send(SocketFrame::Binary(frame.encode()?)).is_err() && !remove {
            self.inner
                .network_streams
                .lock()
                .await
                .remove(&frame.stream_id);
            return Err(ProtocolError::new("server_offline", peer));
        }
        Ok(())
    }

    async fn close_server_network_streams(&self, workspace_id: &str, server_id: &str) {
        let routes = {
            let mut streams = self.inner.network_streams.lock().await;
            let ids = streams
                .iter()
                .filter(|(_, stream)| {
                    stream.workspace_id == workspace_id
                        && (stream.source_server_id == server_id
                            || stream.destination_server_id == server_id)
                })
                .map(|(stream_id, _)| stream_id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|stream_id| {
                    streams.remove(&stream_id).map(|stream| {
                        let peer = if stream.source_server_id == server_id {
                            stream.destination_server_id
                        } else {
                            stream.source_server_id
                        };
                        (stream_id, peer)
                    })
                })
                .collect::<Vec<_>>()
        };
        let payload = serde_json::to_vec(&ProtocolError::new(
            "server_offline",
            "network peer disconnected",
        ))
        .unwrap_or_default();
        for (stream_id, peer) in routes {
            let Some(outgoing) = self.server_outgoing(workspace_id, &peer).await else {
                continue;
            };
            let frame = NetworkBinaryFrame {
                kind: NetworkBinaryKind::Reset,
                stream_id,
                payload: payload.clone(),
            };
            if let Ok(encoded) = frame.encode() {
                let _ = outgoing.send(SocketFrame::Binary(encoded));
            }
        }
    }

    async fn close_server_transfers(&self, workspace_id: &str, server_id: &str, reason: &str) {
        let disconnected = {
            let mut sessions = self.inner.transfer_sessions.lock().await;
            let session_ids = sessions
                .iter()
                .filter(|(_, session)| {
                    session.workspace_id == workspace_id && session.server_id == server_id
                })
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            session_ids
                .into_iter()
                .filter_map(|session_id| sessions.remove(&session_id))
                .collect::<Vec<_>>()
        };
        for session in disconnected {
            let _ = send_transfer_to_client(
                &session.outgoing,
                &TransferServerMessage::Error {
                    error: ProtocolError::new("server_offline", reason),
                },
            )
            .await;
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

async fn send_transfer_to_client(
    outgoing: &mpsc::Sender<SocketFrame>,
    message: &TransferServerMessage,
) -> Result<(), ProtocolError> {
    let encoded = serde_json::to_string(message)
        .map_err(|error| ProtocolError::new("encode_error", error.to_string()))?;
    outgoing
        .send(SocketFrame::Text(encoded))
        .await
        .map_err(|_| ProtocolError::new("transfer_cancelled", "transfer client disconnected"))
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
            SocketFrame::Close => panic!("expected text socket frame"),
        }
    }

    fn test_server() -> ServerInfo {
        let now = Utc::now();
        ServerInfo {
            server_id: "server".to_string(),
            workspace_id: "alpha".to_string(),
            name: "test-host".to_string(),
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
    async fn remote_shells_route_by_machine_name_and_detach_transiently() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        let mut server = test_server();
        server
            .labels
            .insert("treer.ssh".to_string(), "1".to_string());
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server, Uuid::new_v4(), server_tx)
            .await
            .expect("register server");
        let (terminal_tx, _terminal_rx) = mpsc::unbounded_channel();

        let session_id = state
            .attach_shell(
                "alpha",
                "test-host",
                ShellOptions {
                    cwd: "src".to_string(),
                    command: Some("pwd".to_string()),
                    cols: 100,
                    rows: 40,
                },
                terminal_tx,
            )
            .await
            .expect("attach shell");
        let open: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("shell open message"),
        ))
        .expect("decode shell open");
        assert_eq!(
            open,
            ProxyMessage::ShellOpen {
                session_id: session_id.clone(),
                cols: 100,
                rows: 40,
                cwd: "src".to_string(),
                command: Some("pwd".to_string()),
            }
        );

        state.detach_terminal(&session_id).await;
        let detach: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("shell detach message"),
        ))
        .expect("decode shell detach");
        assert_eq!(detach, ProxyMessage::ShellDetach { session_id });
    }

    #[tokio::test]
    async fn old_agent_servers_reject_remote_shells_without_disconnect() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        let (server_tx, _server_rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), Uuid::new_v4(), server_tx)
            .await
            .expect("register server");
        let (terminal_tx, _terminal_rx) = mpsc::unbounded_channel();

        let error = state
            .attach_shell(
                "alpha",
                "server",
                ShellOptions {
                    cwd: ".".to_string(),
                    command: None,
                    cols: 120,
                    rows: 36,
                },
                terminal_tx,
            )
            .await
            .expect_err("old server must not receive new wire messages");
        assert_eq!(error.code, "ssh_unsupported");
    }

    #[tokio::test]
    async fn file_uploads_route_binary_frames_and_completion() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        let mut server = test_server();
        server
            .labels
            .insert("treer.scp".to_string(), "1".to_string());
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server, connection_id, server_tx)
            .await
            .expect("register server");
        let (client_tx, mut client_rx) = mpsc::channel(16);
        let session_id = state
            .attach_transfer(
                "alpha",
                "test-host",
                TransferOptions {
                    path: "dest.bin".to_string(),
                    recursive: false,
                    direction: TransferDirection::Upload,
                },
                client_tx,
            )
            .await
            .expect("attach transfer");
        let open: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("transfer open message"),
        ))
        .expect("decode transfer open");
        assert_eq!(
            open,
            ProxyMessage::TransferUpload {
                session_id: session_id.clone(),
                destination: "dest.bin".to_string(),
                recursive: false,
            }
        );

        state
            .transfer_ready("alpha", "server", connection_id, &session_id)
            .await
            .expect("transfer ready");
        let ready: TransferServerMessage =
            serde_json::from_str(&expect_text(client_rx.recv().await.expect("ready message")))
                .expect("decode ready");
        assert_eq!(
            ready,
            TransferServerMessage::Ready {
                session_id: session_id.clone()
            }
        );

        state
            .transfer_progress("alpha", "server", connection_id, &session_id)
            .await
            .expect("transfer progress");
        let progress: TransferServerMessage = serde_json::from_str(&expect_text(
            client_rx.recv().await.expect("progress message"),
        ))
        .expect("decode progress");
        assert_eq!(
            progress,
            TransferServerMessage::Progress {
                session_id: session_id.clone()
            }
        );

        let encoded = TransferBinaryFrame {
            kind: treer_protocol::TransferBinaryKind::Data,
            session_id: session_id.clone(),
            payload: vec![0, 0xff],
        }
        .encode()
        .expect("encode transfer data");
        state
            .transfer_input(&session_id, encoded.clone())
            .await
            .expect("route transfer data");
        assert_eq!(server_rx.recv().await, Some(SocketFrame::Binary(encoded)));

        let stats = TransferStats {
            entries: 1,
            bytes: 2,
        };
        state
            .transfer_complete("alpha", "server", connection_id, &session_id, stats)
            .await
            .expect("complete transfer");
        let complete: TransferServerMessage = serde_json::from_str(&expect_text(
            client_rx.recv().await.expect("complete message"),
        ))
        .expect("decode complete");
        assert_eq!(
            complete,
            TransferServerMessage::Complete { session_id, stats }
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
    async fn renamed_objects_survive_controller_snapshots_and_events() {
        let state = AppState::new();
        let server = test_server();
        let connection_id = Uuid::new_v4();
        let (outgoing, _messages) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, outgoing)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server: server.clone(),
                    agents: vec![test_agent("agent-1", "original-agent")],
                },
            )
            .await
            .expect("initial snapshot");

        state
            .rename_server("alpha", "server", "renamed-machine".to_string())
            .await
            .expect("rename server");
        state
            .rename_agent("alpha", "agent-1", "renamed-agent".to_string())
            .await
            .expect("rename agent");

        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server,
                    agents: vec![test_agent("agent-1", "original-agent")],
                },
            )
            .await
            .expect("replacement snapshot");
        state
            .apply_agent_event(connection_id, test_agent("agent-1", "original-agent"))
            .await
            .expect("agent event");

        let snapshot = state.snapshot("alpha").await.expect("workspace snapshot");
        assert_eq!(snapshot.servers[0].name, "renamed-machine");
        assert_eq!(snapshot.agents[0].name, "renamed-agent");
    }

    #[tokio::test]
    async fn deleted_agents_ignore_controller_snapshots_and_events() {
        let state = AppState::new();
        let server = test_server();
        let connection_id = Uuid::new_v4();
        let (outgoing, _messages) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, outgoing)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server: server.clone(),
                    agents: vec![test_agent("agent-1", "helper")],
                },
            )
            .await
            .expect("initial snapshot");
        state
            .delete_agent("alpha", "agent-1")
            .await
            .expect("delete agent");

        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server,
                    agents: vec![test_agent("agent-1", "helper")],
                },
            )
            .await
            .expect("replacement snapshot");
        state
            .apply_agent_event(connection_id, test_agent("agent-1", "helper"))
            .await
            .expect("late agent event");

        assert!(state
            .snapshot("alpha")
            .await
            .expect("workspace snapshot")
            .agents
            .is_empty());
        assert_eq!(
            state
                .resolve_agent("alpha", "agent-1")
                .await
                .expect_err("deleted agent must stay hidden")
                .code,
            "agent_not_found"
        );
    }

    #[tokio::test]
    async fn deleting_server_closes_resources_and_blocks_late_reconnects() {
        let state = AppState::new();
        let server = test_server();
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, server_tx)
            .await
            .expect("register server");
        state
            .apply_snapshot(
                connection_id,
                AgentServerSnapshot {
                    server: server.clone(),
                    agents: vec![test_agent("agent-1", "helper")],
                },
            )
            .await
            .expect("apply snapshot");

        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        state
            .attach_terminal("alpha", "agent-1", 120, 40, terminal_tx)
            .await
            .expect("attach terminal");
        let attach = server_rx.recv().await.expect("terminal attach");
        assert!(matches!(attach, SocketFrame::Text(_)));

        let command_state = state.clone();
        let pending = tokio::spawn(async move {
            command_state
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
        let command = server_rx.recv().await.expect("pending command");
        assert!(matches!(command, SocketFrame::Text(_)));

        let (deleted, agents) = state
            .delete_server("alpha", "server")
            .await
            .expect("delete server");
        assert_eq!(deleted.server_id, "server");
        assert_eq!(agents.len(), 1);
        assert_eq!(server_rx.recv().await, Some(SocketFrame::Close));

        let pending_error = pending
            .await
            .expect("join pending command")
            .expect_err("pending command should fail");
        assert_eq!(pending_error.code, "server_deleted");
        let terminal_message: TerminalServerMessage = serde_json::from_str(&expect_text(
            terminal_rx.recv().await.expect("terminal close"),
        ))
        .expect("decode terminal close");
        assert_eq!(
            terminal_message,
            TerminalServerMessage::Closed {
                reason: Some("machine deleted".to_string()),
                exit_code: None,
            }
        );

        let snapshot = state.snapshot("alpha").await.expect("workspace snapshot");
        assert!(snapshot.servers.is_empty());
        assert!(snapshot.agents.is_empty());
        let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
        assert_eq!(
            state
                .register_server(server, Uuid::new_v4(), replacement_tx)
                .await
                .expect_err("deleted server should not reconnect")
                .code,
            "server_deleted"
        );
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
    async fn machine_shutdown_uses_the_confirmed_command_channel() {
        let state = AppState::new();
        let server = test_server();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server, Uuid::new_v4(), server_tx)
            .await
            .expect("register controller");

        let waiting_state = state.clone();
        let waiting = tokio::spawn(async move {
            waiting_state
                .send_command("alpha", "server", AgentCommand::ShutdownMachine)
                .await
        });
        let message: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("shutdown command"),
        ))
        .expect("decode shutdown command");
        let ProxyMessage::Command { envelope } = message else {
            panic!("expected command message");
        };
        assert_eq!(envelope.command, AgentCommand::ShutdownMachine);

        state
            .complete_command(CommandResult::success(
                envelope.command_id,
                serde_json::json!({"accepted": true}),
            ))
            .await;
        assert_eq!(
            waiting
                .await
                .expect("join shutdown command")
                .expect("shutdown accepted"),
            serde_json::json!({"accepted": true})
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
            SocketFrame::Close => panic!("expected binary terminal input"),
        };
        assert_eq!(input.kind, TerminalBinaryKind::Input);
        assert_eq!(input.session_id, session_id);
        assert_eq!(input.payload, vec![0, 0xff, b'\r']);
    }
}
