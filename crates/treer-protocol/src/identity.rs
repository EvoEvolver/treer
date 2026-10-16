use super::*;

/// Administrative projection; neither field can be used as an invitation bearer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInvitationRecord {
    pub invitation_id: String,
    pub token_prefix: String,
    pub created_at: String,
    pub used_at: Option<String>,
}

pub const ORGANIZATION_INVITATION_CAPABILITY: &str = "organization.invitation-accept.v1";

#[cfg(test)]
mod invitation_contract_tests {
    use super::*;

    #[test]
    fn invitation_accept_supports_token_or_pending_without_extra_fields() {
        let token: InvitationAcceptRequest =
            serde_json::from_value(serde_json::json!({"token":"inv_fixture"})).unwrap();
        assert_eq!(token.token.as_deref(), Some("inv_fixture"));
        assert!(token.pending_id.is_none());
        let pending: InvitationAcceptRequest =
            serde_json::from_value(serde_json::json!({"pending_id":"fixture"})).unwrap();
        assert!(pending.token.is_none());
        assert_eq!(
            serde_json::to_value(pending).unwrap()["pending_id"],
            "fixture"
        );
        assert!(serde_json::from_value::<InvitationAcceptRequest>(
            serde_json::json!({"token":"inv_fixture","user_id":"forged"})
        )
        .is_err());
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationTokenRequest {
    pub token: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationAcceptRequest {
    pub token: Option<String>,
    pub pending_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InvitationPreview {
    pub capabilities: Vec<String>,
    pub status: String,
    pub kind: Option<String>,
    pub organization_id: Option<String>,
    pub organization_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InvitationAcceptance {
    pub organization_id: String,
    pub role: String,
    pub already_member: bool,
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
    /// Current canonical Tree path, never a client-supplied or token-cached path.
    /// None for humans and unplaced legacy workloads; absent on older Proxies.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

pub const APP_DIRECTORY_PATH_CAPABILITY: &str = "app.directory-paths.v1";

#[cfg(test)]
mod app_principal_tests {
    use super::*;

    #[test]
    fn app_principal_path_round_trip_and_legacy_response() {
        let value = serde_json::json!({"kind":"agent","id":"a","name":"reviewer","path":"/research/reviewer"});
        let principal: AppPrincipal = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(principal).unwrap(), value);
        let legacy: AppPrincipal =
            serde_json::from_value(serde_json::json!({"kind":"agent","id":"a","name":"reviewer"}))
                .unwrap();
        assert!(legacy.path.is_none());
        // A missing path never invents a canonical root from a display name.
        assert!(serde_json::to_value(legacy).unwrap()["path"].is_null());
    }
}
