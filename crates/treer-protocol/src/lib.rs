use std::collections::BTreeMap;
pub mod cli;
pub mod recovery;
pub use recovery::*;
pub mod prompt_delivery;
pub use prompt_delivery::*;
pub mod machine_file;
mod targets;
pub use machine_file::*;
pub use targets::*;
mod workload_tree;
pub use workload_tree::*;
mod policy_path;
pub use policy_path::*;
mod provider_cli;
pub use provider_cli::*;
mod provider_cli_observation;
pub use provider_cli_observation::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 4;
pub const PROTOCOL_MIN_VERSION: u32 = 4;
pub const INSTALL_SKILL: &str = include_str!("../../../skills/treer-install/SKILL.md");
/// Text control frame on a private service WebSocket; binary frames carry bytes.
pub const SERVICE_STREAM_EOF: &str = "treer-service-eof-v1";
pub const CAPABILITY_AGENT_PATH_TARGETS: &str = "agent.path-targets.v1";
pub const CAPABILITY_WORKLOAD_CREATE_PLACEMENT: &str = "workload.create-placement.v1";
pub const CAPABILITY_AGENT_RESERVATION_FENCE: &str = "agent.reservation-fence.v1";
/// Explicit root destination on creation; omission inherits the trusted caller.
pub const WORKLOAD_CREATE_ROOT: &str = "/";
pub const CAPABILITY_AGENT_ABORT: &str = "agent.abort.v1";
pub const CAPABILITY_AGENT_STARTUP: &str = "agent.startup.v1";
pub const CAPABILITY_AGENT_STARTUP_ADMIN: &str = "agent.startup-admin.v1";
pub const CAPABILITY_APP_UPDATE: &str = "app.update.v1";
pub const CAPABILITY_AGENT_PROMPT_DELIVERY: &str = "agent.prompt-delivery.v1";
pub const CAPABILITY_AGENT_LIFECYCLE: &str = "agent.lifecycle.v1";
pub const CAPABILITY_MACHINE_EXEC: &str = "machine.exec.v1";
pub const CAPABILITY_MACHINE_UPLOAD: &str = "machine.upload.v1";
/// Chunked, resumable browser upload (#166): `UploadBegin` carries the
/// declared size, `UploadChunk` carries its offset, `UploadPartCommit` verifies
/// a part's SHA-256 before the committed offset advances, `UploadResult`
/// answers the terminal receipt, and the Controller caps the total at
/// [`MACHINE_UPLOAD_MAX_BYTES`], bounds active uploads and reaps abandoned
/// staging files.
pub const CAPABILITY_MACHINE_UPLOAD_V3: &str = "machine.upload.v3";
/// Largest single file the chunked upload and the Controller accept.
pub const MACHINE_UPLOAD_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// Payload bytes per `UploadChunk` command (before base64).
pub const MACHINE_UPLOAD_CHUNK_BYTES: usize = 192 * 1024;
/// Largest part one HTTP request may carry (the server advertises it).
pub const MACHINE_UPLOAD_PART_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Smallest part, except the final one that reaches the declared size.
pub const MACHINE_UPLOAD_PART_MIN_BYTES: u64 = 1024 * 1024;
/// Parts per upload, so a client cannot advance the offset byte by byte.
pub const MACHINE_UPLOAD_MAX_PARTS: usize = 160;
/// Concurrent uploads one Controller stages at a time.
pub const MACHINE_UPLOAD_MAX_ACTIVE: usize = 8;
/// An upload with no part for this long is reaped (staging file deleted).
pub const MACHINE_UPLOAD_IDLE_SECONDS: u64 = 60 * 60;
/// Hard lifetime of one upload, however slowly it trickles.
pub const MACHINE_UPLOAD_MAX_LIFETIME_SECONDS: u64 = 6 * 60 * 60;
/// How long the Controller remembers a committed upload's receipt, so a Proxy
/// that lost the commit response can still learn the outcome after a long
/// Controller outage.
pub const MACHINE_UPLOAD_RESULT_RETENTION_SECONDS: u64 = 24 * 60 * 60;
/// How long the Proxy serves a terminal upload row (idempotent commit, status).
pub const MACHINE_UPLOAD_RECEIPT_RETENTION_SECONDS: u64 = 10 * 60;
pub const CAPABILITY_RUNTIME_RESTORE: &str = "runtime.restore.v1";
pub const CAPABILITY_SERVICE_HTTP_FETCH: &str = "service.http-fetch.v1";
pub const CAPABILITY_LAUNCHER_INSTALL: &str = "launcher.install.v1";
pub const CAPABILITY_AGENT_INTERFACE_PROVENANCE: &str = "agent.interface-provenance.v1";
/// Launcher-contract feature: Controller-owned per-Agent private state directory,
/// quota reservations and deletion cleanup. Not an AIS manifest capability.
pub const CAPABILITY_AGENT_PRIVATE_STATE: &str = "agent.private-state.v1";
pub const CONTROLLER_CAPABILITIES: &[&str] = &[
    CAPABILITY_LAUNCHER_INSTALL,
    CAPABILITY_AGENT_INTERFACE_PROVENANCE,
    CAPABILITY_WORKLOAD_CREATE_PLACEMENT,
    CAPABILITY_AGENT_RESERVATION_FENCE,
    CAPABILITY_AGENT_PATH_TARGETS,
    CAPABILITY_AGENT_ABORT,
    CAPABILITY_AGENT_STARTUP,
    CAPABILITY_AGENT_STARTUP_ADMIN,
    CAPABILITY_APP_UPDATE,
    CAPABILITY_AGENT_PROMPT_DELIVERY,
    CAPABILITY_AGENT_LIFECYCLE,
    CAPABILITY_MACHINE_EXEC,
    CAPABILITY_MACHINE_UPLOAD,
    CAPABILITY_MACHINE_UPLOAD_V3,
    CAPABILITY_MACHINE_FILE_TRANSFER,
    CAPABILITY_RUNTIME_RESTORE,
    CAPABILITY_SERVICE_HTTP_FETCH,
    CAPABILITY_POLICY_PATH_RULES,
];
pub const AGENT_INTERFACE_PROTOCOL_V1: &str = "treer.agent-interface/v1";
/// Semantic AIS features implemented by this Controller. Unknown advertised AIS
/// extensions are still preserved during registration, but not asserted here.
pub const CONTROLLER_INTERFACE_CAPABILITIES: &[&str] = &[
    "abort",
    "prompt.notify",
    "prompt.queue",
    "prompt.queue.read",
    "prompt.submit",
    "state.observe",
    "transcript.read",
];
/// Retained launchers below this floor cannot be installed or rolled back to.
pub const MINIMUM_LAUNCHER_SECURITY_EPOCH: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherControllerContract {
    pub git_commit: String,
    pub protocol: String,
    pub capabilities: Vec<String>,
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_launcher_security_epoch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<LauncherMachineContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherMachineContext {
    pub root: String,
    pub server_id: String,
    pub workspace_id: String,
}
/// Browser-defense provenance, not a secret or an OS process authentication token.
pub const AGENT_INTERFACE_SOURCE_HEADER: &str = "x-treer-interface-source";
pub const AGENT_INTERFACE_INSTANCE_HEADER: &str = "x-treer-interface-instance";
pub const AGENT_INTERFACE_SOURCE_UI: &str = "proxy-ui";
pub const AGENT_INTERFACE_SOURCE_CONTROLLER: &str = "controller-api";
pub const POLICY_PROVIDER_PROTOCOL_V1: &str = "treer.policy-provider/v1";
pub const POLICY_PROVIDER_BUNDLE_CAPABILITY_V1: &str = "policy.bundle.v1";
pub const DOMAIN_EVENT_SCHEMA_VERSION: u32 = 1;
pub const POLICY_SCHEMA_VERSION: u32 = 1;
pub const POLICY_SCHEMA_VERSION_V2: u32 = 2;
pub const POLICY_PROVIDER_BUNDLE_CAPABILITY_V2: &str = "policy.bundle.v2";
pub const CAPABILITY_POLICY_PATH_RULES: &str = "policy.path-rules.v2";
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
/// Treer-controlled header names. Underscores are folded to hyphens because CGI/WSGI
/// style servers map `X_Treer_Foo` and `X-Treer-Foo` to the same variable, so a
/// forged underscore spelling must be stripped like the hyphenated one.
pub fn is_treer_header_name(name: &str) -> bool {
    name.len() >= 8
        && name.as_bytes()[..8]
            .iter()
            .map(|b| {
                if *b == b'_' {
                    b'-'
                } else {
                    b.to_ascii_lowercase()
                }
            })
            .eq(*b"x-treer-")
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Vec<String>>,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<AgentCreationErrorDetails>>,
}

/// Only bounded display identifiers, never argv, PATH, cwd or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCreationErrorDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub creation_state: AgentCreationFailureState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCreationFailureState {
    /// Controller proves resolution failed before startup persistence/Host spawn.
    NotStarted,
    IdentityRemoved,
    ReservedOutcomeUnknown,
    CleanupFailed,
}

/// The UI may match this identifier against a vetted provider table. Absolute
/// paths and arbitrary diagnostic strings must never become install recipes.
pub fn agent_install_command_name(command: &str) -> Option<String> {
    (!command.is_empty()
        && command.len() <= 128
        && command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(command, "." | "..")
        && !command.ends_with('.'))
    .then(|| command.to_owned())
}

impl ProtocolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            hint: None,
            code: code.into(),
            message: message.into(),
            details: None,
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

mod email;
pub use email::*;
