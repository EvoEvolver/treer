//! Proposed machine-local metrics wire contract (#220). Not advertised until
//! collection, authorization and bounded routing are implemented.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod validation;
pub use validation::MachineMetricsValidationError;

pub const CAPABILITY_MACHINE_METRICS: &str = "machine.metrics.v1";
pub const MACHINE_METRICS_MAX_RANGE_MS: i64 = 86_400_000;
pub const MACHINE_METRICS_MAX_POINTS: u16 = 720;
pub const MACHINE_METRICS_MAX_GPUS: usize = 32;
pub const MACHINE_METRICS_MAX_SERIES: usize = 4 + 3 * MACHINE_METRICS_MAX_GPUS;
pub const MACHINE_METRICS_MAX_TOTAL_BUCKETS: usize = 7200;
pub const MACHINE_METRICS_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MACHINE_METRICS_SAMPLE_INTERVAL_MS: u64 = 5000;
pub const MACHINE_METRICS_STALE_AFTER_MS: u64 = 15_000;

/// Both timestamps are resolved explicitly by the API before Controller
/// dispatch. Unknown query parameters must not silently alter the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineMetricsQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub max_points: u16,
}

impl MachineMetricsQuery {
    pub fn valid_at(&self, now: DateTime<Utc>) -> bool {
        let range = self.to.signed_duration_since(self.from);
        range > chrono::Duration::zero()
            && range <= chrono::Duration::milliseconds(MACHINE_METRICS_MAX_RANGE_MS)
            && self.to <= now
            && (1..=MACHINE_METRICS_MAX_POINTS).contains(&self.max_points)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineMetricStatus {
    Ok,
    WarmingUp,
    NoDevice,
    DriverUnavailable,
    UnsupportedPlatform,
    UnsupportedMetric,
    CollectionFailed,
    TimedOut,
    Stale,
    DeviceAbsent,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineMetricKind {
    CpuUtilization,
    RamUsed,
    RamAvailable,
    RamTotal,
    GpuUtilization,
    GpuMemoryUsed,
    GpuMemoryTotal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineMetricUnit {
    Percent,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineMemoryMethod {
    LinuxMemAvailable,
    MacosAvailableEstimate,
    WindowsPhysicalAvailable,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineMetricsGpu {
    /// Opaque per-installation salted key, never a raw UUID, serial or PCI ID.
    pub id: String,
    /// Plain display text, at most 128 UTF-8 bytes; no driver/error text.
    pub name: String,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetricCurrent {
    pub status: MachineMetricStatus,
    /// Null unless status is ok; never manufacture zero for unavailable data.
    pub value: Option<f64>,
    /// Actual acquisition time of THIS field, independent of the history range.
    pub sampled_at: Option<DateTime<Utc>>,
    pub age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineMetricStatusCount {
    pub status: MachineMetricStatus,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetricBucket {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub expected_count: u32,
    pub valid_count: u32,
    /// Disjoint observed-slot counts, including failures; absent slots are the
    /// difference between expected_count and their sum. No synthetic zero slot.
    pub status_counts: Vec<MachineMetricStatusCount>,
    pub first_sample_at: Option<DateTime<Utc>>,
    pub last_sample_at: Option<DateTime<Utc>>,
    pub mean: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// True on any missing/invalid slot, epoch or capacity discontinuity.
    /// Clients must break continuity here, even if some valid values remain.
    pub partial: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetricSeries {
    pub kind: MachineMetricKind,
    /// Required only for GPU kinds, referring to the response GPU descriptors.
    pub gpu_id: Option<String>,
    pub unit: MachineMetricUnit,
    pub current: MachineMetricCurrent,
    /// Same bucket boundaries across every series; <= query.max_points each.
    pub points: Vec<MachineMetricBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetricsResponse {
    pub schema: String,
    pub server_id: String,
    pub generated_at: DateTime<Utc>,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub retained_from: Option<DateTime<Utc>>,
    pub truncated: bool,
    pub sample_interval_ms: u64,
    pub bucket_width_ms: u64,
    pub stale_after_ms: u64,
    pub logical_cpus: Option<u32>,
    pub memory_method: MachineMemoryMethod,
    /// Fixed "os_visible_machine", not a cgroup or physical-host claim.
    pub scope: String,
    /// Union of current and retained range devices, at most 32, never re-keyed
    /// to a new card when a numeric device index is reused.
    pub gpus: Vec<MachineMetricsGpu>,
    pub inventory_truncated: bool,
    pub gpu_status: MachineMetricStatus,
    /// Four system series plus three per returned GPU, maximum 100 series.
    pub series: Vec<MachineMetricSeries>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_metrics_query_enforces_exact_range_and_point_bounds() {
        let now = DateTime::from_timestamp(1_792_500_000, 0).unwrap();
        let mut query = MachineMetricsQuery {
            from: now - chrono::Duration::hours(24),
            to: now,
            max_points: 720,
        };
        assert!(query.valid_at(now));
        query.from -= chrono::Duration::nanoseconds(1);
        assert!(!query.valid_at(now));
        query.from = now;
        assert!(!query.valid_at(now));
        query.from = now - chrono::Duration::minutes(15);
        query.to = now + chrono::Duration::nanoseconds(1);
        assert!(!query.valid_at(now));
        query.to = now;
        for points in [0, 721, u16::MAX] {
            query.max_points = points;
            assert!(!query.valid_at(now));
        }
        query.max_points = 360;
        let mut wire = serde_json::to_value(&query).unwrap();
        wire["recipient"] = serde_json::json!("another-machine");
        assert!(serde_json::from_value::<MachineMetricsQuery>(wire).is_err());
    }

    #[test]
    fn machine_metrics_status_has_no_arbitrary_error_text() {
        assert_eq!(
            serde_json::to_value(MachineMetricStatus::DeviceAbsent).unwrap(),
            serde_json::json!("device_absent")
        );
        assert!(
            serde_json::from_value::<MachineMetricStatus>(serde_json::json!(
                "NVML: private driver diagnostic"
            ))
            .is_err()
        );
    }
}
