use super::*;

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
