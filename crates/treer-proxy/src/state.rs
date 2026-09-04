use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tracing::warn;
use treer_protocol::{
    AgentCommand, AgentInfo, AgentServerSnapshot, CommandEnvelope, CommandResult, DomainEventActor,
    DomainEventEnvelope, DomainEventResource, MachineTrafficRecord, NetworkBinaryFrame,
    NetworkBinaryKind, NetworkConnectRequest, NetworkDirectTarget, ProtocolError, ProxyMessage,
    ServerInfo, ServerStatus, TerminalBinaryFrame, TerminalBinaryKind, TerminalCursor,
    TerminalServerMessage, WorkspaceEvent, WorkspaceInfo, WorkspaceSnapshot,
    DOMAIN_EVENT_SCHEMA_VERSION,
};
use uuid::Uuid;

use crate::cluster::{
    ClusterBus, ClusterProjectionUpdate, ClusterServerSnapshot, ClusterSessionDelivery,
    ClusterSessionKind,
};
use crate::event_bus::EventBus;
#[cfg(test)]
use crate::traffic::BROWSER_TRAFFIC_ENDPOINT;
use crate::traffic::{StreamTrafficCounters, TrafficClass, TrafficCounter, TrafficRecorder};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(35);
const NETWORK_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const NETWORK_INITIAL_WINDOW: usize = 256 * 1024;
const NETWORK_MAX_CHUNK: usize = 16 * 1024;
pub(crate) const TERMINAL_BROWSER_QUEUE_CAPACITY: usize = 32;
const TERMINAL_REPLAY_CHUNK_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SocketFrame {
    Text(String),
    Binary(#[serde(with = "serde_bytes")] Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    workspaces: RwLock<HashMap<String, WorkspaceState>>,
    connections: RwLock<HashMap<ServerKey, ServerConnection>>,
    cluster_snapshot_revisions: Mutex<HashMap<ServerKey, u64>>,
    cluster_leases: Mutex<HashMap<ServerKey, (u64, Option<Instant>)>>,
    pending: Mutex<HashMap<String, PendingCommand>>,
    terminal_sessions: Mutex<HashMap<String, TerminalSession>>,
    network_streams: Mutex<HashMap<NetworkStreamKey, NetworkStreamLeg>>,
    browser_network_streams: Mutex<HashMap<NetworkStreamKey, mpsc::Sender<NetworkBinaryFrame>>>,
    events: broadcast::Sender<WorkspaceEvent>,
    event_bus: EventBus,
    cluster: ClusterBus,
    traffic: TrafficRecorder,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ServerKey {
    workspace_id: String,
    server_id: String,
}

#[derive(Clone)]
struct ServerConnection {
    connection_id: Uuid,
    controller_instance_id: String,
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
    outgoing: mpsc::Sender<SocketFrame>,
    last_revision: Option<u64>,
    stream_epoch: Option<String>,
}

pub(crate) struct TerminalReadyPayload {
    pub revision: u64,
    pub replay: Vec<u8>,
    pub stream_epoch: Option<String>,
    pub gap: bool,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct NetworkStreamKey {
    workspace_id: String,
    server_id: String,
    stream_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkStreamRole {
    Source,
    Destination,
}

struct NetworkStreamLeg {
    peer: NetworkStreamKey,
    role: NetworkStreamRole,
    closed: bool,
    outgoing_traffic: std::sync::Arc<TrafficCounter>,
}

struct WorkspaceState {
    info: WorkspaceInfo,
    revision: u64,
    servers: HashMap<String, ServerInfo>,
    agents: HashMap<String, AgentInfo>,
    server_names: HashMap<String, String>,
    agent_names: HashMap<String, String>,
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
        Self::with_event_bus(EventBus::in_process())
    }

    pub fn with_event_bus(event_bus: EventBus) -> Self {
        Self::with_backplanes(
            event_bus,
            ClusterBus::standalone(format!("proxy_{}", Uuid::new_v4().simple())),
        )
    }

    pub fn with_backplanes(event_bus: EventBus, cluster: ClusterBus) -> Self {
        Self::with_backplanes_and_traffic(event_bus, cluster, TrafficRecorder::default())
    }

    pub fn with_backplanes_and_traffic(
        event_bus: EventBus,
        cluster: ClusterBus,
        traffic: TrafficRecorder,
    ) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            inner: std::sync::Arc::new(Inner {
                workspaces: RwLock::new(HashMap::new()),
                connections: RwLock::new(HashMap::new()),
                cluster_snapshot_revisions: Mutex::new(HashMap::new()),
                cluster_leases: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                terminal_sessions: Mutex::new(HashMap::new()),
                network_streams: Mutex::new(HashMap::new()),
                browser_network_streams: Mutex::new(HashMap::new()),
                events,
                event_bus,
                cluster,
                traffic,
            }),
        }
    }

    pub async fn recent_machine_traffic(
        &self,
        workspace_id: &str,
        hours: u16,
    ) -> anyhow::Result<Vec<MachineTrafficRecord>> {
        self.inner.traffic.recent(workspace_id, hours).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkspaceEvent> {
        self.inner.events.subscribe()
    }

    pub async fn allow_server_reenrollment(&self, workspace_id: &str, server_id: &str) {
        if let Some(workspace) = self.inner.workspaces.write().await.get_mut(workspace_id) {
            workspace.deleted_servers.remove(server_id);
        }
    }

    pub async fn broadcast_proxy_message(&self, workspace_id: &str, message: &ProxyMessage) {
        let Ok(encoded) = serde_json::to_string(message) else {
            return;
        };
        let frame = SocketFrame::Text(encoded);
        self.handle_cluster_workspace_broadcast(workspace_id, frame.clone())
            .await;
        if let Err(error) = self
            .inner
            .cluster
            .broadcast_workspace(workspace_id, frame)
            .await
        {
            warn!(?error, %workspace_id, "failed to broadcast workspace message across proxies");
        }
    }

    pub(crate) async fn handle_cluster_workspace_broadcast(
        &self,
        workspace_id: &str,
        frame: SocketFrame,
    ) {
        let outgoing = self
            .inner
            .connections
            .read()
            .await
            .iter()
            .filter(|(key, _)| key.workspace_id == workspace_id)
            .map(|(_, connection)| connection.outgoing.clone())
            .collect::<Vec<_>>();
        for connection in outgoing {
            let _ = connection.send(frame.clone());
        }
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
        self.upsert_workspace_info(info, false).await
    }

    async fn upsert_workspace_info(
        &self,
        info: WorkspaceInfo,
        publish_change: bool,
    ) -> WorkspaceInfo {
        let mut event = None;
        let mut workspaces = self.inner.workspaces.write().await;
        let workspace = workspaces
            .entry(info.workspace_id.clone())
            .or_insert_with(|| WorkspaceState {
                info: info.clone(),
                revision: 0,
                servers: HashMap::new(),
                agents: HashMap::new(),
                server_names: HashMap::new(),
                agent_names: HashMap::new(),
                deleted_servers: HashSet::new(),
                deleted_agents: HashSet::new(),
            });
        if publish_change && workspace.info != info {
            workspace.info = info.clone();
            workspace.revision = workspace.revision.saturating_add(1);
            event = Some(WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: info.workspace_id.clone(),
                event: "workspace.renamed".to_string(),
                data: serde_json::to_value(&info).unwrap_or(Value::Null),
            });
        }
        let current = workspace.info.clone();
        drop(workspaces);
        if let Some(event) = event {
            self.publish_workspace_event(event);
        }
        current
    }

    pub async fn create_workspace_info(
        &self,
        info: WorkspaceInfo,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        {
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
                    server_names: HashMap::new(),
                    agent_names: HashMap::new(),
                    deleted_servers: HashSet::new(),
                    deleted_agents: HashSet::new(),
                },
            );
        }
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: info.clone(),
        })
        .await?;
        Ok(info)
    }

    pub async fn rename_workspace_info(
        &self,
        info: WorkspaceInfo,
    ) -> Result<WorkspaceInfo, ProtocolError> {
        self.upsert_workspace_info(info.clone(), true).await;
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert {
            workspace: info.clone(),
        })
        .await?;
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

    pub async fn platform_agent_count(&self) -> usize {
        self.inner
            .workspaces
            .read()
            .await
            .values()
            .map(|workspace| workspace.agents.len())
            .sum()
    }

    pub async fn live_servers(&self) -> Vec<ServerInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut servers: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.servers.values().cloned())
            .collect();
        servers.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        servers
    }

    pub async fn live_agents(&self) -> Vec<AgentInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut agents: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.agents.values().cloned())
            .collect();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        agents
    }

    pub async fn live_server(&self, server_id: &str) -> Option<ServerInfo> {
        let workspaces = self.inner.workspaces.read().await;
        workspaces
            .values()
            .find_map(|workspace| workspace.servers.get(server_id).cloned())
    }

    #[cfg(test)]
    pub async fn test_insert_agent(&self, agent: AgentInfo) {
        let mut workspaces = self.inner.workspaces.write().await;
        workspaces
            .get_mut(&agent.workspace_id)
            .expect("workspace")
            .agents
            .insert(agent.agent_id.clone(), agent);
    }

    pub async fn live_agents_on_server(&self, server_id: &str) -> Vec<AgentInfo> {
        let workspaces = self.inner.workspaces.read().await;
        let mut agents: Vec<_> = workspaces
            .values()
            .flat_map(|workspace| workspace.agents.values())
            .filter(|agent| agent.server_id == server_id)
            .cloned()
            .collect();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        agents
    }

    #[cfg(test)]
    pub async fn register_server(
        &self,
        server: ServerInfo,
        connection_id: Uuid,
        outgoing: mpsc::UnboundedSender<SocketFrame>,
    ) -> Result<u64, ProtocolError> {
        self.register_server_instance(server, connection_id, "unknown".to_string(), outgoing)
            .await
    }

    pub async fn register_server_instance(
        &self,
        mut server: ServerInfo,
        connection_id: Uuid,
        controller_instance_id: String,
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
        let replaced = self.inner.connections.write().await.insert(
            key,
            ServerConnection {
                connection_id,
                controller_instance_id: controller_instance_id.clone(),
                outgoing,
            },
        );
        if let Some(replaced) = replaced {
            if let Ok(frame) = proxy_message_frame(&ProxyMessage::Error {
                error: ProtocolError::new(
                    "duplicate_machine_connection",
                    format!(
                        "Controller {controller_instance_id} replaced this connection for {}",
                        server.server_id
                    ),
                ),
            }) {
                let _ = replaced.outgoing.send(frame);
            }
            let _ = replaced.outgoing.send(SocketFrame::Close);
        }

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
        let snapshot = self
            .server_snapshot(&server.workspace_id, &server.server_id)
            .await?;
        if let Err(error) = self
            .inner
            .cluster
            .claim(
                &server.workspace_id,
                &server.server_id,
                connection_id,
                snapshot,
            )
            .await
        {
            self.inner.connections.write().await.remove(&ServerKey {
                workspace_id: server.workspace_id.clone(),
                server_id: server.server_id.clone(),
            });
            return Err(error);
        }
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
        self.publish_workspace_event(event);
        let snapshot = self.server_snapshot(&workspace_id, &server_id).await?;
        self.inner
            .cluster
            .publish_snapshot(&workspace_id, &server_id, connection_id, snapshot)
            .await?;
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
        {
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
        }
        if self
            .inner
            .cluster
            .renew(workspace_id, server_id, connection_id)
            .await?
        {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "duplicate_machine_connection",
                format!(
                    "another Controller currently owns machine {server_id}; stopping this duplicate connection"
                ),
            ))
        }
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
        self.publish_workspace_event(event);
        let snapshot = self
            .server_snapshot(&agent.workspace_id, &agent.server_id)
            .await?;
        self.inner
            .cluster
            .publish_snapshot(
                &agent.workspace_id,
                &agent.server_id,
                connection_id,
                snapshot,
            )
            .await?;
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
            workspace.agent_names.remove(&agent_id);
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
            workspace.agent_names.remove(agent_id);
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
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::AgentDeleted {
            workspace_id: workspace_id.to_string(),
            agent_id: agent_id.to_string(),
        })
        .await?;
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
            workspace
                .server_names
                .insert(server_id.to_string(), server.name.clone());
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
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::ServerRenamed {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            name: server.name.clone(),
        })
        .await?;
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
            workspace.server_names.remove(server_id);
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
            for agent_id in &agent_ids {
                workspace.agent_names.remove(agent_id);
            }
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
        self.publish_workspace_event(event);
        self.release_server_runtime(workspace_id, server_id, "machine deleted", "server_deleted")
            .await;
        self.broadcast_projection(ClusterProjectionUpdate::ServerDeleted {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        })
        .await?;

        Ok((server, agents))
    }

    pub async fn delete_workspace(&self, workspace_id: &str) -> Result<(), ProtocolError> {
        let removed = {
            let mut workspaces = self.inner.workspaces.write().await;
            workspaces.remove(workspace_id)
        };
        if let Some(workspace) = removed {
            self.publish_workspace_event(WorkspaceEvent {
                revision: workspace.revision.saturating_add(1),
                workspace_id: workspace_id.to_string(),
                event: "workspace.deleted".to_string(),
                data: serde_json::to_value(&workspace.info).unwrap_or(Value::Null),
            });
            for server_id in workspace.servers.keys() {
                self.release_server_runtime(
                    workspace_id,
                    server_id,
                    "workspace deleted",
                    "workspace_deleted",
                )
                .await;
            }
        }
        self.broadcast_projection(ClusterProjectionUpdate::WorkspaceDeleted {
            workspace_id: workspace_id.to_string(),
        })
        .await?;
        Ok(())
    }

    async fn release_server_runtime(
        &self,
        workspace_id: &str,
        server_id: &str,
        terminal_reason: &str,
        pending_error: &str,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let connection = self.inner.connections.write().await.remove(&key);
        if let Some(connection) = connection {
            self.inner
                .cluster
                .release(workspace_id, server_id, connection.connection_id)
                .await;
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
                ProtocolError::new(pending_error, server_id),
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
                    reason: Some(terminal_reason.to_string()),
                    exit_code: None,
                },
            );
        }
        self.close_server_network_streams(workspace_id, server_id)
            .await;
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
            workspace
                .agent_names
                .insert(agent_id.to_string(), agent.name.clone());
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
        self.publish_workspace_event(event);
        self.broadcast_projection(ClusterProjectionUpdate::AgentRenamed {
            workspace_id: workspace_id.to_string(),
            agent_id: agent_id.to_string(),
            name: agent.name.clone(),
        })
        .await?;
        Ok(agent)
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
        let command_id = format!("cmd_{}", Uuid::new_v4().simple());
        if !self.inner.cluster.is_distributed() {
            return self
                .send_local_command(workspace_id, server_id, None, command_id, command)
                .await;
        }
        let owner = self
            .inner
            .cluster
            .owner(workspace_id, server_id)
            .await?
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        if owner.proxy_id == self.inner.cluster.instance_id() {
            self.send_local_command(
                workspace_id,
                server_id,
                Some(owner.connection_id),
                command_id,
                command,
            )
            .await
        } else {
            self.inner
                .cluster
                .request_command(&owner, workspace_id, server_id, command_id, command)
                .await
        }
    }

    async fn send_local_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        expected_connection_id: Option<Uuid>,
        command_id: String,
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
            .filter(|connection| {
                expected_connection_id.is_none_or(|expected| expected == connection.connection_id)
            })
            .map(|connection| connection.outgoing.clone())
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;

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

    pub(crate) async fn handle_cluster_command(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        command_id: String,
        command: AgentCommand,
    ) -> Result<Value, ProtocolError> {
        self.send_local_command(
            workspace_id,
            server_id,
            Some(connection_id),
            command_id,
            command,
        )
        .await
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
        cursor: Option<TerminalCursor>,
        outgoing: mpsc::Sender<SocketFrame>,
    ) -> Result<String, ProtocolError> {
        let agent = self.resolve_agent(workspace_id, agent_id).await?;
        let server_id = agent.server_id;
        let session_id = self.inner.cluster.routed_id("term");
        self.inner.terminal_sessions.lock().await.insert(
            session_id.clone(),
            TerminalSession {
                workspace_id: workspace_id.to_string(),
                server_id: server_id.clone(),
                process_id: agent.agent_id.clone(),
                outgoing,
                last_revision: None,
                stream_epoch: cursor.as_ref().map(|cursor| cursor.stream_epoch.clone()),
            },
        );
        let message = ProxyMessage::TerminalAttach {
            session_id: session_id.clone(),
            agent_id: agent.agent_id,
            cols: cols.max(1),
            rows: rows.max(1),
            cursor,
        };
        if let Err(error) = self
            .send_server_frame(workspace_id, &server_id, proxy_message_frame(&message)?)
            .await
        {
            self.inner
                .terminal_sessions
                .lock()
                .await
                .remove(&session_id);
            return Err(error);
        }
        Ok(session_id)
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
        let (workspace_id, server_id) = self.terminal_session_route(session_id).await?;
        let encoded = TerminalBinaryFrame {
            kind: TerminalBinaryKind::Input,
            session_id: session_id.to_string(),
            revision: 0,
            payload: data,
        }
        .encode()?;
        self.send_server_frame(&workspace_id, &server_id, SocketFrame::Binary(encoded))
            .await
    }

    pub async fn terminal_resize(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), ProtocolError> {
        let (workspace_id, server_id) = self.terminal_session_route(session_id).await?;
        let message = ProxyMessage::TerminalResize {
            session_id: session_id.to_string(),
            cols: cols.max(1),
            rows: rows.max(1),
        };
        self.send_server_frame(&workspace_id, &server_id, proxy_message_frame(&message)?)
            .await
    }

    pub async fn detach_terminal(&self, session_id: &str) {
        let session = self.inner.terminal_sessions.lock().await.remove(session_id);
        let Some(session) = session else {
            return;
        };
        let message = ProxyMessage::TerminalDetach {
            session_id: session_id.to_string(),
        };
        if let Ok(frame) = proxy_message_frame(&message) {
            let _ = self
                .send_server_frame(&session.workspace_id, &session.server_id, frame)
                .await;
        }
    }

    pub async fn terminal_ready(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        session_id: &str,
        ready: TerminalReadyPayload,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, server_id, connection_id)
            .await?;
        {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.stream_epoch = ready.stream_epoch.clone();
            }
        }
        let replay_chunks = ready.replay.len().div_ceil(TERMINAL_REPLAY_CHUNK_BYTES);
        let replay_chunks = u32::try_from(replay_chunks).map_err(|_| {
            ProtocolError::new(
                "terminal_replay_too_large",
                "terminal replay has too many chunks",
            )
        })?;
        let message = TerminalServerMessage::Ready {
            session_id: session_id.to_string(),
            stream_epoch: ready.stream_epoch.clone(),
            revision: Some(ready.revision),
            gap: ready.gap,
            replay_chunks: Some(replay_chunks),
        };
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: (replay_chunks == 0).then_some(ready.revision),
            cursor: false,
            close: false,
            frame: SocketFrame::Text(
                serde_json::to_string(&message)
                    .map_err(|error| ProtocolError::new("encode_error", error.to_string()))?,
            ),
        })
        .await?;
        for (index, chunk) in ready.replay.chunks(TERMINAL_REPLAY_CHUNK_BYTES).enumerate() {
            let final_chunk = index + 1 == replay_chunks as usize;
            self.deliver_session_frame(ClusterSessionDelivery {
                kind: ClusterSessionKind::Terminal,
                workspace_id: workspace_id.to_string(),
                server_id: server_id.to_string(),
                session_id: session_id.to_string(),
                revision: final_chunk.then_some(ready.revision),
                cursor: false,
                close: false,
                frame: SocketFrame::Binary(chunk.to_vec()),
            })
            .await?;
        }
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
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: Some(revision),
            cursor: true,
            close: false,
            frame: SocketFrame::Binary(data),
        })
        .await
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
        self.inner
            .cluster
            .release(workspace_id, server_id, connection_id)
            .await;
        let payload = serde_json::json!({ "server_id": server_id, "status": "offline" });
        let _ = self
            .mutate_workspace(workspace_id, "server.offline", &payload, |workspace| {
                if let Some(server) = workspace.servers.get_mut(server_id) {
                    server.status = ServerStatus::Offline;
                    server.last_seen_at = Utc::now();
                }
            })
            .await;

        self.close_server_sessions(workspace_id, server_id, "agent server disconnected")
            .await;
    }

    pub async fn open_browser_network_stream(
        &self,
        workspace_id: &str,
        destination_server_id: &str,
        destination_agent_id: Option<&str>,
        host: &str,
        port: u16,
        traffic_class: TrafficClass,
    ) -> Result<DuplexStream, ProtocolError> {
        let key = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: destination_server_id.to_string(),
            stream_id: self.inner.cluster.routed_id("browser"),
        };
        let (incoming_tx, mut incoming_rx) = mpsc::channel(32);
        self.inner
            .browser_network_streams
            .lock()
            .await
            .insert(key.clone(), incoming_tx);

        let request = NetworkConnectRequest {
            source_server_id: "browser".to_string(),
            source_agent_id: None,
            destination_agent_id: destination_agent_id.map(str::to_string),
            host: host.to_string(),
            port,
        };
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Open,
            stream_id: key.stream_id.clone(),
            payload: serde_json::to_vec(&request).map_err(|error| {
                ProtocolError::new(
                    "encode_error",
                    format!("failed to encode network request: {error}"),
                )
            })?,
        };
        if self
            .send_server_frame(
                workspace_id,
                destination_server_id,
                SocketFrame::Binary(frame.encode()?),
            )
            .await
            .is_err()
        {
            self.inner.browser_network_streams.lock().await.remove(&key);
            return Err(ProtocolError::new("server_offline", destination_server_id));
        }

        let opened = tokio::time::timeout(NETWORK_OPEN_TIMEOUT, incoming_rx.recv()).await;
        match opened {
            Ok(Some(frame)) if frame.kind == NetworkBinaryKind::Opened => {}
            Ok(Some(frame)) if frame.kind == NetworkBinaryKind::Reset => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(decode_network_reset(&frame));
            }
            Ok(Some(frame)) => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "invalid_network_frame",
                    format!("expected opened frame, received {:?}", frame.kind),
                ));
            }
            Ok(None) => {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "network_stream_closed",
                    "network stream closed before it opened",
                ));
            }
            Err(_) => {
                self.reset_browser_network_stream(
                    &key,
                    ProtocolError::new("network_open_timeout", "network connection timed out"),
                )
                .await;
                return Err(ProtocolError::new(
                    "network_open_timeout",
                    "network connection timed out",
                ));
            }
        }

        let (client, bridge) = tokio::io::duplex(NETWORK_INITIAL_WINDOW);
        let traffic = self.inner.traffic.register_client_stream(
            workspace_id,
            traffic_class,
            destination_server_id,
        );
        let state = self.clone();
        tokio::spawn(async move {
            if let Err(error) = state
                .bridge_browser_network_stream(&key, bridge, incoming_rx, traffic)
                .await
            {
                state.reset_browser_network_stream(&key, error).await;
            } else {
                state
                    .inner
                    .browser_network_streams
                    .lock()
                    .await
                    .remove(&key);
            }
        });
        Ok(client)
    }

    async fn bridge_browser_network_stream(
        &self,
        key: &NetworkStreamKey,
        stream: DuplexStream,
        mut incoming: mpsc::Receiver<NetworkBinaryFrame>,
        traffic: StreamTrafficCounters,
    ) -> Result<(), ProtocolError> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut buffer = vec![0_u8; NETWORK_MAX_CHUNK];
        let mut send_window = NETWORK_INITIAL_WINDOW;
        let mut local_closed = false;
        let mut remote_closed = false;
        while !local_closed || !remote_closed {
            tokio::select! {
                read = reader.read(&mut buffer[..send_window.min(NETWORK_MAX_CHUNK)]), if !local_closed && send_window > 0 => {
                    let read = read.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                    if read == 0 {
                        local_closed = true;
                        self.send_browser_network_frame(key, NetworkBinaryKind::HalfClose, Vec::new()).await?;
                    } else {
                        send_window -= read;
                        self.send_browser_network_frame(key, NetworkBinaryKind::Data, buffer[..read].to_vec()).await?;
                        traffic.source_to_destination.record(read);
                    }
                }
                frame = incoming.recv() => {
                    let frame = frame.ok_or_else(|| ProtocolError::new("network_stream_closed", "network stream receiver closed"))?;
                    match frame.kind {
                        NetworkBinaryKind::Data => {
                            writer.write_all(&frame.payload).await.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                            traffic.destination_to_source.record(frame.payload.len());
                            let amount = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
                            self.send_browser_network_frame(key, NetworkBinaryKind::WindowUpdate, amount).await?;
                        }
                        NetworkBinaryKind::WindowUpdate => {
                            let bytes: [u8; 4] = frame.payload.as_slice().try_into().map_err(|_| ProtocolError::new("invalid_network_frame", "invalid network window update"))?;
                            send_window = send_window.saturating_add(u32::from_be_bytes(bytes) as usize);
                        }
                        NetworkBinaryKind::HalfClose => {
                            if !remote_closed {
                                writer.shutdown().await.map_err(|error| ProtocolError::new("network_io_error", error.to_string()))?;
                                remote_closed = true;
                            }
                        }
                        NetworkBinaryKind::Reset => return Err(decode_network_reset(&frame)),
                        NetworkBinaryKind::Open
                        | NetworkBinaryKind::Opened
                        | NetworkBinaryKind::Direct => {
                            return Err(ProtocolError::new("invalid_network_frame", format!("unexpected network stream frame {:?}", frame.kind)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn send_browser_network_frame(
        &self,
        key: &NetworkStreamKey,
        kind: NetworkBinaryKind,
        payload: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let frame = NetworkBinaryFrame {
            kind,
            stream_id: key.stream_id.clone(),
            payload,
        };
        self.send_server_frame(
            &key.workspace_id,
            &key.server_id,
            SocketFrame::Binary(frame.encode()?),
        )
        .await
    }

    async fn reset_browser_network_stream(&self, key: &NetworkStreamKey, error: ProtocolError) {
        self.inner.browser_network_streams.lock().await.remove(key);
        let payload = serde_json::to_vec(&error).unwrap_or_default();
        let _ = self
            .send_browser_network_frame(key, NetworkBinaryKind::Reset, payload)
            .await;
    }

    pub async fn open_network_stream(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        connection_id: Uuid,
        destination_server_id: &str,
        mut frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        self.require_current_connection(workspace_id, source_server_id, connection_id)
            .await?;
        if frame.kind != NetworkBinaryKind::Open {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "new network stream must begin with an open frame",
            ));
        }
        let source = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: source_server_id.to_string(),
            stream_id: frame.stream_id.clone(),
        };
        let destination = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: destination_server_id.to_string(),
            stream_id: self.inner.cluster.routed_id("net"),
        };
        let traffic = self.inner.traffic.register_machine_stream(
            workspace_id,
            source_server_id,
            destination_server_id,
        );
        {
            let mut streams = self.inner.network_streams.lock().await;
            if streams.contains_key(&source) || streams.contains_key(&destination) {
                return Err(ProtocolError::new(
                    "network_stream_exists",
                    "network stream ID is already in use",
                ));
            }
            streams.insert(
                source.clone(),
                NetworkStreamLeg {
                    peer: destination.clone(),
                    role: NetworkStreamRole::Source,
                    closed: false,
                    outgoing_traffic: traffic.source_to_destination,
                },
            );
            streams.insert(
                destination.clone(),
                NetworkStreamLeg {
                    peer: source.clone(),
                    role: NetworkStreamRole::Destination,
                    closed: false,
                    outgoing_traffic: traffic.destination_to_source,
                },
            );
        }
        frame.stream_id.clone_from(&destination.stream_id);
        let encoded = frame.encode()?;
        if self
            .send_server_frame(
                workspace_id,
                destination_server_id,
                SocketFrame::Binary(encoded),
            )
            .await
            .is_err()
        {
            let mut streams = self.inner.network_streams.lock().await;
            remove_network_stream(&mut streams, &source);
            return Err(ProtocolError::new("server_offline", destination_server_id));
        }
        Ok(())
    }

    pub async fn send_direct_network_route(
        &self,
        workspace_id: &str,
        source_server_id: &str,
        connection_id: Uuid,
        stream_id: String,
        target: NetworkDirectTarget,
    ) -> Result<(), ProtocolError> {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: source_server_id.to_string(),
        };
        let outgoing = {
            let connections = self.inner.connections.read().await;
            match connections.get(&key) {
                Some(connection) if connection.connection_id == connection_id => {
                    connection.outgoing.clone()
                }
                _ => {
                    return Err(ProtocolError::new(
                        "stale_connection",
                        format!("connection for {source_server_id} is no longer current"),
                    ));
                }
            }
        };
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Direct,
            stream_id,
            payload: serde_json::to_vec(&target).map_err(|error| {
                ProtocolError::new(
                    "encode_error",
                    format!("failed to encode direct route: {error}"),
                )
            })?,
        };
        outgoing
            .send(SocketFrame::Binary(frame.encode()?))
            .map_err(|_| ProtocolError::new("server_offline", source_server_id))
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
        self.relay_network_frame_inner(workspace_id, server_id, frame)
            .await
    }

    async fn relay_network_frame_inner(
        &self,
        workspace_id: &str,
        server_id: &str,
        mut frame: NetworkBinaryFrame,
    ) -> Result<(), ProtocolError> {
        if matches!(
            frame.kind,
            NetworkBinaryKind::Open | NetworkBinaryKind::Direct
        ) {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "route frame cannot be relayed as an existing stream",
            ));
        }
        let key = NetworkStreamKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            stream_id: frame.stream_id.clone(),
        };
        let browser = self
            .inner
            .browser_network_streams
            .lock()
            .await
            .get(&key)
            .cloned();
        if let Some(browser) = browser {
            if browser.send(frame).await.is_err() {
                self.inner.browser_network_streams.lock().await.remove(&key);
                return Err(ProtocolError::new(
                    "network_stream_closed",
                    "browser network stream closed",
                ));
            }
            return Ok(());
        }
        let route = {
            let mut streams = self.inner.network_streams.lock().await;
            let Some(stream) = streams.get_mut(&key) else {
                drop(streams);
                let target = ClusterBus::route_target(&frame.stream_id).ok_or_else(|| {
                    ProtocolError::new("network_stream_not_found", &frame.stream_id)
                })?;
                if target == self.inner.cluster.instance_id() {
                    return Err(ProtocolError::new(
                        "network_stream_not_found",
                        &frame.stream_id,
                    ));
                }
                return self
                    .inner
                    .cluster
                    .deliver_network(&target, workspace_id, server_id, frame.encode()?)
                    .await;
            };
            if frame.kind == NetworkBinaryKind::Opened
                && stream.role != NetworkStreamRole::Destination
            {
                return Err(ProtocolError::new(
                    "invalid_network_frame",
                    "only the destination can open a network stream",
                ));
            }
            if frame.kind == NetworkBinaryKind::HalfClose {
                stream.closed = true;
            }
            let peer = stream.peer.clone();
            let traffic =
                (frame.kind == NetworkBinaryKind::Data).then(|| stream.outgoing_traffic.clone());
            let remove = frame.kind == NetworkBinaryKind::Reset
                || (stream.closed && streams.get(&peer).is_some_and(|peer| peer.closed));
            if remove {
                remove_network_stream(&mut streams, &key);
            }
            (peer, remove, traffic)
        };
        let (peer, remove, traffic) = route;
        let payload_bytes = frame.payload.len();
        frame.stream_id.clone_from(&peer.stream_id);
        if self
            .send_server_frame(
                workspace_id,
                &peer.server_id,
                SocketFrame::Binary(frame.encode()?),
            )
            .await
            .is_err()
            && !remove
        {
            let mut streams = self.inner.network_streams.lock().await;
            remove_network_stream(&mut streams, &key);
            return Err(ProtocolError::new("server_offline", peer.server_id));
        }
        if let Some(traffic) = traffic {
            traffic.record(payload_bytes);
        }
        Ok(())
    }

    pub(crate) async fn handle_cluster_network_delivery(
        &self,
        workspace_id: &str,
        server_id: &str,
        encoded: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let frame = NetworkBinaryFrame::decode(&encoded)?;
        self.relay_network_frame_inner(workspace_id, server_id, frame)
            .await
    }

    async fn close_server_network_streams(&self, workspace_id: &str, server_id: &str) {
        let browser_streams = {
            let mut streams = self.inner.browser_network_streams.lock().await;
            let keys = streams
                .keys()
                .filter(|key| key.workspace_id == workspace_id && key.server_id == server_id)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| streams.remove(&key).map(|sender| (key, sender)))
                .collect::<Vec<_>>()
        };
        let browser_payload = serde_json::to_vec(&ProtocolError::new(
            "server_offline",
            "agent server disconnected",
        ))
        .unwrap_or_default();
        for (key, sender) in browser_streams {
            let _ = sender
                .send(NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Reset,
                    stream_id: key.stream_id,
                    payload: browser_payload.clone(),
                })
                .await;
        }

        let routes = {
            let mut streams = self.inner.network_streams.lock().await;
            let keys = streams
                .keys()
                .filter(|key| key.workspace_id == workspace_id && key.server_id == server_id)
                .cloned()
                .collect::<Vec<_>>();
            let mut routes = Vec::new();
            for key in keys {
                let Some(stream) = remove_network_stream(&mut streams, &key) else {
                    continue;
                };
                if stream.peer.server_id != server_id {
                    routes.push(stream.peer);
                }
            }
            routes
        };
        let payload = serde_json::to_vec(&ProtocolError::new(
            "server_offline",
            "network peer disconnected",
        ))
        .unwrap_or_default();
        for peer in routes {
            let frame = NetworkBinaryFrame {
                kind: NetworkBinaryKind::Reset,
                stream_id: peer.stream_id,
                payload: payload.clone(),
            };
            if let Ok(encoded) = frame.encode() {
                let _ = self
                    .send_server_frame(workspace_id, &peer.server_id, SocketFrame::Binary(encoded))
                    .await;
            }
        }
    }

    async fn close_server_sessions(&self, workspace_id: &str, server_id: &str, reason: &str) {
        let disconnected = {
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
        for session in disconnected {
            send_terminal_to_browser(
                &session.outgoing,
                &TerminalServerMessage::Closed {
                    reason: Some(reason.to_string()),
                    exit_code: None,
                },
            );
        }
        self.close_server_network_streams(workspace_id, server_id)
            .await;
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

    async fn send_server_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        if !self.inner.cluster.is_distributed() {
            return self
                .local_server_frame(workspace_id, server_id, None, frame)
                .await;
        }
        let owner = self
            .inner
            .cluster
            .owner(workspace_id, server_id)
            .await?
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        if owner.proxy_id == self.inner.cluster.instance_id() {
            self.local_server_frame(workspace_id, server_id, Some(owner.connection_id), frame)
                .await
        } else {
            self.inner
                .cluster
                .send_socket(&owner, workspace_id, server_id, frame)
                .await
        }
    }

    async fn local_server_frame(
        &self,
        workspace_id: &str,
        server_id: &str,
        expected_connection_id: Option<Uuid>,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
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
            .filter(|connection| {
                expected_connection_id.is_none_or(|expected| expected == connection.connection_id)
            })
            .map(|connection| connection.outgoing.clone())
            .ok_or_else(|| ProtocolError::new("server_offline", server_id))?;
        outgoing
            .send(frame)
            .map_err(|_| ProtocolError::new("server_offline", server_id))
    }

    pub(crate) async fn handle_cluster_socket(
        &self,
        workspace_id: &str,
        server_id: &str,
        connection_id: Uuid,
        frame: SocketFrame,
    ) -> Result<(), ProtocolError> {
        self.local_server_frame(workspace_id, server_id, Some(connection_id), frame)
            .await
    }

    async fn server_snapshot(
        &self,
        workspace_id: &str,
        server_id: &str,
    ) -> Result<AgentServerSnapshot, ProtocolError> {
        let workspaces = self.inner.workspaces.read().await;
        let workspace = workspaces
            .get(workspace_id)
            .ok_or_else(|| ProtocolError::new("workspace_not_found", workspace_id))?;
        let server = workspace
            .servers
            .get(server_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new("server_not_found", server_id))?;
        let agents = workspace
            .agents
            .values()
            .filter(|agent| agent.server_id == server_id)
            .cloned()
            .collect();
        Ok(AgentServerSnapshot { server, agents })
    }

    pub(crate) async fn apply_cluster_snapshot(&self, update: ClusterServerSnapshot) {
        let key = ServerKey {
            workspace_id: update.snapshot.server.workspace_id.clone(),
            server_id: update.snapshot.server.server_id.clone(),
        };
        {
            let mut revisions = self.inner.cluster_snapshot_revisions.lock().await;
            if revisions
                .get(&key)
                .is_some_and(|revision| *revision >= update.revision)
            {
                return;
            }
            revisions.insert(key, update.revision);
        }
        let snapshot = update.snapshot;
        let workspace_id = snapshot.server.workspace_id.clone();
        let server_id = snapshot.server.server_id.clone();
        self.ensure_workspace(&workspace_id, &workspace_id).await;
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                return;
            };
            workspace.deleted_servers.remove(&server_id);
            let mut server = snapshot.server;
            if let Some(name) = workspace.server_names.get(&server_id) {
                server.name.clone_from(name);
            } else if let Some(current) = workspace.servers.get(&server_id) {
                server.name.clone_from(&current.name);
            }
            workspace.servers.insert(server_id.clone(), server);
            let names = workspace
                .agents
                .values()
                .filter(|agent| agent.server_id == server_id)
                .map(|agent| (agent.agent_id.clone(), agent.name.clone()))
                .collect::<HashMap<_, _>>();
            workspace
                .agents
                .retain(|_, agent| agent.server_id != server_id);
            for mut agent in snapshot.agents {
                if !workspace.deleted_agents.contains(&agent.agent_id) {
                    if let Some(name) = workspace.agent_names.get(&agent.agent_id) {
                        agent.name.clone_from(name);
                    } else if let Some(name) = names.get(&agent.agent_id) {
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
                data: serde_json::json!({ "server_id": server_id }),
            }
        };
        let _ = self.inner.events.send(event);
    }

    pub(crate) async fn apply_cluster_disconnect(
        &self,
        workspace_id: &str,
        server_id: &str,
        revision: u64,
    ) {
        if self
            .server_outgoing(workspace_id, server_id)
            .await
            .is_some()
        {
            return;
        }
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        {
            let mut leases = self.inner.cluster_leases.lock().await;
            if leases
                .get(&key)
                .is_some_and(|(current, _)| *current > revision)
            {
                return;
            }
            leases.insert(key.clone(), (revision, None));
        }
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                return;
            };
            let Some(server) = workspace.servers.get_mut(server_id) else {
                return;
            };
            if server.status == ServerStatus::Offline {
                None
            } else {
                server.status = ServerStatus::Offline;
                server.last_seen_at = Utc::now();
                workspace.revision = workspace.revision.saturating_add(1);
                Some(WorkspaceEvent {
                    revision: workspace.revision,
                    workspace_id: workspace_id.to_string(),
                    event: "server.offline".to_string(),
                    data: serde_json::json!({ "server_id": server_id, "status": "offline" }),
                })
            }
        };
        if let Some(event) = event {
            let _ = self.inner.events.send(event);
        }
        self.close_server_sessions(workspace_id, server_id, "agent server disconnected")
            .await;
    }

    pub(crate) async fn note_cluster_lease(
        &self,
        workspace_id: &str,
        server_id: &str,
        revision: u64,
    ) {
        let key = ServerKey {
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
        };
        let accepted = {
            let mut leases = self.inner.cluster_leases.lock().await;
            if leases
                .get(&key)
                .is_some_and(|(current, _)| revision < *current)
            {
                false
            } else {
                leases.insert(key, (revision, Some(Instant::now())));
                true
            }
        };
        if !accepted {
            return;
        }
        let event = {
            let mut workspaces = self.inner.workspaces.write().await;
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                return;
            };
            let Some(server) = workspace.servers.get_mut(server_id) else {
                return;
            };
            if server.status == ServerStatus::Online {
                return;
            }
            server.status = ServerStatus::Online;
            server.last_seen_at = Utc::now();
            workspace.revision = workspace.revision.saturating_add(1);
            WorkspaceEvent {
                revision: workspace.revision,
                workspace_id: workspace_id.to_string(),
                event: "server.online".to_string(),
                data: serde_json::json!({ "server_id": server_id, "status": "online" }),
            }
        };
        let _ = self.inner.events.send(event);
    }

    async fn expire_cluster_lease_entries(&self, max_age: Duration) -> Vec<(ServerKey, u64)> {
        let mut leases = self.inner.cluster_leases.lock().await;
        let expired = leases
            .iter()
            .filter(|(_, (_, seen_at))| seen_at.is_some_and(|seen_at| seen_at.elapsed() >= max_age))
            .map(|(key, (revision, _))| (key.clone(), *revision))
            .collect::<Vec<_>>();
        for (key, revision) in &expired {
            leases.insert(key.clone(), (*revision, None));
        }
        expired
    }

    pub(crate) async fn expire_cluster_leases(&self, max_age: Duration) {
        let expired = self.expire_cluster_lease_entries(max_age).await;
        for (key, revision) in expired {
            self.apply_cluster_disconnect(&key.workspace_id, &key.server_id, revision)
                .await;
        }
    }

    async fn broadcast_projection(
        &self,
        update: ClusterProjectionUpdate,
    ) -> Result<(), ProtocolError> {
        self.inner.cluster.broadcast_projection(update).await
    }

    pub(crate) async fn apply_cluster_projection(&self, update: ClusterProjectionUpdate) {
        match update {
            ClusterProjectionUpdate::WorkspaceUpsert { workspace } => {
                self.upsert_workspace_info(workspace, true).await;
            }
            ClusterProjectionUpdate::WorkspaceDeleted { workspace_id } => {
                let (event, server_ids) = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.remove(&workspace_id) else {
                        return;
                    };
                    let event = Some(WorkspaceEvent {
                        revision: workspace.revision.saturating_add(1),
                        workspace_id: workspace_id.clone(),
                        event: "workspace.deleted".to_string(),
                        data: serde_json::to_value(&workspace.info).unwrap_or(Value::Null),
                    });
                    let server_ids = workspace.servers.keys().cloned().collect::<Vec<_>>();
                    (event, server_ids)
                };
                if let Some(event) = event {
                    self.publish_workspace_event(event);
                }
                for server_id in server_ids {
                    self.release_server_runtime(
                        &workspace_id,
                        &server_id,
                        "workspace deleted",
                        "workspace_deleted",
                    )
                    .await;
                }
            }
            ClusterProjectionUpdate::ServerRenamed {
                workspace_id,
                server_id,
                name,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    workspace.deleted_servers.remove(&server_id);
                    workspace
                        .server_names
                        .insert(server_id.clone(), name.clone());
                    let Some(server) = workspace.servers.get_mut(&server_id) else {
                        return;
                    };
                    if server.name == name {
                        return;
                    }
                    server.name = name;
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "server.renamed".to_string(),
                        data: serde_json::json!({ "server_id": server_id }),
                    }
                };
                let _ = self.inner.events.send(event);
            }
            ClusterProjectionUpdate::AgentRenamed {
                workspace_id,
                agent_id,
                name,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    workspace.deleted_agents.remove(&agent_id);
                    workspace.agent_names.insert(agent_id.clone(), name.clone());
                    let Some(agent) = workspace.agents.get_mut(&agent_id) else {
                        return;
                    };
                    if agent.name == name {
                        return;
                    }
                    agent.name = name;
                    agent.updated_at = Utc::now();
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "agent.renamed".to_string(),
                        data: serde_json::json!({ "agent_id": agent_id }),
                    }
                };
                let _ = self.inner.events.send(event);
            }
            ClusterProjectionUpdate::AgentDeleted {
                workspace_id,
                agent_id,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    let removed = workspace.agents.remove(&agent_id).is_some();
                    workspace.agent_names.remove(&agent_id);
                    let inserted = workspace.deleted_agents.insert(agent_id.clone());
                    if !removed && !inserted {
                        return;
                    }
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "agent.deleted".to_string(),
                        data: serde_json::json!({ "agent_id": agent_id }),
                    }
                };
                let _ = self.inner.events.send(event);
                self.close_agent_terminals(&workspace_id, &agent_id).await;
            }
            ClusterProjectionUpdate::ServerDeleted {
                workspace_id,
                server_id,
            } => {
                let event = {
                    let mut workspaces = self.inner.workspaces.write().await;
                    let Some(workspace) = workspaces.get_mut(&workspace_id) else {
                        return;
                    };
                    let removed = workspace.servers.remove(&server_id).is_some();
                    workspace.server_names.remove(&server_id);
                    let inserted = workspace.deleted_servers.insert(server_id.clone());
                    if !removed && !inserted {
                        return;
                    }
                    workspace
                        .agents
                        .retain(|_, agent| agent.server_id != server_id);
                    workspace.revision = workspace.revision.saturating_add(1);
                    WorkspaceEvent {
                        revision: workspace.revision,
                        workspace_id: workspace_id.clone(),
                        event: "server.deleted".to_string(),
                        data: serde_json::json!({ "server_id": server_id }),
                    }
                };
                let _ = self.inner.events.send(event);
                let key = ServerKey {
                    workspace_id: workspace_id.clone(),
                    server_id: server_id.clone(),
                };
                let connection = self.inner.connections.write().await.remove(&key);
                if let Some(connection) = connection {
                    self.inner
                        .cluster
                        .release(&workspace_id, &server_id, connection.connection_id)
                        .await;
                    let _ = connection.outgoing.send(SocketFrame::Close);
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
                self.close_server_network_streams(&workspace_id, &server_id)
                    .await;
            }
        }
    }

    pub(crate) async fn handle_cluster_session_delivery(
        &self,
        delivery: ClusterSessionDelivery,
    ) -> Result<(), ProtocolError> {
        let overloaded_route = {
            let mut sessions = self.inner.terminal_sessions.lock().await;
            let session = sessions
                .get_mut(&delivery.session_id)
                .ok_or_else(|| ProtocolError::new("terminal_not_found", &delivery.session_id))?;
            if session.workspace_id != delivery.workspace_id
                || session.server_id != delivery.server_id
            {
                return Err(ProtocolError::new(
                    "terminal_identity_mismatch",
                    &delivery.session_id,
                ));
            }
            if delivery.revision.is_some_and(|revision| {
                session
                    .last_revision
                    .is_some_and(|last_revision| revision <= last_revision)
            }) {
                return Ok(());
            }
            let cursor_frame = if delivery.cursor {
                session
                    .stream_epoch
                    .as_ref()
                    .zip(delivery.revision)
                    .map(|(stream_epoch, revision)| {
                        serde_json::to_string(&TerminalServerMessage::Cursor {
                            stream_epoch: stream_epoch.clone(),
                            revision,
                        })
                        .map(SocketFrame::Text)
                        .map_err(|error| ProtocolError::new("encode_error", error.to_string()))
                    })
                    .transpose()?
            } else {
                None
            };
            let route = (session.workspace_id.clone(), session.server_id.clone());
            let send_result = session.outgoing.try_send(delivery.frame).and_then(|_| {
                if let Some(cursor_frame) = cursor_frame {
                    session.outgoing.try_send(cursor_frame)
                } else {
                    Ok(())
                }
            });
            match send_result {
                Ok(()) => {
                    if let Some(revision) = delivery.revision {
                        session.last_revision = Some(revision);
                    }
                    if delivery.close {
                        sessions.remove(&delivery.session_id);
                    }
                    None
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    sessions.remove(&delivery.session_id);
                    Some(route)
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    sessions.remove(&delivery.session_id);
                    return Err(ProtocolError::new("terminal_closed", delivery.session_id));
                }
            }
        };
        if let Some((workspace_id, server_id)) = overloaded_route {
            let message = ProxyMessage::TerminalDetach {
                session_id: delivery.session_id,
            };
            if let Ok(frame) = proxy_message_frame(&message) {
                let _ = self
                    .send_server_frame(&workspace_id, &server_id, frame)
                    .await;
            }
        }
        Ok(())
    }

    async fn terminal_session_route(
        &self,
        session_id: &str,
    ) -> Result<(String, String), ProtocolError> {
        self.inner
            .terminal_sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| (session.workspace_id.clone(), session.server_id.clone()))
            .ok_or_else(|| ProtocolError::new("terminal_not_found", session_id))
    }

    async fn relay_terminal(
        &self,
        workspace_id: &str,
        server_id: &str,
        session_id: &str,
        message: TerminalServerMessage,
        close: bool,
    ) -> Result<(), ProtocolError> {
        let encoded = serde_json::to_string(&message)
            .map_err(|error| ProtocolError::new("encode_error", error.to_string()))?;
        self.deliver_session_frame(ClusterSessionDelivery {
            kind: ClusterSessionKind::Terminal,
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            session_id: session_id.to_string(),
            revision: None,
            cursor: false,
            close,
            frame: SocketFrame::Text(encoded),
        })
        .await
    }

    async fn deliver_session_frame(
        &self,
        delivery: ClusterSessionDelivery,
    ) -> Result<(), ProtocolError> {
        let local = self
            .inner
            .terminal_sessions
            .lock()
            .await
            .contains_key(&delivery.session_id);
        if local {
            return self.handle_cluster_session_delivery(delivery).await;
        }
        let target = ClusterBus::route_target(&delivery.session_id)
            .ok_or_else(|| ProtocolError::new("session_not_found", &delivery.session_id))?;
        if target == self.inner.cluster.instance_id() {
            return Err(ProtocolError::new("session_not_found", delivery.session_id));
        }
        self.inner.cluster.deliver_session(&target, delivery).await
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
            Some(connection) => Err(ProtocolError::new(
                "duplicate_machine_connection",
                format!(
                    "Controller {} currently owns machine {server_id}",
                    connection.controller_instance_id
                ),
            )),
            None => Err(ProtocolError::new(
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
        self.publish_workspace_event(event.clone());
        Ok(event)
    }

    fn publish_workspace_event(&self, event: WorkspaceEvent) {
        let _ = self.inner.events.send(event.clone());
        let envelope = DomainEventEnvelope {
            event_id: format!("evt_{}", Uuid::new_v4().simple()),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            organization_id: None,
            workspace_id: event.workspace_id.clone(),
            actor: DomainEventActor {
                kind: "system".to_string(),
                id: Some("treer-proxy".to_string()),
            },
            action: event.event,
            resource: DomainEventResource {
                kind: "workspace".to_string(),
                id: event.workspace_id,
            },
            occurred_at: Utc::now(),
            trace_id: None,
            causation_id: None,
            correlation_id: None,
            workspace_revision: Some(event.revision),
            payload: event.data,
        };
        if let Err(error) = self.inner.event_bus.publish(envelope) {
            warn!(%error, "domain event could not be queued");
        }
    }
}

fn remove_network_stream(
    streams: &mut HashMap<NetworkStreamKey, NetworkStreamLeg>,
    key: &NetworkStreamKey,
) -> Option<NetworkStreamLeg> {
    let stream = streams.remove(key)?;
    streams.remove(&stream.peer);
    Some(stream)
}

fn decode_network_reset(frame: &NetworkBinaryFrame) -> ProtocolError {
    serde_json::from_slice::<ProtocolError>(&frame.payload)
        .unwrap_or_else(|_| ProtocolError::new("network_stream_reset", "network stream was reset"))
}

fn proxy_message_frame(message: &ProxyMessage) -> Result<SocketFrame, ProtocolError> {
    let encoded = serde_json::to_string(message).map_err(|error| {
        ProtocolError::new(
            "encode_error",
            format!("failed to encode terminal message: {error}"),
        )
    })?;
    Ok(SocketFrame::Text(encoded))
}

fn send_terminal_to_browser(outgoing: &mpsc::Sender<SocketFrame>, message: &TerminalServerMessage) {
    if let Ok(encoded) = serde_json::to_string(message) {
        let _ = outgoing.try_send(SocketFrame::Text(encoded));
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
            SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
                panic!("expected text socket frame")
            }
        }
    }

    fn expect_network(frame: SocketFrame) -> NetworkBinaryFrame {
        match frame {
            SocketFrame::Binary(encoded) => {
                NetworkBinaryFrame::decode(&encoded).expect("decode network frame")
            }
            SocketFrame::Text(_) => panic!("expected binary network frame"),
            SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
                panic!("expected binary network frame")
            }
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
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        }
    }

    #[tokio::test]
    async fn workspace_renames_update_snapshots_and_live_events() {
        let state = AppState::new();
        let original = state.ensure_workspace("alpha", "Original").await;
        let mut events = state.subscribe();
        let renamed = WorkspaceInfo {
            name: "Renamed".to_string(),
            ..original
        };

        state
            .rename_workspace_info(renamed.clone())
            .await
            .expect("rename workspace");
        assert_eq!(
            state.snapshot("alpha").await.expect("snapshot").workspace,
            renamed
        );
        let event = events.recv().await.expect("workspace rename event");
        assert_eq!(event.event, "workspace.renamed");
        assert_eq!(event.data["name"], "Renamed");
        state.ensure_workspace("alpha", "alpha").await;
        assert_eq!(
            state
                .snapshot("alpha")
                .await
                .expect("snapshot after runtime ensure")
                .workspace,
            renamed
        );

        let replicated = WorkspaceInfo {
            name: "Replicated".to_string(),
            ..renamed
        };
        state
            .apply_cluster_projection(ClusterProjectionUpdate::WorkspaceUpsert {
                workspace: replicated.clone(),
            })
            .await;
        assert_eq!(
            state.snapshot("alpha").await.expect("snapshot").workspace,
            replicated
        );
        assert_eq!(
            events.recv().await.expect("replicated rename event").event,
            "workspace.renamed"
        );
    }

    #[tokio::test]
    async fn proxy_messages_broadcast_only_to_their_workspace() {
        let state = AppState::new();
        let (alpha_tx, mut alpha_rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), Uuid::new_v4(), alpha_tx)
            .await
            .expect("register alpha controller");
        let mut beta = test_server();
        beta.workspace_id = "beta".to_string();
        let (beta_tx, mut beta_rx) = mpsc::unbounded_channel();
        state
            .register_server(beta, Uuid::new_v4(), beta_tx)
            .await
            .expect("register beta controller");
        let message = ProxyMessage::VirtualNetworkHosts {
            snapshot: treer_protocol::VirtualNetworkHostsSnapshot {
                workspace_id: "alpha".to_string(),
                revision: 4,
                hosts: Vec::new(),
            },
        };

        state.broadcast_proxy_message("alpha", &message).await;

        let received: ProxyMessage = serde_json::from_str(&expect_text(
            alpha_rx.recv().await.expect("alpha virtual-host snapshot"),
        ))
        .expect("decode virtual-host snapshot");
        assert_eq!(received, message);
        assert!(beta_rx.try_recv().is_err());
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
            interface: None,
        }
    }

    #[tokio::test]
    async fn platform_agent_count_sums_current_workspace_agents() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        state.ensure_workspace("beta", "Beta").await;
        let mut beta_agent = test_agent("agent-beta", "Beta agent");
        beta_agent.workspace_id = "beta".to_string();
        {
            let mut workspaces = state.inner.workspaces.write().await;
            workspaces
                .get_mut("alpha")
                .expect("alpha workspace")
                .agents
                .insert(
                    "agent-alpha".to_string(),
                    test_agent("agent-alpha", "Alpha agent"),
                );
            workspaces
                .get_mut("beta")
                .expect("beta workspace")
                .agents
                .insert("agent-beta".to_string(), beta_agent);
        }

        assert_eq!(state.platform_agent_count().await, 2);
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
    async fn workspace_mutations_publish_versioned_domain_events() {
        let event_bus = EventBus::in_process();
        let mut events = event_bus.subscribe();
        let state = AppState::with_event_bus(event_bus);
        let (outgoing, _incoming) = mpsc::unbounded_channel();

        state
            .register_server(test_server(), Uuid::new_v4(), outgoing)
            .await
            .expect("register server");

        let event = events.recv().await.expect("domain event");
        assert_eq!(event.schema_version, DOMAIN_EVENT_SCHEMA_VERSION);
        assert_eq!(event.workspace_id, "alpha");
        assert_eq!(event.action, "server.updated");
        assert_eq!(event.resource.kind, "workspace");
        assert_eq!(event.resource.id, "alpha");
        assert_eq!(event.workspace_revision, Some(1));
        assert_eq!(event.payload["server_id"], "server");
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

        let (terminal_tx, mut terminal_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
        state
            .attach_terminal("alpha", "agent-1", 120, 40, None, terminal_tx)
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
    async fn reenrollment_clears_a_deleted_machine_tombstone() {
        let state = AppState::new();
        let server = test_server();
        let connection_id = Uuid::new_v4();
        let (server_tx, _server_rx) = mpsc::unbounded_channel();
        state
            .register_server(server.clone(), connection_id, server_tx)
            .await
            .expect("register machine");
        state
            .delete_server("alpha", "server")
            .await
            .expect("delete machine");

        let (blocked_tx, _blocked_rx) = mpsc::unbounded_channel();
        assert_eq!(
            state
                .register_server(server.clone(), Uuid::new_v4(), blocked_tx)
                .await
                .expect_err("deleted machine remains blocked")
                .code,
            "server_deleted"
        );

        state.allow_server_reenrollment("alpha", "server").await;
        let (reenrolled_tx, _reenrolled_rx) = mpsc::unbounded_channel();
        state
            .register_server(server, Uuid::new_v4(), reenrolled_tx)
            .await
            .expect("reenrolled machine reconnects");
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
    async fn replacement_controller_fences_the_old_local_connection() {
        let state = AppState::new();
        let server = test_server();
        let first_connection = Uuid::new_v4();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        state
            .register_server_instance(
                server.clone(),
                first_connection,
                "ctl_11111111111111111111111111111111".to_string(),
                first_tx,
            )
            .await
            .expect("register first controller");

        let second_connection = Uuid::new_v4();
        let (second_tx, _second_rx) = mpsc::unbounded_channel();
        state
            .register_server_instance(
                server,
                second_connection,
                "ctl_22222222222222222222222222222222".to_string(),
                second_tx,
            )
            .await
            .expect("register replacement controller");

        let error: ProxyMessage = serde_json::from_str(&expect_text(
            first_rx.recv().await.expect("replacement error"),
        ))
        .expect("decode replacement error");
        assert!(matches!(
            error,
            ProxyMessage::Error { error }
                if error.code == "duplicate_machine_connection"
        ));
        assert_eq!(first_rx.recv().await, Some(SocketFrame::Close));
        assert_eq!(
            state
                .heartbeat("alpha", "server", first_connection)
                .await
                .expect_err("old connection must be fenced")
                .code,
            "duplicate_machine_connection"
        );
        state
            .heartbeat("alpha", "server", second_connection)
            .await
            .expect("replacement owns the machine");
    }

    #[tokio::test]
    async fn original_controller_can_reclaim_after_the_replacement_disconnects() {
        let state = AppState::new();
        let server = test_server();
        let first_connection = Uuid::new_v4();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        state
            .register_server_instance(
                server.clone(),
                first_connection,
                "ctl_11111111111111111111111111111111".to_string(),
                first_tx,
            )
            .await
            .expect("register first controller");

        let second_connection = Uuid::new_v4();
        let (second_tx, _second_rx) = mpsc::unbounded_channel();
        state
            .register_server_instance(
                server.clone(),
                second_connection,
                "ctl_22222222222222222222222222222222".to_string(),
                second_tx,
            )
            .await
            .expect("register replacement controller");
        assert!(first_rx.recv().await.is_some());
        assert_eq!(first_rx.recv().await, Some(SocketFrame::Close));

        state
            .disconnect_server("alpha", "server", second_connection)
            .await;

        let reclaim_connection = Uuid::new_v4();
        let (reclaim_tx, _reclaim_rx) = mpsc::unbounded_channel();
        state
            .register_server_instance(
                server,
                reclaim_connection,
                "ctl_11111111111111111111111111111111".to_string(),
                reclaim_tx,
            )
            .await
            .expect("original controller reclaims the machine");
        state
            .heartbeat("alpha", "server", reclaim_connection)
            .await
            .expect("reclaimed connection owns the machine");
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
    async fn same_machine_network_streams_use_distinct_leg_ids() {
        let state = AppState::new();
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), connection_id, server_tx)
            .await
            .expect("register controller");

        let source_stream_id = "net_source".to_string();
        state
            .open_network_stream(
                "alpha",
                "server",
                connection_id,
                "server",
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Open,
                    stream_id: source_stream_id.clone(),
                    payload: b"connect".to_vec(),
                },
            )
            .await
            .expect("open same-machine stream");

        let destination_open =
            expect_network(server_rx.recv().await.expect("destination open frame"));
        assert_eq!(destination_open.kind, NetworkBinaryKind::Open);
        assert_ne!(destination_open.stream_id, source_stream_id);
        assert_eq!(destination_open.payload, b"connect");
        let destination_stream_id = destination_open.stream_id;
        assert_eq!(state.inner.network_streams.lock().await.len(), 2);

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Opened,
                    stream_id: destination_stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("relay destination opened frame");
        let source_opened = expect_network(server_rx.recv().await.expect("source opened frame"));
        assert_eq!(source_opened.kind, NetworkBinaryKind::Opened);
        assert_eq!(source_opened.stream_id, source_stream_id);

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: source_stream_id.clone(),
                    payload: b"request".to_vec(),
                },
            )
            .await
            .expect("relay source data");
        let destination_data =
            expect_network(server_rx.recv().await.expect("destination data frame"));
        assert_eq!(destination_data.stream_id, destination_stream_id);
        assert_eq!(destination_data.payload, b"request");

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: destination_stream_id.clone(),
                    payload: b"response".to_vec(),
                },
            )
            .await
            .expect("relay destination data");
        let source_data = expect_network(server_rx.recv().await.expect("source data frame"));
        assert_eq!(source_data.stream_id, source_stream_id);
        assert_eq!(source_data.payload, b"response");

        for (stream_id, expected_peer_id) in [
            (source_stream_id.clone(), destination_stream_id.clone()),
            (destination_stream_id, source_stream_id.clone()),
        ] {
            state
                .relay_network_frame(
                    "alpha",
                    "server",
                    connection_id,
                    NetworkBinaryFrame {
                        kind: NetworkBinaryKind::HalfClose,
                        stream_id,
                        payload: Vec::new(),
                    },
                )
                .await
                .expect("relay half-close");
            let close = expect_network(server_rx.recv().await.expect("peer half-close frame"));
            assert_eq!(close.kind, NetworkBinaryKind::HalfClose);
            assert_eq!(close.stream_id, expected_peer_id);
        }
        assert!(state.inner.network_streams.lock().await.is_empty());
    }

    #[tokio::test]
    async fn relayed_payload_is_counted_once_in_machine_direction() {
        let traffic = TrafficRecorder::default();
        let state = AppState::with_backplanes_and_traffic(
            EventBus::in_process(),
            ClusterBus::standalone("traffic-test".to_string()),
            traffic.clone(),
        );
        let source_connection = Uuid::new_v4();
        let destination_connection = Uuid::new_v4();
        let (source_tx, mut source_rx) = mpsc::unbounded_channel();
        let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
        let mut source = test_server();
        source.server_id = "source".to_string();
        source.name = "source".to_string();
        let mut destination = test_server();
        destination.server_id = "destination".to_string();
        destination.name = "destination".to_string();
        state
            .register_server(source, source_connection, source_tx)
            .await
            .expect("register source");
        state
            .register_server(destination, destination_connection, destination_tx)
            .await
            .expect("register destination");

        state
            .open_network_stream(
                "alpha",
                "source",
                source_connection,
                "destination",
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Open,
                    stream_id: "source-stream".to_string(),
                    payload: b"connect".to_vec(),
                },
            )
            .await
            .expect("open stream");
        let destination_open =
            expect_network(destination_rx.recv().await.expect("destination open"));

        state
            .relay_network_frame(
                "alpha",
                "source",
                source_connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: "source-stream".to_string(),
                    payload: b"request".to_vec(),
                },
            )
            .await
            .expect("relay request");
        let _ = destination_rx.recv().await.expect("destination data");
        state
            .relay_network_frame(
                "alpha",
                "destination",
                destination_connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: destination_open.stream_id,
                    payload: b"response".to_vec(),
                },
            )
            .await
            .expect("relay response");
        let _ = source_rx.recv().await.expect("source data");

        assert_eq!(
            traffic.pending_for(
                "alpha",
                TrafficClass::VirtualNetwork,
                "source",
                "destination"
            ),
            (7, 1)
        );
        assert_eq!(
            traffic.pending_for(
                "alpha",
                TrafficClass::VirtualNetwork,
                "destination",
                "source"
            ),
            (8, 1)
        );
    }

    #[tokio::test]
    async fn direct_network_routes_do_not_create_proxy_stream_legs() {
        let state = AppState::new();
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), connection_id, server_tx)
            .await
            .expect("register controller");

        let target = NetworkDirectTarget {
            host: "example.com".to_string(),
            port: 443,
        };
        state
            .send_direct_network_route(
                "alpha",
                "server",
                connection_id,
                "net_source".to_string(),
                target.clone(),
            )
            .await
            .expect("send direct route");

        let route = expect_network(server_rx.recv().await.expect("direct route frame"));
        assert_eq!(route.kind, NetworkBinaryKind::Direct);
        assert_eq!(route.stream_id, "net_source");
        assert_eq!(
            serde_json::from_slice::<NetworkDirectTarget>(&route.payload)
                .expect("decode direct target"),
            target
        );
        assert!(state.inner.network_streams.lock().await.is_empty());
    }

    #[tokio::test]
    async fn browser_network_streams_bridge_data_and_close_cleanly() {
        let traffic = TrafficRecorder::default();
        let state = AppState::with_backplanes_and_traffic(
            EventBus::in_process(),
            ClusterBus::standalone("browser-traffic-test".to_string()),
            traffic.clone(),
        );
        let connection_id = Uuid::new_v4();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        state
            .register_server(test_server(), connection_id, server_tx)
            .await
            .expect("register controller");

        let opening_state = state.clone();
        let opening = tokio::spawn(async move {
            opening_state
                .open_browser_network_stream(
                    "alpha",
                    "server",
                    Some("agent-a"),
                    "127.0.0.1",
                    8080,
                    TrafficClass::AgentInterface,
                )
                .await
        });
        let open = expect_network(server_rx.recv().await.expect("browser open frame"));
        assert_eq!(open.kind, NetworkBinaryKind::Open);
        let request: NetworkConnectRequest =
            serde_json::from_slice(&open.payload).expect("decode browser connect request");
        assert_eq!(request.host, "127.0.0.1");
        assert_eq!(request.port, 8080);
        assert_eq!(request.source_server_id, "browser");
        assert_eq!(request.destination_agent_id.as_deref(), Some("agent-a"));

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open browser stream");
        let mut browser = opening
            .await
            .expect("join browser open")
            .expect("browser stream");

        browser.write_all(b"request").await.expect("write request");
        let request_data = expect_network(server_rx.recv().await.expect("request data frame"));
        assert_eq!(request_data.kind, NetworkBinaryKind::Data);
        assert_eq!(request_data.stream_id, open.stream_id);
        assert_eq!(request_data.payload, b"request");

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Data,
                    stream_id: open.stream_id.clone(),
                    payload: b"response".to_vec(),
                },
            )
            .await
            .expect("relay response");
        let mut response = [0_u8; 8];
        browser
            .read_exact(&mut response)
            .await
            .expect("read response");
        assert_eq!(&response, b"response");
        let window = expect_network(server_rx.recv().await.expect("window update"));
        assert_eq!(window.kind, NetworkBinaryKind::WindowUpdate);

        state
            .relay_network_frame(
                "alpha",
                "server",
                connection_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("relay remote close");
        browser.shutdown().await.expect("close browser stream");
        let close = expect_network(server_rx.recv().await.expect("browser close frame"));
        assert_eq!(close.kind, NetworkBinaryKind::HalfClose);
        tokio::task::yield_now().await;
        assert!(state.inner.browser_network_streams.lock().await.is_empty());
        assert_eq!(
            traffic.pending_for(
                "alpha",
                TrafficClass::AgentInterface,
                BROWSER_TRAFFIC_ENDPOINT,
                "server"
            ),
            (7, 1)
        );
        assert_eq!(
            traffic.pending_for(
                "alpha",
                TrafficClass::AgentInterface,
                "server",
                BROWSER_TRAFFIC_ENDPOINT
            ),
            (8, 1)
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
        let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
        let session_id = state
            .attach_terminal("alpha", "agent-1", 120, 40, None, browser_tx)
            .await
            .expect("attach terminal");
        let attach: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("terminal attach message"),
        ))
        .expect("decode attach");
        assert!(matches!(
            attach,
            ProxyMessage::TerminalAttach {
                session_id: ref attached,
                cursor: None,
                ..
            } if attached == &session_id
        ));

        let replay = vec![b'x'; TERMINAL_REPLAY_CHUNK_BYTES + 1];
        state
            .terminal_ready(
                "alpha",
                "server",
                connection_id,
                &session_id,
                TerminalReadyPayload {
                    revision: 7,
                    replay: replay.clone(),
                    stream_epoch: Some("stream_a".to_string()),
                    gap: false,
                },
            )
            .await
            .expect("terminal ready");
        let ready: TerminalServerMessage =
            serde_json::from_str(&expect_text(browser_rx.recv().await.expect("ready frame")))
                .expect("decode ready");
        assert_eq!(
            ready,
            TerminalServerMessage::Ready {
                session_id: session_id.clone(),
                stream_epoch: Some("stream_a".to_string()),
                revision: Some(7),
                gap: false,
                replay_chunks: Some(2),
            }
        );
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(
                replay[..TERMINAL_REPLAY_CHUNK_BYTES].to_vec()
            ))
        );
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(
                replay[TERMINAL_REPLAY_CHUNK_BYTES..].to_vec()
            ))
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
        let cursor: TerminalServerMessage =
            serde_json::from_str(&expect_text(browser_rx.recv().await.expect("cursor frame")))
                .expect("decode cursor");
        assert_eq!(
            cursor,
            TerminalServerMessage::Cursor {
                stream_epoch: "stream_a".to_string(),
                revision: 8,
            }
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
            SocketFrame::Ping(_) | SocketFrame::Pong(_) | SocketFrame::Close => {
                panic!("expected binary terminal input")
            }
        };
        assert_eq!(input.kind, TerminalBinaryKind::Input);
        assert_eq!(input.session_id, session_id);
        assert_eq!(input.payload, vec![0, 0xff, b'\r']);
    }

    #[tokio::test]
    async fn slow_terminal_consumer_is_detached_when_its_queue_fills() {
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
        let (browser_tx, mut browser_rx) = mpsc::channel(1);
        let session_id = state
            .attach_terminal("alpha", "agent-1", 120, 40, None, browser_tx)
            .await
            .expect("attach terminal");
        let _attach = server_rx.recv().await.expect("terminal attach");
        state
            .inner
            .terminal_sessions
            .lock()
            .await
            .get_mut(&session_id)
            .expect("terminal session")
            .stream_epoch = Some("stream_a".to_string());

        state
            .terminal_output(
                "alpha",
                "server",
                connection_id,
                &session_id,
                1,
                b"output".to_vec(),
            )
            .await
            .expect("detach overloaded terminal without blocking controller");

        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(b"output".to_vec()))
        );
        assert!(!state
            .inner
            .terminal_sessions
            .lock()
            .await
            .contains_key(&session_id));
        let detach: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("terminal detach"),
        ))
        .expect("decode terminal detach");
        assert_eq!(
            detach,
            ProxyMessage::TerminalDetach {
                session_id: session_id.clone(),
            }
        );
    }

    #[tokio::test]
    async fn terminal_attach_forwards_a_stream_cursor() {
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
        let (browser_tx, _browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
        let cursor = TerminalCursor {
            stream_epoch: "stream_a".to_string(),
            revision: 12,
        };
        state
            .attach_terminal("alpha", "agent-1", 80, 24, Some(cursor.clone()), browser_tx)
            .await
            .expect("attach terminal");
        let attach: ProxyMessage = serde_json::from_str(&expect_text(
            server_rx.recv().await.expect("terminal attach message"),
        ))
        .expect("decode attach");
        match attach {
            ProxyMessage::TerminalAttach {
                cursor: Some(attached),
                ..
            } => assert_eq!(attached, cursor),
            other => panic!("expected attach with cursor, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cluster_name_projections_survive_snapshot_replay_ordering() {
        let state = AppState::new();
        state.ensure_workspace("alpha", "Alpha").await;
        state
            .apply_cluster_projection(ClusterProjectionUpdate::ServerRenamed {
                workspace_id: "alpha".to_string(),
                server_id: "server".to_string(),
                name: "persisted-server-name".to_string(),
            })
            .await;
        state
            .apply_cluster_projection(ClusterProjectionUpdate::AgentRenamed {
                workspace_id: "alpha".to_string(),
                agent_id: "agent".to_string(),
                name: "persisted-agent-name".to_string(),
            })
            .await;

        let now = Utc::now();
        state
            .apply_cluster_snapshot(ClusterServerSnapshot {
                owner: crate::cluster::ConnectionOwner {
                    proxy_id: "remote-proxy".to_string(),
                    connection_id: Uuid::new_v4(),
                },
                revision: 1,
                snapshot: AgentServerSnapshot {
                    server: test_server(),
                    agents: vec![AgentInfo {
                        agent_id: "agent".to_string(),
                        workspace_id: "alpha".to_string(),
                        server_id: "server".to_string(),
                        kind: "command".to_string(),
                        name: "stale-agent-name".to_string(),
                        cwd: ".".to_string(),
                        status: AgentStatus::Idle,
                        pid: None,
                        started_at: now,
                        updated_at: now,
                        exited_at: None,
                        exit_code: None,
                        output_revision: 0,
                        interface: None,
                    }],
                },
            })
            .await;

        assert_eq!(
            state
                .resolve_server("alpha", "server")
                .await
                .expect("replayed server")
                .name,
            "persisted-server-name"
        );
        assert_eq!(
            state
                .resolve_agent("alpha", "agent")
                .await
                .expect("replayed agent")
                .name,
            "persisted-agent-name"
        );
    }

    #[tokio::test]
    async fn nats_cluster_routes_projection_commands_terminal_and_network() {
        let Ok(nats_url) = std::env::var("TREER_TEST_NATS_URL") else {
            return;
        };
        let suffix = Uuid::new_v4().simple().to_string();
        let workspace_id = format!("workspace-{suffix}");
        let source_id = format!("source-{suffix}");
        let destination_id = format!("destination-{suffix}");
        let subject_prefix = format!("treer.test.cluster.{suffix}");
        let bus_a = ClusterBus::connect(
            &nats_url,
            format!("proxy-a-{suffix}"),
            subject_prefix.clone(),
        )
        .await
        .expect("connect proxy A cluster bus");
        let bus_b = ClusterBus::connect(&nats_url, format!("proxy-b-{suffix}"), subject_prefix)
            .await
            .expect("connect proxy B cluster bus");
        let state_a = AppState::with_backplanes(EventBus::in_process(), bus_a.clone());
        let state_b = AppState::with_backplanes(EventBus::in_process(), bus_b.clone());
        let workspace = state_a
            .ensure_workspace(&workspace_id, "Cluster test")
            .await;
        bus_a
            .start(state_a.clone())
            .await
            .expect("start proxy A bus");
        bus_a
            .broadcast_projection(ClusterProjectionUpdate::WorkspaceUpsert { workspace })
            .await
            .expect("persist workspace projection before proxy B starts");
        bus_b
            .start(state_b.clone())
            .await
            .expect("start proxy B bus");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state_b.snapshot(&workspace_id).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("late proxy replays durable workspace projection");

        let now = Utc::now();
        let source_connection = Uuid::new_v4();
        let destination_connection = Uuid::new_v4();
        let (source_tx, mut source_rx) = mpsc::unbounded_channel();
        let source = ServerInfo {
            server_id: source_id.clone(),
            workspace_id: workspace_id.clone(),
            name: "source".to_string(),
            hostname: "source".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        state_a
            .register_server(source, source_connection, source_tx)
            .await
            .expect("register source on proxy A");

        let (destination_tx, mut destination_rx) = mpsc::unbounded_channel();
        let destination = ServerInfo {
            server_id: destination_id.clone(),
            workspace_id: workspace_id.clone(),
            name: "destination".to_string(),
            hostname: "destination".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        };
        state_b
            .register_server(destination.clone(), destination_connection, destination_tx)
            .await
            .expect("register destination on proxy B");
        let agent_id = format!("agent-{suffix}");
        state_b
            .apply_snapshot(
                destination_connection,
                AgentServerSnapshot {
                    server: destination,
                    agents: vec![AgentInfo {
                        agent_id: agent_id.clone(),
                        workspace_id: workspace_id.clone(),
                        server_id: destination_id.clone(),
                        kind: "command".to_string(),
                        name: "remote-agent".to_string(),
                        cwd: ".".to_string(),
                        status: AgentStatus::Idle,
                        pid: None,
                        started_at: now,
                        updated_at: now,
                        exited_at: None,
                        exit_code: None,
                        output_revision: 0,
                        interface: None,
                    }],
                },
            )
            .await
            .expect("publish destination snapshot");

        let replicated = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state_a
                    .resolve_agent(&workspace_id, &agent_id)
                    .await
                    .is_ok()
                    && state_b
                        .resolve_server(&workspace_id, &source_id)
                        .await
                        .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if replicated.is_err() {
            panic!(
                "replicate live projections: proxy_a={:?}, proxy_b={:?}",
                state_a.snapshot(&workspace_id).await,
                state_b.snapshot(&workspace_id).await
            );
        }

        state_a
            .rename_agent(&workspace_id, &agent_id, "renamed-remote-agent".to_string())
            .await
            .expect("rename agent through proxy A");
        state_a
            .rename_server(
                &workspace_id,
                &destination_id,
                "renamed-destination".to_string(),
            )
            .await
            .expect("rename server through proxy A");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let agent_name = state_b
                    .resolve_agent(&workspace_id, &agent_id)
                    .await
                    .map(|agent| agent.name);
                let server_name = state_b
                    .resolve_server(&workspace_id, &destination_id)
                    .await
                    .map(|server| server.name);
                if agent_name.as_deref() == Ok("renamed-remote-agent")
                    && server_name.as_deref() == Ok("renamed-destination")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("replicate rename projection updates");

        let command_state = state_a.clone();
        let command_workspace = workspace_id.clone();
        let command_server = destination_id.clone();
        let command = tokio::spawn(async move {
            command_state
                .send_command(
                    &command_workspace,
                    &command_server,
                    AgentCommand::ShutdownMachine,
                )
                .await
        });
        let command_message: ProxyMessage = serde_json::from_str(&expect_text(
            destination_rx.recv().await.expect("cross-proxy command"),
        ))
        .expect("decode cross-proxy command");
        let ProxyMessage::Command { envelope } = command_message else {
            panic!("expected command envelope");
        };
        state_b
            .complete_command(CommandResult::success(
                envelope.command_id,
                serde_json::json!({"accepted": true}),
            ))
            .await;
        assert_eq!(
            command
                .await
                .expect("join command")
                .expect("command result"),
            serde_json::json!({"accepted": true})
        );

        let (browser_tx, mut browser_rx) = mpsc::channel(TERMINAL_BROWSER_QUEUE_CAPACITY);
        let session_id = state_a
            .attach_terminal(&workspace_id, &agent_id, 100, 30, None, browser_tx)
            .await
            .expect("attach across proxies");
        let attach: ProxyMessage = serde_json::from_str(&expect_text(
            destination_rx.recv().await.expect("remote terminal attach"),
        ))
        .expect("decode terminal attach");
        assert!(
            matches!(attach, ProxyMessage::TerminalAttach { session_id: attached, .. } if attached == session_id)
        );
        state_b
            .terminal_ready(
                &workspace_id,
                &destination_id,
                destination_connection,
                &session_id,
                TerminalReadyPayload {
                    revision: 4,
                    replay: b"replay".to_vec(),
                    stream_epoch: Some("stream_b".to_string()),
                    gap: false,
                },
            )
            .await
            .expect("return terminal replay across proxies");
        assert!(matches!(
            browser_rx.recv().await,
            Some(SocketFrame::Text(_))
        ));
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(b"replay".to_vec()))
        );
        state_b
            .terminal_output(
                &workspace_id,
                &destination_id,
                destination_connection,
                &session_id,
                5,
                b"live".to_vec(),
            )
            .await
            .expect("return live terminal output across proxies");
        assert_eq!(
            browser_rx.recv().await,
            Some(SocketFrame::Binary(b"live".to_vec()))
        );
        let cursor: TerminalServerMessage = serde_json::from_str(&expect_text(
            browser_rx.recv().await.expect("cross-proxy cursor"),
        ))
        .expect("decode cross-proxy cursor");
        assert_eq!(
            cursor,
            TerminalServerMessage::Cursor {
                stream_epoch: "stream_b".to_string(),
                revision: 5,
            }
        );
        state_a
            .terminal_input(&session_id, b"hello".to_vec())
            .await
            .expect("route terminal input across proxies");
        assert!(matches!(
            destination_rx.recv().await,
            Some(SocketFrame::Binary(_))
        ));

        let source_stream_id = format!("source-stream-{suffix}");
        state_a
            .open_network_stream(
                &workspace_id,
                &source_id,
                source_connection,
                &destination_id,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Open,
                    stream_id: source_stream_id.clone(),
                    payload: b"connect".to_vec(),
                },
            )
            .await
            .expect("open cross-proxy network stream");
        let destination_open =
            expect_network(destination_rx.recv().await.expect("destination open"));
        assert_eq!(destination_open.kind, NetworkBinaryKind::Open);
        state_b
            .relay_network_frame(
                &workspace_id,
                &destination_id,
                destination_connection,
                NetworkBinaryFrame {
                    kind: NetworkBinaryKind::Opened,
                    stream_id: destination_open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("return opened frame to coordinating proxy");
        let opened = expect_network(source_rx.recv().await.expect("source opened"));
        assert_eq!(opened.kind, NetworkBinaryKind::Opened);
        assert_eq!(opened.stream_id, source_stream_id);

        state_b
            .disconnect_server(&workspace_id, &destination_id, destination_connection)
            .await;
        let terminal_closed = tokio::time::timeout(Duration::from_secs(5), browser_rx.recv())
            .await
            .expect("remote disconnect closes coordinating terminal")
            .expect("terminal close frame");
        let closed: TerminalServerMessage =
            serde_json::from_str(&expect_text(terminal_closed)).expect("decode terminal close");
        assert!(matches!(closed, TerminalServerMessage::Closed { .. }));
        let network_reset = tokio::time::timeout(Duration::from_secs(5), source_rx.recv())
            .await
            .expect("remote disconnect resets coordinating network stream")
            .expect("network reset frame");
        assert_eq!(expect_network(network_reset).kind, NetworkBinaryKind::Reset);
        state_a
            .disconnect_server(&workspace_id, &source_id, source_connection)
            .await;
    }
}
