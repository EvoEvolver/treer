use super::*;

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

/// Discovery document served by a Policy App at `/v1/manifest`.
/// Unknown capabilities are ignored so providers can add optional features.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyProviderManifest {
    pub protocol: String,
    pub provider_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Immutable policy snapshot served by a Policy App at `/v1/policy/bundle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyProviderBundle {
    pub protocol: String,
    pub workspace_id: String,
    pub revision: u64,
    pub mode: PolicyMode,
    pub document: WorkspacePolicyDocument,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyProviderFailureMode {
    #[default]
    FailClosed,
    FailOpen,
}

impl PolicyProviderFailureMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FailClosed => "fail_closed",
            Self::FailOpen => "fail_open",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePolicyProvider {
    pub workspace_id: String,
    pub app_id: String,
    pub service_id: String,
    pub failure_mode: PolicyProviderFailureMode,
    pub max_stale_seconds: u64,
    pub revision_hint: u64,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetWorkspacePolicyProviderRequest {
    pub app_id: String,
    #[serde(default)]
    pub failure_mode: PolicyProviderFailureMode,
    #[serde(default = "default_policy_provider_max_stale_seconds")]
    pub max_stale_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallDefaultPolicyAppRequest {
    pub server_id: String,
}

const fn default_policy_provider_max_stale_seconds() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyProviderInvalidationRequest {
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyProviderInvalidationResponse {
    pub accepted_revision: u64,
}
