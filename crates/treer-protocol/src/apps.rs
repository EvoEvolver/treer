use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Service-audience identity shared by App OAuth and trusted browser forwarding.
/// OAuth human tokens last 12 hours; forwarded human tokens last 60 seconds.
/// Consumers must verify signature, audience, expiry and current membership;
/// a human token does not remove the need for browser Origin/Fetch Metadata checks.
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
