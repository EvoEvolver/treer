//! Organization, workspace and account administration wire models shared by
//! the Proxy, the Console and Agents calling the administration routes.
use super::*;

/// Default and bounds for `CreateInvitationRequest::expires_in_hours`.
pub const INVITATION_DEFAULT_EXPIRES_HOURS: u32 = 168;
pub const INVITATION_MAX_EXPIRES_HOURS: u32 = 720;
/// Agent administration routes (`/agent/workspaces/{ws}/admin/...`) exist.
pub const CAPABILITY_AGENT_ADMIN: &str = "agent.admin.v1";
/// Settings administration routes (invitation list/revoke, organization
/// leave/transfer/delete, sessions, password, effective workspace access).
pub const CAPABILITY_SETTINGS_ADMIN: &str = "settings.admin.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationRole {
    Owner,
    Admin,
    Member,
}

impl OrganizationRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// Who did something: a user (`kind: "user"`) or an Agent (`kind: "agent"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorSummary {
    pub id: String,
    pub name: String,
    #[serde(default = "default_actor_kind")]
    pub kind: String,
}

fn default_invitation_role() -> OrganizationRole {
    OrganizationRole::Member
}

fn default_actor_kind() -> String {
    "user".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Defaults to `member` (older Consoles post `{}`).
    #[serde(default = "default_invitation_role")]
    pub role: OrganizationRole,
    /// 1..=720, default 168.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_hours: Option<u32>,
    /// Organization groups the new member joins on acceptance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_ids: Vec<String>,
    /// Workspaces the new member is granted (`member` role) on acceptance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_ids: Vec<String>,
}

/// The only response that carries the invitation secret (inside `url`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedInvitation {
    pub id: String,
    pub url: String,
    pub email: Option<String>,
    pub role: OrganizationRole,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub created_by: ActorSummary,
    pub group_ids: Vec<String>,
    pub workspace_ids: Vec<String>,
    /// True when `email` was given and the Proxy's email sender accepted the
    /// job; delivery is asynchronous. False means: share `url` yourself.
    pub email_queued: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationSummary {
    pub id: String,
    pub email: Option<String>,
    pub role: OrganizationRole,
    pub status: InvitationStatus,
    pub created_at: DateTime<Utc>,
    /// Absent for invitations created before expiry existed (they never expire).
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by: ActorSummary,
    pub group_ids: Vec<String>,
    pub workspace_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationList {
    pub invitations: Vec<InvitationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferOrganizationRequest {
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteOrganizationRequest {
    pub confirm_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteWorkspaceRequest {
    /// Required with `revoke_machines`; when present it must equal the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_name: Option<String>,
    /// Revoke every machine credential and delete the machine records in the
    /// same transaction instead of refusing while machines remain.
    #[serde(default)]
    pub revoke_machines: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSessionInfo {
    pub id: String,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSessionList {
    pub sessions: Vec<AuthSessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSummary {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessViaKind {
    OrgRole,
    Group,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessVia {
    pub kind: AccessViaKind,
    /// `org_role`: the organization role; `group`: `"<group name> (<role>)"`;
    /// `direct`: the direct grant's role.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveAccessMember {
    pub user: UserSummary,
    /// Effective workspace role: `owner` or `member`.
    pub role: String,
    pub via: Vec<AccessVia>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveWorkspaceAccess {
    pub members: Vec<EffectiveAccessMember>,
    /// Workspace access mode: `organization` or `restricted`.
    pub mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_request_rejects_unknown_fields_and_parses_roles() {
        let request: CreateInvitationRequest =
            serde_json::from_value(serde_json::json!({"role": "admin"})).unwrap();
        assert_eq!(request.role, OrganizationRole::Admin);
        assert!(request.group_ids.is_empty() && request.expires_in_hours.is_none());
        let legacy: CreateInvitationRequest =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(legacy.role, OrganizationRole::Member);
        assert!(serde_json::from_value::<CreateInvitationRequest>(
            serde_json::json!({"role": "member", "token": "x"})
        )
        .is_err());
        assert!(serde_json::from_value::<CreateInvitationRequest>(
            serde_json::json!({"role": "superuser"})
        )
        .is_err());
        let delete: DeleteWorkspaceRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!delete.revoke_machines && delete.confirm_name.is_none());
    }
}
