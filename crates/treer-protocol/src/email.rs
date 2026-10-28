use serde::{Deserialize, Serialize};

/// Proxy route `POST /api/apps/{service_id}/email` and Controller `POST /api/email`.
pub const CAPABILITY_APP_EMAIL: &str = "app.email.v1";

/// App email transport. No caller-selected sender, Reply-To or HTML.
///
/// Exactly one recipient field: `to` (any valid address) or `to_member` (a
/// workspace member's user ID; Core resolves that member's verified account
/// email).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendAppEmailRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub subject: String,
    pub text: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_member: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendAppEmailResponse {
    pub email_id: String,
    pub status: String,
}
