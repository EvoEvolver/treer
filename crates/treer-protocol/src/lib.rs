use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MACHINE_ENROLLMENT_KEY_PREFIX: &str = "enr_v1_";
pub const TERMINAL_BINARY_VERSION: u8 = 1;
const TERMINAL_BINARY_HEADER_LEN: usize = 12;
pub const TRANSFER_BINARY_VERSION: u8 = 1;
const TRANSFER_BINARY_MAGIC: &[u8; 3] = b"TRF";
const TRANSFER_BINARY_HEADER_LEN: usize = 7;

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
    #[serde(default)]
    pub name: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRequest {
    pub name: String,
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
    pub data: Vec<u8>,
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
        data: Vec<u8>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TerminalBinaryKind {
    Ready = 1,
    Output = 2,
    Input = 3,
}

impl TryFrom<u8> for TerminalBinaryKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Ready),
            2 => Ok(Self::Output),
            3 => Ok(Self::Input),
            _ => Err(ProtocolError::new(
                "invalid_terminal_frame",
                format!("unknown terminal binary frame kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalBinaryFrame {
    pub kind: TerminalBinaryKind,
    pub session_id: String,
    pub revision: u64,
    pub payload: Vec<u8>,
}

impl TerminalBinaryFrame {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let session = self.session_id.as_bytes();
        let session_len = u16::try_from(session.len()).map_err(|_| {
            ProtocolError::new("invalid_terminal_frame", "terminal session id is too long")
        })?;
        if session.is_empty() {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal session id is empty",
            ));
        }
        let mut encoded =
            Vec::with_capacity(TERMINAL_BINARY_HEADER_LEN + session.len() + self.payload.len());
        encoded.push(TERMINAL_BINARY_VERSION);
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&session_len.to_be_bytes());
        encoded.extend_from_slice(&self.revision.to_be_bytes());
        encoded.extend_from_slice(session);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() < TERMINAL_BINARY_HEADER_LEN {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal binary frame is shorter than its header",
            ));
        }
        if encoded[0] != TERMINAL_BINARY_VERSION {
            return Err(ProtocolError::new(
                "terminal_binary_version_mismatch",
                format!(
                    "terminal binary frame uses version {}, expected {}",
                    encoded[0], TERMINAL_BINARY_VERSION
                ),
            ));
        }
        let kind = TerminalBinaryKind::try_from(encoded[1])?;
        let session_len = usize::from(u16::from_be_bytes([encoded[2], encoded[3]]));
        let payload_offset = TERMINAL_BINARY_HEADER_LEN
            .checked_add(session_len)
            .filter(|offset| *offset <= encoded.len())
            .ok_or_else(|| {
                ProtocolError::new(
                    "invalid_terminal_frame",
                    "terminal session id exceeds the binary frame",
                )
            })?;
        if session_len == 0 {
            return Err(ProtocolError::new(
                "invalid_terminal_frame",
                "terminal session id is empty",
            ));
        }
        let revision = u64::from_be_bytes(
            encoded[4..12]
                .try_into()
                .map_err(|_| ProtocolError::new("invalid_terminal_frame", "invalid revision"))?,
        );
        let session_id = std::str::from_utf8(&encoded[12..payload_offset])
            .map_err(|_| {
                ProtocolError::new("invalid_terminal_frame", "terminal session id is not UTF-8")
            })?
            .to_string();
        Ok(Self {
            kind,
            session_id,
            revision,
            payload: encoded[payload_offset..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransferBinaryKind {
    Entry = 1,
    Data = 2,
    EntryEnd = 3,
    TransferEnd = 4,
}

impl TryFrom<u8> for TransferBinaryKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Entry),
            2 => Ok(Self::Data),
            3 => Ok(Self::EntryEnd),
            4 => Ok(Self::TransferEnd),
            _ => Err(ProtocolError::new(
                "invalid_transfer_frame",
                format!("unknown transfer binary frame kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferBinaryFrame {
    pub kind: TransferBinaryKind,
    pub session_id: String,
    pub payload: Vec<u8>,
}

impl TransferBinaryFrame {
    pub fn is_transfer_frame(encoded: &[u8]) -> bool {
        encoded.starts_with(TRANSFER_BINARY_MAGIC)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let session = self.session_id.as_bytes();
        let session_len = u16::try_from(session.len()).map_err(|_| {
            ProtocolError::new("invalid_transfer_frame", "transfer session id is too long")
        })?;
        if session.is_empty() {
            return Err(ProtocolError::new(
                "invalid_transfer_frame",
                "transfer session id is empty",
            ));
        }
        let mut encoded =
            Vec::with_capacity(TRANSFER_BINARY_HEADER_LEN + session.len() + self.payload.len());
        encoded.extend_from_slice(TRANSFER_BINARY_MAGIC);
        encoded.push(TRANSFER_BINARY_VERSION);
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&session_len.to_be_bytes());
        encoded.extend_from_slice(session);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() < TRANSFER_BINARY_HEADER_LEN {
            return Err(ProtocolError::new(
                "invalid_transfer_frame",
                "transfer binary frame is shorter than its header",
            ));
        }
        if !Self::is_transfer_frame(encoded) {
            return Err(ProtocolError::new(
                "invalid_transfer_frame",
                "transfer binary frame has an invalid magic value",
            ));
        }
        if encoded[3] != TRANSFER_BINARY_VERSION {
            return Err(ProtocolError::new(
                "transfer_binary_version_mismatch",
                format!(
                    "transfer binary frame uses version {}, expected {}",
                    encoded[3], TRANSFER_BINARY_VERSION
                ),
            ));
        }
        let kind = TransferBinaryKind::try_from(encoded[4])?;
        let session_len = usize::from(u16::from_be_bytes([encoded[5], encoded[6]]));
        let payload_offset = TRANSFER_BINARY_HEADER_LEN
            .checked_add(session_len)
            .filter(|offset| *offset <= encoded.len())
            .ok_or_else(|| {
                ProtocolError::new(
                    "invalid_transfer_frame",
                    "transfer session id exceeds the binary frame",
                )
            })?;
        if session_len == 0 {
            return Err(ProtocolError::new(
                "invalid_transfer_frame",
                "transfer session id is empty",
            ));
        }
        let session_id = std::str::from_utf8(&encoded[7..payload_offset])
            .map_err(|_| {
                ProtocolError::new("invalid_transfer_frame", "transfer session id is not UTF-8")
            })?
            .to_string();
        Ok(Self {
            kind,
            session_id,
            payload: encoded[payload_offset..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEntry {
    pub path: String,
    pub kind: TransferEntryKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferStats {
    pub entries: u64,
    pub bytes: u64,
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
    TerminalClosed {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    TransferReady {
        session_id: String,
    },
    TransferProgress {
        session_id: String,
    },
    TransferComplete {
        session_id: String,
        stats: TransferStats,
    },
    TransferFailed {
        session_id: String,
        error: ProtocolError,
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
    ShellOpen {
        session_id: String,
        cols: u16,
        rows: u16,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    TerminalResize {
        session_id: String,
        cols: u16,
        rows: u16,
    },
    TerminalDetach {
        session_id: String,
    },
    ShellDetach {
        session_id: String,
    },
    TransferUpload {
        session_id: String,
        destination: String,
        recursive: bool,
    },
    TransferDownload {
        session_id: String,
        source: String,
        recursive: bool,
    },
    TransferCancel {
        session_id: String,
    },
    Error {
        error: ProtocolError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalClientMessage {
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalServerMessage {
    Ready {
        session_id: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransferServerMessage {
    Ready {
        session_id: String,
    },
    Progress {
        session_id: String,
    },
    Complete {
        session_id: String,
        stats: TransferStats,
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
pub struct ApiError {
    pub error: ProtocolError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineEnrollmentKey {
    pub identifier: String,
    pub workspace_id: String,
    pub enrollment_id: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEnrollmentResponse {
    pub workspace_id: String,
    pub server_id: String,
    pub machine_token: String,
}

pub fn format_machine_enrollment_key(
    workspace_id: &str,
    enrollment_id: &str,
    secret: &str,
) -> Result<String, ProtocolError> {
    if workspace_id.is_empty()
        || enrollment_id.is_empty()
        || !enrollment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ProtocolError::new(
            "invalid_machine_enrollment_key",
            "machine enrollment key components are invalid",
        ));
    }
    let workspace = encode_hex(workspace_id.as_bytes());
    Ok(format!(
        "{MACHINE_ENROLLMENT_KEY_PREFIX}{workspace}_{enrollment_id}.{secret}"
    ))
}

pub fn parse_machine_enrollment_key(value: &str) -> Result<MachineEnrollmentKey, ProtocolError> {
    let (identifier, secret) = value.split_once('.').ok_or_else(invalid_enrollment_key)?;
    let encoded = identifier
        .strip_prefix(MACHINE_ENROLLMENT_KEY_PREFIX)
        .ok_or_else(invalid_enrollment_key)?;
    let (workspace, enrollment_id) = encoded
        .rsplit_once('_')
        .ok_or_else(invalid_enrollment_key)?;
    if enrollment_id.is_empty()
        || !enrollment_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || secret.len() != 64
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_enrollment_key());
    }
    let workspace_id =
        String::from_utf8(decode_hex(workspace)?).map_err(|_| invalid_enrollment_key())?;
    if workspace_id.is_empty() {
        return Err(invalid_enrollment_key());
    }
    Ok(MachineEnrollmentKey {
        identifier: identifier.to_string(),
        workspace_id,
        enrollment_id: enrollment_id.to_string(),
        secret: secret.to_string(),
    })
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ProtocolError> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return Err(invalid_enrollment_key());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0]).ok_or_else(invalid_enrollment_key)?;
            let low = hex_value(pair[1]).ok_or_else(invalid_enrollment_key)?;
            Ok((high << 4) | low)
        })
        .collect()
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn invalid_enrollment_key() -> ProtocolError {
    ProtocolError::new(
        "invalid_machine_enrollment_key",
        "machine enrollment key is invalid",
    )
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

    #[test]
    fn remote_shell_messages_include_command_and_exit_status() {
        let open = ProxyMessage::ShellOpen {
            session_id: "ssh_1".to_string(),
            cols: 120,
            rows: 36,
            cwd: "src".to_string(),
            command: Some("cargo test -q".to_string()),
        };
        assert_eq!(
            serde_json::to_value(open).expect("serialize shell open"),
            serde_json::json!({
                "type": "shell_open",
                "session_id": "ssh_1",
                "cols": 120,
                "rows": 36,
                "cwd": "src",
                "command": "cargo test -q"
            })
        );
        let closed = TerminalServerMessage::Closed {
            reason: Some("remote process exited".to_string()),
            exit_code: Some(7),
        };
        assert_eq!(
            serde_json::to_value(closed).expect("serialize close"),
            serde_json::json!({
                "type": "closed",
                "reason": "remote process exited",
                "exit_code": 7
            })
        );
    }

    #[test]
    fn terminal_binary_frame_round_trips_raw_bytes() {
        let frame = TerminalBinaryFrame {
            kind: TerminalBinaryKind::Output,
            session_id: "term_abc".to_string(),
            revision: 42,
            payload: vec![0, 1, 2, 0xff],
        };
        let encoded = frame.encode().expect("encode terminal frame");
        assert_eq!(TerminalBinaryFrame::decode(&encoded), Ok(frame));
    }

    #[test]
    fn terminal_binary_frame_rejects_unknown_versions() {
        let frame = TerminalBinaryFrame {
            kind: TerminalBinaryKind::Input,
            session_id: "term_abc".to_string(),
            revision: 0,
            payload: b"hello".to_vec(),
        };
        let mut encoded = frame.encode().expect("encode terminal frame");
        encoded[0] = TERMINAL_BINARY_VERSION.saturating_add(1);
        let error = TerminalBinaryFrame::decode(&encoded).expect_err("version must fail");
        assert_eq!(error.code, "terminal_binary_version_mismatch");
    }

    #[test]
    fn transfer_binary_frame_round_trips_raw_bytes() {
        let frame = TransferBinaryFrame {
            kind: TransferBinaryKind::Data,
            session_id: "copy_abc".to_string(),
            payload: vec![0, 1, 2, 0xff],
        };
        let encoded = frame.encode().expect("encode transfer frame");
        assert!(TransferBinaryFrame::is_transfer_frame(&encoded));
        assert_eq!(TransferBinaryFrame::decode(&encoded), Ok(frame));
    }

    #[test]
    fn machine_enrollment_keys_embed_the_workspace() {
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let key = format_machine_enrollment_key("team one/研发", "abc123", secret)
            .expect("format enrollment key");
        assert!(!key.contains("team one"));
        assert_eq!(
            parse_machine_enrollment_key(&key).expect("parse enrollment key"),
            MachineEnrollmentKey {
                identifier: key.split_once('.').expect("key separator").0.to_string(),
                workspace_id: "team one/研发".to_string(),
                enrollment_id: "abc123".to_string(),
                secret: secret.to_string(),
            }
        );
    }

    #[test]
    fn malformed_machine_enrollment_keys_are_rejected() {
        assert!(parse_machine_enrollment_key("enr_old.secret").is_err());
        assert!(parse_machine_enrollment_key("enr_v1_zz_id.0123").is_err());
    }
}
