use super::*;

/// Stable identity attached to a Core Message.
///
/// `name`, `path`, and `role` are immutable display snapshots. Authorization always uses
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
    /// App-owned logical address; the authenticated principal remains `kind/id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub kind: MessagePrincipalKind,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl From<AppPrincipal> for MessagePrincipal {
    fn from(value: AppPrincipal) -> Self {
        Self {
            app_endpoint: None,
            path: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_endpoint: Option<String>,
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

/// One opaque, case-sensitive App address segment. Never a path or principal ID.
pub fn valid_message_app_endpoint(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 519
        && value.split('/').count() <= 8
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 64
                && segment
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
}

/// One lazy page of an App-owned subtree, served through its authenticated UI
/// tunnel at `api/tree?path=<relative/path>`. No node may select a transport URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTreePage {
    pub schema_version: u32,
    pub path: Vec<String>,
    pub nodes: Vec<AppTreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTreeNode {
    pub key: String,
    pub name: String,
    pub kind: AppTreeNodeKind,
    /// Known actions are tree.read, message.send, message.read. Unknown actions
    /// grant no behavior; clients must not infer capabilities from node kind.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppTreeNodeKind {
    App,
    Agent,
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
    #[serde(default)]
    pub app_owner_id: Option<String>,
    #[serde(default)]
    pub app_endpoint: Option<String>,
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
