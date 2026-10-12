use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl CommandResult {
    pub fn success(command_id: impl Into<String>, data: impl Serialize) -> Self {
        let data = serde_json::to_value(data).unwrap_or(Value::Null);
        Self {
            command_id: command_id.into(),
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(command_id: impl Into<String>, error: ProtocolError) -> Self {
        Self {
            command_id: command_id.into(),
            data: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentServerMessage {
    Register {
        /// Legacy single-version field. New peers negotiate from
        /// `supported_protocols`; old peers continue to read this value.
        protocol: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        supported_protocols: Vec<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        controller_instance_id: String,
        server: ServerInfo,
    },
    Snapshot {
        snapshot: AgentServerSnapshot,
    },
    Heartbeat {
        sent_at: DateTime<Utc>,
    },
    AgentEvent {
        agent: AgentInfo,
    },
    CommandResult {
        result: CommandResult,
    },
    TerminalClosed {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    TerminalReady {
        session_id: String,
        stream_epoch: String,
        revision: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        gap: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProxyMessage {
    Registered {
        protocol: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        workspace_revision: u64,
    },
    VirtualNetworkHosts {
        snapshot: VirtualNetworkHostsSnapshot,
    },
    Command {
        envelope: CommandEnvelope,
    },
    TerminalAttach {
        session_id: String,
        agent_id: String,
        cols: u16,
        rows: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<TerminalCursor>,
    },
    TerminalResize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    TerminalDetach {
        session_id: String,
    },
    Error {
        error: ProtocolError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCursor {
    pub stream_epoch: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    Resize { cols: u16, rows: u16 },
    Ack { bytes: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Ready {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_epoch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        gap: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_chunks: Option<u32>,
    },
    Cursor {
        stream_epoch: String,
        revision: u64,
    },
    Closed {
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    Error {
        error: ProtocolError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEvent {
    pub revision: u64,
    pub workspace_id: String,
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEventActor {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEventResource {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainEventEnvelope {
    pub event_id: String,
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    pub workspace_id: String,
    pub actor: DomainEventActor,
    pub action: String,
    pub resource: DomainEventResource,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_revision: Option<u64>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ProtocolError,
}
