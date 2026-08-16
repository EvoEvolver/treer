use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HOST_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProcessInfo {
    pub process_id: String,
    pub pid: Option<u32>,
    pub cwd: String,
    pub running: bool,
    pub started_at: DateTime<Utc>,
    pub last_output_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stream_epoch: String,
    pub first_revision: u64,
    pub next_revision: u64,
    pub bracketed_paste: bool,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOutputChunk {
    pub process_id: String,
    pub stream_epoch: String,
    pub revision: u64,
    pub data: String,
    pub bracketed_paste: bool,
    pub emitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOutputReplay {
    pub process_id: String,
    pub stream_epoch: String,
    pub first_available_revision: u64,
    pub next_revision: u64,
    pub gap: bool,
    pub chunks: Vec<HostOutputChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputCursor {
    pub stream_epoch: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSpawnRequest {
    pub process_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostWrite {
    pub data: String,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HostCommand {
    Sync {
        #[serde(default)]
        cursors: BTreeMap<String, OutputCursor>,
    },
    Spawn {
        request: HostSpawnRequest,
    },
    Read {
        process_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<OutputCursor>,
    },
    Write {
        process_id: String,
        writes: Vec<HostWrite>,
    },
    Resize {
        process_id: String,
        cols: u16,
        rows: u16,
    },
    Stop {
        process_id: String,
    },
    RestartController,
}

impl HostCommand {
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Spawn { .. }
                | Self::Write { .. }
                | Self::Resize { .. }
                | Self::Stop { .. }
                | Self::RestartController
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostRequest {
    pub protocol: u32,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub command: HostCommand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostResponse {
    Synced {
        host_epoch: String,
        processes: Vec<HostProcessInfo>,
        replay: Vec<HostOutputReplay>,
    },
    Process {
        process: HostProcessInfo,
    },
    Output {
        replay: HostOutputReplay,
    },
    Ack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostError {
    pub code: String,
    pub message: String,
}

impl HostError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Response {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<HostResponse>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<HostError>,
    },
    Output {
        chunk: HostOutputChunk,
    },
    Process {
        process: HostProcessInfo,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostDaemonConfig {
    pub socket_path: PathBuf,
    pub controller_path: PathBuf,
    pub controller_config_path: PathBuf,
    pub root: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutating_commands_are_explicit() {
        assert!(HostCommand::Stop {
            process_id: "p1".to_string()
        }
        .is_mutating());
        assert!(!HostCommand::Sync {
            cursors: BTreeMap::new()
        }
        .is_mutating());
    }

    #[test]
    fn wire_shape_is_stable() {
        let request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "req_1".to_string(),
            operation_id: Some("cmd_1".to_string()),
            command: HostCommand::Resize {
                process_id: "p1".to_string(),
                cols: 120,
                rows: 40,
            },
        };
        let json = serde_json::to_value(request).expect("serialize host request");
        assert_eq!(json["command"]["action"], "resize");
        assert_eq!(json["operation_id"], "cmd_1");
    }
}
