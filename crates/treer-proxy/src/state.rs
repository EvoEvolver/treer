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
    capabilities: HashSet<String>,
    outgoing: mpsc::UnboundedSender<SocketFrame>,
}

struct PendingCommand {
    server: ServerKey,
    encoded: String,
    required_capability: Option<&'static str>,
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
    outgoing_agent_traffic: Option<std::sync::Arc<TrafficCounter>>,
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

mod cluster;
mod commands;
mod core;
mod network;
mod terminal;

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
#[path = "state/tests.rs"]
mod tests;
