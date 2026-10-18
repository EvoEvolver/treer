//! Bounded file transfer, separate from legacy machine.upload.v1.
use serde::{Deserialize, Serialize};
pub const CAPABILITY_MACHINE_FILE_TRANSFER: &str = "machine.file-transfer.v1";
pub const MACHINE_TRANSFER_CHUNK_BYTES: usize = 64 * 1024;
pub const MACHINE_TRANSFER_DEFAULT_LIMIT: u64 = 128 * 1024 * 1024;
pub const MACHINE_TRANSFER_HARD_LIMIT: u64 = 1024 * 1024 * 1024;
pub const MACHINE_TRANSFER_IDLE_SECONDS: u64 = 30;
pub const MACHINE_TRANSFER_TOTAL_SECONDS: u64 = 15 * 60;
pub const MACHINE_TRANSFER_MAX_ACTIVE: usize = 8;

/// Proxy derives this from authentication, never from transfer request JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTransferActor {
    pub workspace_id: String,
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFileTransferCall {
    pub path: String,
    pub request: MachineFileTransferRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineFileTransferRequest {
    PutBegin {
        path: String,
        #[serde(default)]
        overwrite: bool,
        #[serde(default = "private_mode")]
        mode: u32,
        #[serde(default)]
        size: Option<u64>,
        limit: u64,
    },
    GetBegin {
        path: String,
        limit: u64,
    },
    PutChunk {
        id: String,
        offset: u64,
        content_base64: String,
    },
    GetChunk {
        id: String,
        offset: u64,
    },
    PutFinish {
        id: String,
        size: u64,
        sha256: String,
    },
    GetFinish {
        id: String,
    },
    Abort {
        id: String,
        #[serde(default)]
        read: bool,
    },
}
const fn private_mode() -> u32 {
    0o600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTransferBegin {
    pub id: String,
    pub path: String,
    pub limit: u64,
    pub chunk_bytes: usize,
    pub size: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTransferChunk {
    pub offset: u64,
    pub content_base64: String,
    pub eof: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTransferComplete {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_contract_round_trips_and_rejects_forged_actor() {
        let requests = [
            MachineFileTransferRequest::PutBegin {
                path: "reports/result".into(),
                overwrite: false,
                mode: 0o600,
                size: None,
                limit: MACHINE_TRANSFER_DEFAULT_LIMIT,
            },
            MachineFileTransferRequest::GetBegin {
                path: "reports/result".into(),
                limit: MACHINE_TRANSFER_DEFAULT_LIMIT,
            },
            MachineFileTransferRequest::PutChunk {
                id: "fixture".into(),
                offset: 64,
                content_base64: "AA==".into(),
            },
            MachineFileTransferRequest::GetChunk {
                id: "fixture".into(),
                offset: 64,
            },
            MachineFileTransferRequest::PutFinish {
                id: "fixture".into(),
                size: 1,
                sha256: "a".repeat(64),
            },
            MachineFileTransferRequest::GetFinish {
                id: "fixture".into(),
            },
            MachineFileTransferRequest::Abort {
                id: "fixture".into(),
                read: false,
            },
        ];
        for request in requests {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(
                serde_json::from_value::<MachineFileTransferRequest>(value).unwrap(),
                request
            );
        }
        assert!(
            serde_json::from_value::<MachineFileTransferRequest>(serde_json::json!({
                "operation":"get_begin", "path":"safe", "limit":1, "actor":{"id":"forged"}
            }))
            .is_err()
        );
        let begin: MachineFileTransferRequest = serde_json::from_value(serde_json::json!({
            "operation":"put_begin", "path":"safe", "limit":1
        }))
        .unwrap();
        assert!(matches!(
            begin,
            MachineFileTransferRequest::PutBegin {
                mode: 0o600,
                overwrite: false,
                size: None,
                ..
            }
        ));
    }
}
