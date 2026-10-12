use serde::{Deserialize, Serialize};

/// App notification transport. No caller-selected sender, Reply-To or HTML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendAppEmailRequest {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendAppEmailResponse {
    pub email_id: String,
    pub status: String,
}
