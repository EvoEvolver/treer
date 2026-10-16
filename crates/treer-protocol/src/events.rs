use super::*;

pub const WORKSPACE_AUDIT_CAPABILITY: &str = "workspace.audit.v1";
pub const AUDIT_MAINTENANCE_CAPABILITY: &str = "audit.maintenance.v1";

/// Exclusive sequence cursor; time bounds are inclusive `since`, exclusive `until`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAuditEventsQuery {
    pub before: Option<i64>,
    pub limit: Option<u16>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub resource_kind: Option<String>,
    #[serde(default)]
    pub include_statistics: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAuditEventsPage {
    pub events: Vec<OrganizationAuditEvent>,
    pub next_cursor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance: Option<AuditMaintenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditMaintenance {
    pub retention_days: i32,
    /// Deletion watermark, not proof that later events form a complete history.
    pub deleted_through: Option<DateTime<Utc>>,
    /// Fixed recent 30-day UTC window, independent of event pagination filters.
    pub statistics_since: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_per_day: Option<Vec<AuditDailyRows>>,
    #[serde(default)]
    pub statistics_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditDailyRows {
    pub day: String,
    pub action: String,
    pub rows: i64,
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
