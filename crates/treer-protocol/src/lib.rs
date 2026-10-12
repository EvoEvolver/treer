use std::collections::BTreeMap;
pub mod cli;
pub mod recovery;
pub use recovery::*;
mod workload_tree;
pub use workload_tree::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 4;
pub const PROTOCOL_MIN_VERSION: u32 = 4;
pub const INSTALL_SKILL: &str = include_str!("../../../skills/treer-install/SKILL.md");
/// Text control frame on a private service WebSocket; binary frames carry bytes.
pub const SERVICE_STREAM_EOF: &str = "treer-service-eof-v1";
pub const CAPABILITY_AGENT_ABORT: &str = "agent.abort.v1";
pub const CAPABILITY_AGENT_STARTUP: &str = "agent.startup.v1";
pub const CAPABILITY_AGENT_STARTUP_ADMIN: &str = "agent.startup-admin.v1";
pub const CAPABILITY_AGENT_LIFECYCLE: &str = "agent.lifecycle.v1";
pub const CAPABILITY_MACHINE_EXEC: &str = "machine.exec.v1";
pub const CAPABILITY_MACHINE_UPLOAD: &str = "machine.upload.v1";
pub const CAPABILITY_RUNTIME_RESTORE: &str = "runtime.restore.v1";
pub const CAPABILITY_SERVICE_HTTP_FETCH: &str = "service.http-fetch.v1";
pub const CONTROLLER_CAPABILITIES: &[&str] = &[
    CAPABILITY_AGENT_ABORT,
    CAPABILITY_AGENT_STARTUP,
    CAPABILITY_AGENT_STARTUP_ADMIN,
    CAPABILITY_AGENT_LIFECYCLE,
    CAPABILITY_MACHINE_EXEC,
    CAPABILITY_MACHINE_UPLOAD,
    CAPABILITY_RUNTIME_RESTORE,
    CAPABILITY_SERVICE_HTTP_FETCH,
];
pub const AGENT_INTERFACE_PROTOCOL_V1: &str = "treer.agent-interface/v1";
pub const POLICY_PROVIDER_PROTOCOL_V1: &str = "treer.policy-provider/v1";
pub const POLICY_PROVIDER_BUNDLE_CAPABILITY_V1: &str = "policy.bundle.v1";
pub const DOMAIN_EVENT_SCHEMA_VERSION: u32 = 1;
pub const POLICY_SCHEMA_VERSION: u32 = 1;
pub const MESSAGE_SCHEMA_VERSION: u32 = 1;
fn default_start_missing() -> bool {
    true
}
pub const MAX_MESSAGE_BODY_BYTES: usize = 32 * 1024;
pub const MAX_MESSAGE_RECIPIENTS: usize = 32;
pub const MAX_MESSAGE_CONTEXTS: usize = 32;
pub const MAX_MESSAGE_PAGE_SIZE: u16 = 100;
pub const MAX_MESSAGE_WAIT_MILLISECONDS: u64 = 30_000;
pub const MAX_MESSAGE_IDEMPOTENCY_KEY_BYTES: usize = 256;
pub const MAX_MESSAGE_EXTERNAL_METADATA_ENTRIES: usize = 16;
pub const MACHINE_ENROLLMENT_KEY_PREFIX: &str = "enr_v1_";
pub const AGENT_ID_HEADER: &str = "x-treer-agent-id";
pub const WORKLOAD_CREDENTIAL_HEADER: &str = "x-treer-workload-credential";
pub const OPERATOR_CREDENTIAL_HEADER: &str = "x-treer-operator-credential";
pub const TERMINAL_BINARY_VERSION: u8 = 1;
const TERMINAL_BINARY_HEADER_LEN: usize = 12;
pub const NETWORK_BINARY_VERSION: u8 = 1;
const NETWORK_BINARY_MAGIC: &[u8; 3] = b"NET";
const NETWORK_BINARY_HEADER_LEN: usize = 7;
const MAX_NETWORK_STREAM_ID_BYTES: usize = 128;
const MAX_NETWORK_PAYLOAD_BYTES: usize = 64 * 1024;

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

mod inventory;
pub use inventory::*;
mod policy;
pub use policy::*;
mod identity;
pub use identity::*;
mod messages;
pub use messages::*;
mod apps;
pub use apps::*;
mod workloads;
pub use workloads::*;
mod terminal;
pub use terminal::*;
mod network;
pub use network::*;
mod events;
pub use events::*;
mod transport;
pub use transport::*;
mod enrollment;
pub use enrollment::*;
#[cfg(test)]
#[path = "lib/tests.rs"]
mod tests;
