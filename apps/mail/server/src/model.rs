use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use treer_protocol::AppPrincipal;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub message_id: String,
    pub workspace_id: String,
    pub sender: AppPrincipal,
    pub recipients: Vec<AppPrincipal>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub message: Message,
    pub unread: bool,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct InboxRequest {
    #[serde(default = "default_inbox_limit")]
    pub limit: u16,
}

const fn default_inbox_limit() -> u16 {
    50
}

#[derive(Debug, Serialize)]
pub struct MailboxResponse {
    pub deliveries: Vec<Delivery>,
    pub remaining_unread: u64,
}

#[derive(Debug, Clone)]
pub struct HumanSession {
    pub token_hash: String,
    pub access_token: String,
    pub workspace_id: String,
    pub service_id: String,
    pub user_id: String,
    pub preferred_name: String,
    pub role: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PendingOAuth {
    pub state_hash: String,
    pub verifier: String,
    pub return_path: String,
    pub expires_at: DateTime<Utc>,
}
