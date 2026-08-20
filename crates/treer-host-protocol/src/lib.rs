use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_VERSION: u32 = 2;
pub const MAX_HOST_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBuildInfo {
    pub version: String,
    pub git_commit: String,
}

pub fn encode_message<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|error| error.to_string())
}

pub fn decode_message<T>(encoded: &[u8]) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let (value, consumed) = bincode::serde::decode_from_slice(encoded, bincode::config::standard())
        .map_err(|error| error.to_string())?;
    if consumed != encoded.len() {
        return Err("host frame contains trailing bytes".to_string());
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProcessInfo {
    pub process_id: String,
    pub pid: Option<u32>,
    pub cwd: String,
    pub running: bool,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub last_output_at: DateTime<Utc>,
    #[serde(default)]
    #[serde(with = "chrono::serde::ts_milliseconds_option")]
    pub exited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub stream_epoch: String,
    pub first_revision: u64,
    pub next_revision: u64,
    pub bracketed_paste: bool,
    #[serde(default)]
    pub metadata: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostOutputChunk {
    pub process_id: String,
    pub stream_epoch: String,
    pub revision: u64,
    pub data: Vec<u8>,
    pub bracketed_paste: bool,
    #[serde(with = "chrono::serde::ts_milliseconds")]
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
    pub metadata: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostWrite {
    pub data: Vec<u8>,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        #[serde(default)]
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
    #[serde(default)]
    pub operation_id: Option<String>,
    pub command: HostCommand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostResponse {
    Synced {
        host_epoch: String,
        host_build: HostBuildInfo,
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
pub enum HostMessage {
    Response {
        request_id: String,
        #[serde(default)]
        response: Option<HostResponse>,
        #[serde(default)]
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
    fn request_round_trips_through_binary_codec() {
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
        let encoded = encode_message(&request).expect("encode host request");
        assert_eq!(decode_message::<HostRequest>(&encoded), Ok(request));
    }

    #[test]
    fn optional_fields_are_encoded_in_binary_frames() {
        let request = HostRequest {
            protocol: HOST_PROTOCOL_VERSION,
            request_id: "req_sync".to_string(),
            operation_id: None,
            command: HostCommand::Sync {
                cursors: BTreeMap::new(),
            },
        };
        let encoded = encode_message(&request).expect("encode sync request");
        assert_eq!(decode_message::<HostRequest>(&encoded), Ok(request));

        let message = HostMessage::Response {
            request_id: "req_sync".to_string(),
            response: Some(HostResponse::Ack),
            error: None,
        };
        let encoded = encode_message(&message).expect("encode host response");
        assert_eq!(decode_message::<HostMessage>(&encoded), Ok(message));

        let synced = HostResponse::Synced {
            host_epoch: "host-epoch".to_string(),
            host_build: HostBuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "0123456789abcdef".to_string(),
            },
            processes: Vec::new(),
            replay: Vec::new(),
        };
        let encoded = encode_message(&synced).expect("encode sync response");
        assert_eq!(decode_message::<HostResponse>(&encoded), Ok(synced));
    }

    #[test]
    fn binary_message_round_trips_raw_pty_bytes() {
        let emitted_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis())
            .expect("valid current timestamp");
        let message = HostMessage::Output {
            chunk: HostOutputChunk {
                process_id: "p1".to_string(),
                stream_epoch: "stream_1".to_string(),
                revision: 9,
                data: vec![0, 1, 0xff],
                bracketed_paste: false,
                emitted_at,
            },
        };
        let encoded = encode_message(&message).expect("encode host message");
        assert_eq!(decode_message::<HostMessage>(&encoded), Ok(message));
    }
}
