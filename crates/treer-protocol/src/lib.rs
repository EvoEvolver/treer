use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Working,
    Idle,
    Blocked,
    Exited,
    Failed,
    Unknown,
}

impl AgentStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub server_id: String,
    pub workspace_id: String,
    pub hostname: String,
    pub root: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub status: ServerStatus,
    pub connected_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub kind: String,
    pub name: String,
    pub cwd: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub output_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub revision: u64,
    pub workspace: WorkspaceInfo,
    pub servers: Vec<ServerInfo>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

const fn default_cols() -> u16 {
    120
}

const fn default_rows() -> u16 {
    36
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAgentRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAgentRequest {
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAgentOutputResponse {
    pub agent_id: String,
    pub revision: u64,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentServerSnapshot {
    pub server: ServerInfo,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentCommand {
    Create {
        agent_id: String,
        request: CreateAgentRequest,
    },
    Prompt {
        agent_id: String,
        text: String,
    },
    Input {
        agent_id: String,
        data: String,
    },
    Read {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    Stop {
        agent_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub command_id: String,
    pub workspace_id: String,
    pub command: AgentCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

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
        protocol: u32,
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
    TerminalReady {
        session_id: String,
        replay: String,
    },
    TerminalOutput {
        session_id: String,
        data: String,
    },
    TerminalClosed {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProxyMessage {
    Registered {
        protocol: u32,
        workspace_revision: u64,
    },
    Command {
        envelope: CommandEnvelope,
    },
    TerminalAttach {
        session_id: String,
        agent_id: String,
        cols: u16,
        rows: u16,
    },
    TerminalInput {
        session_id: String,
        data: String,
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Ready { session_id: String, replay: String },
    Output { data: String },
    Closed { reason: Option<String> },
    Error { error: ProtocolError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEvent {
    pub revision: u64,
    pub workspace_id: String,
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ProtocolError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_wire_shape_is_stable() {
        let message = ProxyMessage::Command {
            envelope: CommandEnvelope {
                command_id: "cmd_1".to_string(),
                workspace_id: "default".to_string(),
                command: AgentCommand::Stop {
                    agent_id: "ag_1".to_string(),
                },
            },
        };

        let json = serde_json::to_value(message).expect("serialize command");
        assert_eq!(json["type"], "command");
        assert_eq!(json["envelope"]["command"]["action"], "stop");
    }

    #[test]
    fn terminal_input_wire_shape_is_stable() {
        let message = TerminalClientMessage::Resize {
            cols: 140,
            rows: 48,
        };
        assert_eq!(
            serde_json::to_value(message).expect("serialize"),
            serde_json::json!({ "type": "resize", "cols": 140, "rows": 48 })
        );
    }
}
