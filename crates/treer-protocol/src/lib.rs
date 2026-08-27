use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 4;
pub const AGENT_INTERFACE_PROTOCOL_V1: &str = "treer.agent-interface/v1";
pub const DOMAIN_EVENT_SCHEMA_VERSION: u32 = 1;
pub const POLICY_SCHEMA_VERSION: u32 = 1;
pub const MESSAGE_SCHEMA_VERSION: u32 = 1;
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
pub struct BuildInfo {
    pub version: String,
    pub git_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub server_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub name: String,
    pub hostname: String,
    pub root: String,
    pub controller_build: BuildInfo,
    pub host_build: BuildInfo,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_agents: Option<Vec<String>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<AgentInterfaceDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterfaceDescriptor {
    pub protocol: String,
    pub instance_id: String,
    pub port: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_path: Option<String>,
    pub registered_at: DateTime<Utc>,
}

impl AgentInterfaceDescriptor {
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAgentInterfaceRequest {
    pub protocol: String,
    pub instance_id: String,
    pub port: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterfaceManifest {
    pub protocol: String,
    pub instance_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTranscriptResponse {
    pub agent_id: String,
    pub interface_instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub entries: Vec<AgentTranscriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTranscriptEntry {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterfaceStatusResponse {
    pub agent_id: String,
    pub interface_instance_id: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub revision: u64,
    pub workspace: WorkspaceInfo,
    pub servers: Vec<ServerInfo>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    Monitor,
    Enforce,
}

impl PolicyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monitor => "monitor",
            Self::Enforce => "enforce",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyPrincipalKind {
    Human,
    Agent,
    Machine,
    Service,
}

impl PolicyPrincipalKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Machine => "machine",
            Self::Service => "service",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPrincipalRef {
    pub kind: PolicyPrincipalKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPrincipalGroup {
    #[serde(default)]
    pub principals: Vec<PolicyPrincipalRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySubjectSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PolicyPrincipalKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, rename = "self", skip_serializing_if = "std::ops::Not::not")]
    pub is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyResourceSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicyRule {
    pub id: String,
    pub priority: i32,
    pub effect: PolicyEffect,
    pub subjects: Vec<PolicySubjectSelector>,
    pub actions: Vec<String>,
    pub resources: Vec<PolicyResourceSelector>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicyDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub defaults: BTreeMap<String, PolicyEffect>,
    #[serde(default)]
    pub groups: BTreeMap<String, PolicyPrincipalGroup>,
    #[serde(default)]
    pub rules: Vec<WorkspacePolicyRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub workspace_id: String,
    pub revision: u64,
    pub mode: PolicyMode,
    pub document: WorkspacePolicyDocument,
    pub updated_at: DateTime<Utc>,
    pub updated_by: PolicyPrincipalRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHuman {
    pub user_id: String,
    pub preferred_name: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdentityTokenRequest {
    pub audience: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdentityTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub expires_at: DateTime<Utc>,
    pub audience: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdentityClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub workspace_id: String,
    pub machine_id: String,
    pub service_id: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdentityVerifyRequest {
    pub token: String,
    pub audience: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdentityVerifyResponse {
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<WorkloadIdentityClaims>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPrincipalKind {
    Agent,
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPrincipal {
    pub kind: AppPrincipalKind,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Stable identity attached to a Core Message.
///
/// `name` and `role` are immutable display snapshots. Authorization always uses
/// `kind` and `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePrincipalKind {
    Agent,
    Human,
    Machine,
    Service,
}

impl MessagePrincipalKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Human => "human",
            Self::Machine => "machine",
            Self::Service => "service",
        }
    }
}

impl From<AppPrincipalKind> for MessagePrincipalKind {
    fn from(value: AppPrincipalKind) -> Self {
        match value {
            AppPrincipalKind::Agent => Self::Agent,
            AppPrincipalKind::Human => Self::Human,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagePrincipal {
    pub kind: MessagePrincipalKind,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl From<AppPrincipal> for MessagePrincipal {
    fn from(value: AppPrincipal) -> Self {
        Self {
            kind: value.kind.into(),
            id: value.id,
            name: value.name,
            role: value.role,
        }
    }
}

/// Channel-neutral, sender-asserted source data. Core never treats these values
/// as an authenticated Treer identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageExternalSource {
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub conversation_id: String,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreMessage {
    pub schema_version: u32,
    pub message_id: String,
    pub workspace_id: String,
    pub sender: MessagePrincipal,
    pub recipients: Vec<MessagePrincipal>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_source: Option<MessageExternalSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageDelivery {
    pub delivery_id: String,
    pub message: CoreMessage,
    pub recipient: MessagePrincipal,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    pub recipients: Vec<String>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_source: Option<MessageExternalSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageResponse {
    pub message: CoreMessage,
    pub delivery_ids: Vec<String>,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetMessageResponse {
    pub message: CoreMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagePage {
    pub messages: Vec<CoreMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub remaining_unacknowledged: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListMessagesQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default = "default_message_page_size")]
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiveMessagesRequest {
    #[serde(default = "default_message_page_size")]
    pub limit: u16,
    #[serde(default)]
    pub wait_milliseconds: u64,
}

const fn default_message_page_size() -> u16 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiveMessagesResponse {
    pub deliveries: Vec<MessageDelivery>,
    pub remaining_unacknowledged: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeMessagesRequest {
    pub delivery_ids: Vec<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeMessagesResponse {
    pub acknowledged_delivery_ids: Vec<String>,
    pub already_acknowledged_delivery_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyMailRecipient {
    pub principal: MessagePrincipal,
    pub position: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyMailMessage {
    pub message_id: String,
    pub workspace_id: String,
    pub sender: MessagePrincipal,
    pub recipients: Vec<LegacyMailRecipient>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportMessagesRequest {
    pub format: String,
    pub operation_id: String,
    pub messages: Vec<LegacyMailMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportMessagesResponse {
    pub imported: u64,
    pub existing: u64,
    pub message_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIdentityClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub workspace_id: String,
    pub service_id: String,
    pub principal_kind: AppPrincipalKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIdentityTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub expires_at: DateTime<Utc>,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIdentityVerifyRequest {
    pub token: String,
    pub audience: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIdentityVerifyResponse {
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<AppIdentityClaims>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveAppRecipientsRequest {
    #[serde(default)]
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveAppRecipientsResponse {
    pub sender: AppPrincipal,
    pub recipients: Vec<AppPrincipal>,
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
    /// Linux network-sandbox ports to publish on the machine loopback.
    /// Each value is both the namespace listen port and the host publish port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish_ports: Vec<u16>,
    /// Git URL of a community recipe. When set, Treer prompts the new Agent
    /// with the bundled install skill instead of waiting for a later prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppDesiredState {
    #[default]
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppDeploymentStatus {
    #[default]
    Pending,
    Running,
    Stopped,
    Exited,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDeployment {
    pub app_id: String,
    pub workspace_id: String,
    pub name: String,
    pub server_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    pub port: u16,
    pub hostname: String,
    pub service_id: String,
    pub desired_state: AppDesiredState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_agent_id: Option<String>,
    #[serde(default)]
    pub restart_count: u64,
    #[serde(default)]
    pub status: AppDeploymentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAppDeploymentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    pub port: u16,
    pub hostname: String,
}

pub const INSTALL_SKILL: &str = include_str!("../../../skills/treer-install/SKILL.md");

pub fn recipe_url(request: &CreateAgentRequest) -> Option<&str> {
    request
        .recipe
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn validate_recipe_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.len() > 2048 {
        return Err("recipe URL is too long".to_string());
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| "recipe must be an http(s) URL".to_string())?;
    if rest.is_empty() || rest.contains(' ') || rest.contains('@') {
        return Err("recipe URL must be an http(s) URL without credentials".to_string());
    }
    Ok(())
}

pub fn installer_base_prompt(recipe_url: &str) -> String {
    format!(
        "{INSTALL_SKILL}\n\n## This install\n\nRecipe URL: {recipe_url}\n\nStart now. Do not ask for confirmation.\n"
    )
}

pub fn installer_composer_ready(text: &str) -> bool {
    if text.contains("Do you trust") || text.contains("Press enter to continue") {
        return false;
    }
    text.contains("Ask Codex")
        || text.contains("YOLO mode")
        || text.contains("Ask Claude")
        || text.contains("Grok Build")
        || text.contains("cursor-agent")
        || text.contains("OpenCode")
}

pub fn recipe_installer_kind_allowed(kind: &str) -> bool {
    matches!(
        kind,
        "auto"
            | "codex"
            | "claude"
            | "cursor"
            | "cursor-agent"
            | "grok"
            | "opencode"
            | "pi"
            | "shell"
    )
}

pub fn reusable_installer_kind(kind: &str) -> bool {
    matches!(
        kind,
        "codex" | "claude" | "cursor" | "cursor-agent" | "grok" | "opencode" | "pi"
    )
}

pub fn pick_existing_installer_agent<'a>(
    agents: &'a [AgentInfo],
    server_id: &str,
    kind: Option<&str>,
) -> Option<&'a AgentInfo> {
    let wanted = kind
        .map(normalize_interactive_agent_kind)
        .filter(|value| *value != "auto");
    let mut best: Option<&AgentInfo> = None;
    let mut best_rank = u8::MAX;
    for agent in agents {
        if agent.server_id != server_id || !reusable_installer_kind(&agent.kind) {
            continue;
        }
        if wanted.is_some_and(|kind| normalize_interactive_agent_kind(&agent.kind) != kind) {
            continue;
        }
        if agent.status.is_terminal() || agent.status == AgentStatus::Working {
            continue;
        }
        let rank = match agent.status {
            AgentStatus::Idle => 0,
            AgentStatus::Blocked => 1,
            AgentStatus::Starting => 2,
            AgentStatus::Unknown => 3,
            _ => 4,
        };
        if rank < best_rank {
            best = Some(agent);
            best_rank = rank;
        }
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveAgentCatalogEntry {
    pub kind: &'static str,
    pub command: &'static str,
    pub install: Option<&'static str>,
}

pub const INTERACTIVE_AGENT_CATALOG: &[InteractiveAgentCatalogEntry] = &[
    InteractiveAgentCatalogEntry {
        kind: "claude",
        command: "claude",
        install: Some("curl -fsSL https://claude.ai/install.sh | bash"),
    },
    InteractiveAgentCatalogEntry {
        kind: "cursor",
        command: "cursor-agent",
        install: Some("curl https://cursor.com/install -fsS | bash"),
    },
    InteractiveAgentCatalogEntry {
        kind: "grok",
        command: "grok",
        install: None,
    },
    InteractiveAgentCatalogEntry {
        kind: "opencode",
        command: "opencode",
        install: Some("npm install -g opencode-ai"),
    },
    InteractiveAgentCatalogEntry {
        kind: "pi",
        command: "pi",
        install: None,
    },
    InteractiveAgentCatalogEntry {
        kind: "codex",
        command: "codex",
        install: Some("npm install -g @openai/codex"),
    },
];

pub fn normalize_interactive_agent_kind(kind: &str) -> &str {
    if kind == "cursor-agent" {
        "cursor"
    } else {
        kind
    }
}

pub fn interactive_agent_kind_for_command(command: &str) -> Option<&'static str> {
    let file = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let name = file.strip_suffix(".exe").unwrap_or(file);
    if name == "cursor-agent" {
        return Some("cursor");
    }
    INTERACTIVE_AGENT_CATALOG
        .iter()
        .find(|entry| entry.kind == name || entry.command == name)
        .map(|entry| entry.kind)
}

pub fn interactive_agent_catalog_entry(
    kind: &str,
) -> Option<&'static InteractiveAgentCatalogEntry> {
    let kind = normalize_interactive_agent_kind(kind);
    INTERACTIVE_AGENT_CATALOG
        .iter()
        .find(|entry| entry.kind == kind)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunchProfile {
    pub profile_id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentLaunchProfileRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub cwd: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAgentLaunchProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgentProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
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
        workload_credential: String,
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
    Transcript {
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Stop {
        agent_id: String,
    },
    ProbeNetwork {
        host: String,
        port: u16,
        timeout_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_agent_id: Option<String>,
    },
    ShutdownMachine,
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
pub enum NetworkBinaryKind {
    Open = 1,
    Opened = 2,
    Data = 3,
    WindowUpdate = 4,
    HalfClose = 5,
    Reset = 6,
    Direct = 7,
}

impl TryFrom<u8> for NetworkBinaryKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Open),
            2 => Ok(Self::Opened),
            3 => Ok(Self::Data),
            4 => Ok(Self::WindowUpdate),
            5 => Ok(Self::HalfClose),
            6 => Ok(Self::Reset),
            7 => Ok(Self::Direct),
            _ => Err(ProtocolError::new(
                "invalid_network_frame",
                format!("unknown network binary frame kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkBinaryFrame {
    pub kind: NetworkBinaryKind,
    pub stream_id: String,
    pub payload: Vec<u8>,
}

impl NetworkBinaryFrame {
    pub fn is_network_frame(encoded: &[u8]) -> bool {
        encoded.starts_with(NETWORK_BINARY_MAGIC)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let stream = self.stream_id.as_bytes();
        let stream_len = u16::try_from(stream.len()).map_err(|_| {
            ProtocolError::new("invalid_network_frame", "network stream id is too long")
        })?;
        if stream.is_empty() {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network stream id is empty",
            ));
        }
        if stream.len() > MAX_NETWORK_STREAM_ID_BYTES
            || self.payload.len() > MAX_NETWORK_PAYLOAD_BYTES
        {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network frame exceeds its size limit",
            ));
        }
        let mut encoded =
            Vec::with_capacity(NETWORK_BINARY_HEADER_LEN + stream.len() + self.payload.len());
        encoded.extend_from_slice(NETWORK_BINARY_MAGIC);
        encoded.push(NETWORK_BINARY_VERSION);
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&stream_len.to_be_bytes());
        encoded.extend_from_slice(stream);
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        if encoded.len() < NETWORK_BINARY_HEADER_LEN {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network binary frame is shorter than its header",
            ));
        }
        if !Self::is_network_frame(encoded) {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network binary frame has an invalid magic value",
            ));
        }
        if encoded[3] != NETWORK_BINARY_VERSION {
            return Err(ProtocolError::new(
                "network_binary_version_mismatch",
                format!(
                    "network binary frame uses version {}, expected {}",
                    encoded[3], NETWORK_BINARY_VERSION
                ),
            ));
        }
        let kind = NetworkBinaryKind::try_from(encoded[4])?;
        let stream_len = usize::from(u16::from_be_bytes([encoded[5], encoded[6]]));
        let payload_offset = NETWORK_BINARY_HEADER_LEN
            .checked_add(stream_len)
            .filter(|offset| *offset <= encoded.len())
            .ok_or_else(|| {
                ProtocolError::new(
                    "invalid_network_frame",
                    "network stream id exceeds the binary frame",
                )
            })?;
        if stream_len == 0 {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network stream id is empty",
            ));
        }
        if stream_len > MAX_NETWORK_STREAM_ID_BYTES
            || encoded.len().saturating_sub(payload_offset) > MAX_NETWORK_PAYLOAD_BYTES
        {
            return Err(ProtocolError::new(
                "invalid_network_frame",
                "network frame exceeds its size limit",
            ));
        }
        let stream_id = std::str::from_utf8(&encoded[7..payload_offset])
            .map_err(|_| {
                ProtocolError::new("invalid_network_frame", "network stream id is not UTF-8")
            })?
            .to_string();
        Ok(Self {
            kind,
            stream_id,
            payload: encoded[payload_offset..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkOpenRequest {
    pub destination: String,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConnectRequest {
    pub source_server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_agent_id: Option<String>,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDirectTarget {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineServiceProtocol {
    Tcp,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineService {
    pub service_id: String,
    pub workspace_id: String,
    pub name: String,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    pub target_host: String,
    pub target_port: u16,
    pub protocol: MachineServiceProtocol,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMachineServiceRequest {
    pub name: String,
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    #[serde(default = "default_network_target_host")]
    pub target_host: String,
    pub target_port: u16,
    #[serde(default = "default_machine_service_protocol")]
    pub protocol: MachineServiceProtocol,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMachineServiceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<MachineServiceProtocol>,
}

const fn default_machine_service_protocol() -> MachineServiceProtocol {
    MachineServiceProtocol::Tcp
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNetworkHost {
    pub workspace_id: String,
    pub hostname: String,
    pub service_id: String,
    pub service_protocol: MachineServiceProtocol,
    pub destination_server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_agent_id: Option<String>,
    pub target_host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNetworkHostsSnapshot {
    pub workspace_id: String,
    pub revision: u64,
    pub hosts: Vec<VirtualNetworkHost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVirtualNetworkHostRequest {
    pub hostname: String,
    pub service_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceIngressAccess {
    #[default]
    Public,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceIngress {
    pub ingress_id: String,
    pub workspace_id: String,
    pub service_id: String,
    pub hostname: String,
    pub access: ServiceIngressAccess,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateServiceIngressRequest {
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default)]
    pub access: ServiceIngressAccess,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateServiceIngressRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<ServiceIngressAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineTrafficRecord {
    pub window_start: DateTime<Utc>,
    pub source_server_id: String,
    pub destination_server_id: String,
    pub payload_bytes: u64,
    pub payload_frames: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizationAuditEvent {
    pub sequence: i64,
    pub event_id: String,
    pub organization_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub actor_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_name: Option<String>,
    pub source: String,
    pub action: String,
    pub outcome: String,
    pub resource_kind: String,
    pub resource_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub payload: Value,
}

fn default_network_target_host() -> String {
    "127.0.0.1".to_string()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineEnrollmentKey {
    pub identifier: String,
    pub workspace_id: String,
    pub enrollment_id: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineEnrollmentRequest {
    pub installation_id: String,
    pub name: String,
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
        .as_chunks::<2>()
        .0
        .iter()
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
    fn virtual_host_snapshot_wire_shape_carries_revision() {
        let message = ProxyMessage::VirtualNetworkHosts {
            snapshot: VirtualNetworkHostsSnapshot {
                workspace_id: "default".to_string(),
                revision: 42,
                hosts: Vec::new(),
            },
        };

        assert_eq!(
            serde_json::to_value(message).expect("serialize virtual hosts"),
            serde_json::json!({
                "type": "virtual_network_hosts",
                "snapshot": {
                    "workspace_id": "default",
                    "revision": 42,
                    "hosts": []
                }
            })
        );
    }

    #[test]
    fn machine_shutdown_wire_shape_is_stable() {
        let message = ProxyMessage::Command {
            envelope: CommandEnvelope {
                command_id: "cmd_shutdown".to_string(),
                workspace_id: "default".to_string(),
                command: AgentCommand::ShutdownMachine,
            },
        };

        let json = serde_json::to_value(message).expect("serialize command");
        assert_eq!(json["envelope"]["command"]["action"], "shutdown_machine");
    }

    #[test]
    fn network_probe_wire_shape_is_stable() {
        let message = ProxyMessage::Command {
            envelope: CommandEnvelope {
                command_id: "cmd_probe".to_string(),
                workspace_id: "default".to_string(),
                command: AgentCommand::ProbeNetwork {
                    host: "127.0.0.1".to_string(),
                    port: 8080,
                    timeout_ms: 3_000,
                    target_agent_id: None,
                },
            },
        };
        let json = serde_json::to_value(message).expect("serialize probe command");
        assert_eq!(json["envelope"]["command"]["action"], "probe_network");
        assert_eq!(json["envelope"]["command"]["port"], 8080);
    }

    #[test]
    fn machine_service_requests_use_service_ids_for_aliases() {
        let request = CreateVirtualNetworkHostRequest {
            hostname: "api.internal".to_string(),
            service_id: "svc_api".to_string(),
        };
        assert_eq!(
            serde_json::to_value(request).expect("serialize virtual host request"),
            serde_json::json!({
                "hostname": "api.internal",
                "service_id": "svc_api"
            })
        );
    }

    #[test]
    fn workload_identity_requests_have_stable_wire_shapes() {
        let token = WorkloadIdentityTokenRequest {
            audience: "api".to_string(),
        };
        assert_eq!(
            serde_json::to_value(token).expect("serialize token request"),
            serde_json::json!({ "audience": "api" })
        );
        let inactive = WorkloadIdentityVerifyResponse {
            active: false,
            claims: None,
        };
        assert_eq!(
            serde_json::to_value(inactive).expect("serialize verification"),
            serde_json::json!({ "active": false })
        );
    }

    #[test]
    fn domain_event_envelope_has_a_stable_wire_shape() {
        let event = DomainEventEnvelope {
            event_id: "evt_123".to_string(),
            schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
            organization_id: Some("org_1".to_string()),
            workspace_id: "ws_1".to_string(),
            actor: DomainEventActor {
                kind: "agent".to_string(),
                id: Some("agent_1".to_string()),
            },
            action: "service.updated".to_string(),
            resource: DomainEventResource {
                kind: "service".to_string(),
                id: "svc_1".to_string(),
            },
            occurred_at: "2026-08-18T21:00:00Z".parse().expect("timestamp"),
            trace_id: Some("trace_1".to_string()),
            causation_id: Some("evt_122".to_string()),
            correlation_id: Some("task_1".to_string()),
            workspace_revision: Some(9),
            payload: serde_json::json!({"hostname": "api.internal"}),
        };

        assert_eq!(
            serde_json::to_value(event).expect("serialize domain event"),
            serde_json::json!({
                "event_id": "evt_123",
                "schema_version": 1,
                "organization_id": "org_1",
                "workspace_id": "ws_1",
                "actor": {"kind": "agent", "id": "agent_1"},
                "action": "service.updated",
                "resource": {"kind": "service", "id": "svc_1"},
                "occurred_at": "2026-08-18T21:00:00Z",
                "trace_id": "trace_1",
                "causation_id": "evt_122",
                "correlation_id": "task_1",
                "workspace_revision": 9,
                "payload": {"hostname": "api.internal"}
            })
        );
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
    fn terminal_attach_cursor_is_optional_on_the_wire() {
        let parsed: ProxyMessage = serde_json::from_value(serde_json::json!({
            "type": "terminal_attach",
            "session_id": "term_1",
            "agent_id": "agent_1",
            "cols": 80,
            "rows": 24
        }))
        .expect("legacy attach without cursor");
        match parsed {
            ProxyMessage::TerminalAttach { cursor, .. } => assert_eq!(cursor, None),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn terminal_ready_carries_stream_replay_metadata() {
        let message = TerminalServerMessage::Ready {
            session_id: "term_1".to_string(),
            stream_epoch: Some("stream_a".to_string()),
            revision: Some(9),
            gap: true,
        };
        assert_eq!(
            serde_json::to_value(&message).expect("serialize"),
            serde_json::json!({
                "type": "ready",
                "session_id": "term_1",
                "stream_epoch": "stream_a",
                "revision": 9,
                "gap": true
            })
        );
        let parsed: TerminalServerMessage = serde_json::from_value(serde_json::json!({
            "type": "ready",
            "session_id": "term_1"
        }))
        .expect("legacy ready");
        assert_eq!(
            parsed,
            TerminalServerMessage::Ready {
                session_id: "term_1".to_string(),
                stream_epoch: None,
                revision: None,
                gap: false,
            }
        );
    }

    #[test]
    fn terminal_cursor_is_a_text_control_frame() {
        let message = TerminalServerMessage::Cursor {
            stream_epoch: "stream_a".to_string(),
            revision: 11,
        };
        assert_eq!(
            serde_json::to_value(&message).expect("serialize"),
            serde_json::json!({
                "type": "cursor",
                "stream_epoch": "stream_a",
                "revision": 11
            })
        );
    }

    #[test]
    fn controller_terminal_ready_is_additive_json() {
        let parsed: AgentServerMessage = serde_json::from_value(serde_json::json!({
            "type": "terminal_ready",
            "session_id": "term_1",
            "stream_epoch": "stream_a",
            "revision": 4
        }))
        .expect("terminal ready without gap");
        assert_eq!(
            parsed,
            AgentServerMessage::TerminalReady {
                session_id: "term_1".to_string(),
                stream_epoch: "stream_a".to_string(),
                revision: 4,
                gap: false,
            }
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
    fn network_binary_frame_round_trips_raw_bytes() {
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Data,
            stream_id: "net_123".to_string(),
            payload: vec![0, 255, 13, 10, 42],
        };
        let encoded = frame.encode().expect("encode network frame");
        assert!(NetworkBinaryFrame::is_network_frame(&encoded));
        assert_eq!(
            NetworkBinaryFrame::decode(&encoded).expect("decode network frame"),
            frame
        );
    }

    #[test]
    fn network_direct_target_round_trips() {
        let target = NetworkDirectTarget {
            host: "example.com".to_string(),
            port: 443,
        };
        let frame = NetworkBinaryFrame {
            kind: NetworkBinaryKind::Direct,
            stream_id: "net_direct".to_string(),
            payload: serde_json::to_vec(&target).expect("encode direct target"),
        };

        let decoded = NetworkBinaryFrame::decode(&frame.encode().expect("encode direct frame"))
            .expect("decode direct frame");
        assert_eq!(decoded.kind, NetworkBinaryKind::Direct);
        assert_eq!(
            serde_json::from_slice::<NetworkDirectTarget>(&decoded.payload)
                .expect("decode direct target"),
            target
        );
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

    #[test]
    fn workspace_policy_document_has_a_stable_json_shape() {
        let document = WorkspacePolicyDocument {
            schema_version: POLICY_SCHEMA_VERSION,
            defaults: BTreeMap::from([("agent.prompt".to_string(), PolicyEffect::Deny)]),
            groups: BTreeMap::new(),
            rules: vec![WorkspacePolicyRule {
                id: "self-read".to_string(),
                priority: 100,
                effect: PolicyEffect::Allow,
                subjects: vec![PolicySubjectSelector {
                    kind: Some(PolicyPrincipalKind::Agent),
                    id: None,
                    machine_id: None,
                    group: None,
                    is_self: true,
                }],
                actions: vec!["agent.metadata.read".to_string()],
                resources: vec![PolicyResourceSelector {
                    kind: Some("agent".to_string()),
                    id: None,
                    principal_group: None,
                }],
            }],
        };
        let value = serde_json::to_value(&document).expect("serialize policy document");
        assert_eq!(
            value,
            serde_json::json!({
                "schema_version": 1,
                "defaults": {"agent.prompt": "deny"},
                "groups": {},
                "rules": [{
                    "id": "self-read",
                    "priority": 100,
                    "effect": "allow",
                    "subjects": [{"kind": "agent", "self": true}],
                    "actions": ["agent.metadata.read"],
                    "resources": [{"kind": "agent"}]
                }]
            })
        );
        assert_eq!(
            serde_json::from_value::<WorkspacePolicyDocument>(value)
                .expect("deserialize policy document"),
            document
        );
        assert!(
            serde_json::from_value::<WorkspacePolicyDocument>(serde_json::json!({
                "schema_version": 1,
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn core_message_wire_shape_round_trips_ordered_contexts() {
        let message = CoreMessage {
            schema_version: MESSAGE_SCHEMA_VERSION,
            message_id: "msg_1".to_string(),
            workspace_id: "ws_1".to_string(),
            sender: MessagePrincipal {
                kind: MessagePrincipalKind::Agent,
                id: "agent_1".to_string(),
                name: "builder".to_string(),
                role: None,
            },
            recipients: vec![MessagePrincipal {
                kind: MessagePrincipalKind::Human,
                id: "user_1".to_string(),
                name: "Owner".to_string(),
                role: Some("owner".to_string()),
            }],
            context_ids: vec!["msg_parent_a".to_string(), "msg_parent_b".to_string()],
            body: "ready\nwith details".to_string(),
            created_at: "2026-08-21T12:00:00Z".parse().expect("timestamp"),
            expires_at: None,
            correlation_id: Some("task_1".to_string()),
            trace_id: None,
            external_source: Some(MessageExternalSource {
                channel: "telegram".to_string(),
                account_id: Some("bot_7".to_string()),
                conversation_id: "-10042:9".to_string(),
                message_id: "100".to_string(),
                metadata: BTreeMap::from([("user_id".to_string(), "1234".to_string())]),
            }),
        };

        let encoded = serde_json::to_value(&message).expect("serialize message");
        assert_eq!(encoded["schema_version"], MESSAGE_SCHEMA_VERSION);
        assert_eq!(
            encoded["context_ids"],
            serde_json::json!(["msg_parent_a", "msg_parent_b"])
        );
        assert_eq!(
            serde_json::from_value::<CoreMessage>(encoded).expect("deserialize message"),
            message
        );
    }

    #[test]
    fn message_requests_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<SendMessageRequest>(serde_json::json!({
                "recipients": ["agent_1"],
                "body": "hello",
                "unexpected": true
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::from_value::<ReceiveMessagesRequest>(serde_json::json!({}))
                .expect("default receive request"),
            ReceiveMessagesRequest {
                limit: 50,
                wait_milliseconds: 0,
            }
        );
    }

    #[test]
    fn installer_prompt_embeds_the_bundled_skill_and_recipe_url() {
        assert!(INSTALL_SKILL.starts_with("---\nname: treer-install\n"));
        assert!(!INSTALL_SKILL.contains("TODO"));
        let prompt = installer_base_prompt("https://github.com/example/recipe.git");
        assert!(prompt.contains("You are the **installer**"));
        assert!(prompt.contains("Recipe URL: https://github.com/example/recipe.git"));
        assert!(validate_recipe_url("https://github.com/example/recipe.git").is_ok());
        assert!(validate_recipe_url("https://user:pass@example.com/repo.git").is_err());
        assert!(!installer_composer_ready("Do you trust the contents"));
        assert!(installer_composer_ready("Ask Codex to do anything"));
        assert!(installer_composer_ready("Ask Claude to do anything"));
        assert!(recipe_installer_kind_allowed("auto"));
        assert!(recipe_installer_kind_allowed("cursor"));
        assert!(!recipe_installer_kind_allowed("command"));
        assert!(reusable_installer_kind("claude"));
        assert!(!reusable_installer_kind("auto"));
        assert!(!reusable_installer_kind("shell"));
    }

    fn installer_agent(
        agent_id: &str,
        server_id: &str,
        kind: &str,
        status: AgentStatus,
    ) -> AgentInfo {
        let now = Utc::now();
        AgentInfo {
            agent_id: agent_id.to_string(),
            workspace_id: "ws_test".to_string(),
            server_id: server_id.to_string(),
            kind: kind.to_string(),
            name: agent_id.to_string(),
            cwd: ".".to_string(),
            status,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        }
    }

    #[test]
    fn recipe_install_reuses_an_idle_interactive_agent_on_the_machine() {
        let idle_claude = installer_agent("ag_claude", "srv_a", "claude", AgentStatus::Idle);
        let working_codex = installer_agent("ag_codex", "srv_a", "codex", AgentStatus::Working);
        let other_machine = installer_agent("ag_other", "srv_b", "cursor", AgentStatus::Idle);
        let terminal = installer_agent("ag_term", "srv_a", "command", AgentStatus::Idle);
        let agents = vec![working_codex, terminal, other_machine, idle_claude.clone()];
        let picked = pick_existing_installer_agent(&agents, "srv_a", None).expect("idle installer");
        assert_eq!(picked.agent_id, idle_claude.agent_id);
        assert_eq!(
            pick_existing_installer_agent(&agents, "srv_a", Some("codex"))
                .map(|agent| agent.agent_id.as_str()),
            None
        );
        assert!(pick_existing_installer_agent(&agents, "srv_missing", None).is_none());
        assert_eq!(
            interactive_agent_kind_for_command("cursor-agent"),
            Some("cursor")
        );
        assert_eq!(
            interactive_agent_kind_for_command("/usr/bin/claude"),
            Some("claude")
        );
    }

    #[test]
    fn agent_interface_command_and_transcript_round_trip() {
        let command = AgentCommand::Transcript {
            agent_id: "agent-1".to_string(),
            cursor: Some("42".to_string()),
            limit: Some(1),
        };
        let encoded = serde_json::to_value(&command).expect("serialize interface command");
        assert_eq!(encoded["action"], "transcript");
        assert_eq!(
            serde_json::from_value::<AgentCommand>(encoded).expect("deserialize interface command"),
            command
        );

        let descriptor = AgentInterfaceDescriptor {
            protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
            instance_id: "pi-one".to_string(),
            port: 4180,
            capabilities: vec!["prompt.submit".to_string(), "transcript.read".to_string()],
            ui_path: Some("/".to_string()),
            registered_at: "2026-08-24T12:00:00Z".parse().expect("timestamp"),
        };
        assert!(descriptor.supports("prompt.submit"));
        assert!(!descriptor.supports("abort"));
        let encoded = serde_json::to_value(&descriptor).expect("serialize descriptor");
        assert_eq!(
            serde_json::from_value::<AgentInterfaceDescriptor>(encoded)
                .expect("deserialize descriptor"),
            descriptor
        );

        let transcript = AgentTranscriptResponse {
            agent_id: "agent-1".to_string(),
            interface_instance_id: "pi-one".to_string(),
            page: Some(0),
            page_count: Some(2),
            next_page: Some(1),
            cursor: Some("0".to_string()),
            next_cursor: Some("1".to_string()),
            entries: vec![AgentTranscriptEntry {
                id: "entry-1".to_string(),
                kind: "message".to_string(),
                role: Some("user".to_string()),
                content: serde_json::json!("hello"),
                created_at: Some("2026-08-24T12:00:00Z".to_string()),
            }],
        };
        let encoded = serde_json::to_value(&transcript).expect("serialize transcript page");
        assert_eq!(encoded["page"], 0);
        assert_eq!(encoded["page_count"], 2);
        assert_eq!(encoded["next_page"], 1);
        assert_eq!(
            serde_json::from_value::<AgentTranscriptResponse>(encoded)
                .expect("deserialize transcript page"),
            transcript
        );
    }
}
