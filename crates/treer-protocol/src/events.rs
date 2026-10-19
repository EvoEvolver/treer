use super::*;

pub const WORKSPACE_AUDIT_CAPABILITY: &str = "workspace.audit.v1";
pub const AUDIT_MAINTENANCE_CAPABILITY: &str = "audit.maintenance.v1";
/// `GET /api/workspaces/{ws}/audit-summary` (#191) and the list route's
/// `category`, `outcome`, `actor_kind`, `resource_id`, `notable` and
/// `sequence` filters.
pub const WORKSPACE_AUDIT_SUMMARY_CAPABILITY: &str = "workspace.audit-summary.v1";

/// Preset ranges ending now; `since`/`until` are the explicit alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSummaryRange {
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSummaryBucket {
    Hour,
    Day,
    Week,
}

impl AuditSummaryBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditSummaryBucket::Hour => "hour",
            AuditSummaryBucket::Day => "day",
            AuditSummaryBucket::Week => "week",
        }
    }
}

/// Query of the summary route; every filter also applies to the list route.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAuditSummaryQuery {
    pub range: Option<AuditSummaryRange>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub bucket: Option<AuditSummaryBucket>,
    /// IANA zone for bucket boundaries; default UTC.
    pub tz: Option<String>,
    pub actor_kind: Option<String>,
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub category: Option<AuditCategory>,
    pub resource_kind: Option<String>,
    pub resource_id: Option<String>,
    pub outcome: Option<String>,
    #[serde(default)]
    pub notable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAuditSummary {
    pub workspace_id: String,
    pub category_table_version: u32,
    pub range: AuditSummaryRangeEcho,
    pub filters: AuditSummaryFilters,
    pub totals: AuditSummaryCount,
    pub categories: Vec<AuditSummaryCategoryCount>,
    pub series: Vec<AuditSummaryBucketRow>,
    pub actors: AuditSummaryRanking<AuditSummaryActor>,
    pub resources: AuditSummaryResources,
    pub notable_total: i64,
    pub notable: Vec<AuditSummaryNotable>,
    /// Actions whose notable rows were capped at 20 and how many were omitted.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub notable_capped: std::collections::BTreeMap<String, i64>,
    pub anomalies: AuditSummaryAnomalies,
    pub maintenance: AuditMaintenance,
    pub coverage: AuditSummaryCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryRangeEcho {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub bucket: AuditSummaryBucket,
    pub tz: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummaryFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<AuditCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default)]
    pub notable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummaryCount {
    pub events: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryCategoryCount {
    pub category: AuditCategory,
    pub events: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryBucketRow {
    pub bucket_start: DateTime<Utc>,
    pub bucket_end: DateTime<Utc>,
    /// Drill-down window clipped to the requested range.
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub events: i64,
    pub failed: i64,
    pub by_category: std::collections::BTreeMap<AuditCategory, i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryRanking<T> {
    pub top: Vec<T>,
    /// Distinct keys in the whole range (not only the listed ones).
    pub distinct: i64,
    /// Events belonging to keys not listed in `top`.
    pub omitted_events: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryActor {
    pub actor_kind: String,
    /// Exact as stored (absent for system actors); `drillable` is false when
    /// it is absent or exceeds the filter validator's limits, in which case
    /// the Console must not build a filter from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub drillable: bool,
    pub events: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryResource {
    pub resource_id: String,
    pub drillable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    pub events: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryResources {
    pub agent: AuditSummaryRanking<AuditSummaryResource>,
    pub machine: AuditSummaryRanking<AuditSummaryResource>,
    pub app: AuditSummaryRanking<AuditSummaryResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryNotable {
    pub sequence: i64,
    pub occurred_at: DateTime<Utc>,
    pub action: String,
    pub category: AuditCategory,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub outcome: String,
    pub resource_kind: String,
    pub resource_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAnomalyKind {
    VolumeSpike,
    FailureBurst,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSummaryAnomaly {
    pub kind: AuditAnomalyKind,
    pub bucket_start: DateTime<Utc>,
    pub observed: i64,
    /// Median of the other non-empty buckets (events, or failed events).
    pub baseline: f64,
    pub threshold: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummaryAnomalies {
    /// False when fewer than three other non-empty buckets exist for the rule.
    pub volume_evaluated: bool,
    pub failure_evaluated: bool,
    pub items: Vec<AuditSummaryAnomaly>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummaryCoverage {
    /// Oldest instant among the counted rows, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_in_range: Option<DateTime<Utc>>,
    pub backfill_complete: bool,
    /// Rows newer than this carry the typed instant; a range reaching below it
    /// is partial and must be shown as such.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_since: Option<DateTime<Utc>>,
}

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
    // Shared with the summary route (#191); older Proxies reject them as
    // unknown parameters, which the capability flag tells the Console.
    pub actor_kind: Option<String>,
    pub category: Option<AuditCategory>,
    pub resource_id: Option<String>,
    pub outcome: Option<String>,
    #[serde(default)]
    pub notable: bool,
    /// Exactly one event, for the notable timeline's drill-down.
    pub sequence: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAuditEventsPage {
    pub events: Vec<OrganizationAuditEvent>,
    pub next_cursor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance: Option<AuditMaintenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_table_version: Option<u32>,
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
    /// Present only with statistics; scoped indicator of retention backlog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_expired_at: Option<DateTime<Utc>>,
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
