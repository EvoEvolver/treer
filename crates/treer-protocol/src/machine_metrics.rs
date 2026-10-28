//! Machine hardware metrics (`machine.metrics.v1`). The Controller samples
//! whole-machine CPU, RAM and (NVIDIA) GPU load into a bounded in-memory
//! history; the Proxy asks for a window on demand and stores nothing.
use super::*;

pub const CAPABILITY_MACHINE_METRICS: &str = "machine.metrics.v1";
/// Sampling period of the Controller.
pub const MACHINE_METRICS_SAMPLE_SECONDS: u32 = 5;
/// Windows up to this length are answered from raw samples.
pub const MACHINE_METRICS_RAW_SECONDS: u32 = 60 * 60;
/// Longer windows are answered from per-minute means.
pub const MACHINE_METRICS_BUCKET_SECONDS: u32 = 60;
/// Longest window the Controller retains and answers.
pub const MACHINE_METRICS_MAX_WINDOW_SECONDS: u32 = 24 * 60 * 60;
pub const MACHINE_METRICS_DEFAULT_WINDOW_SECONDS: u32 = 60 * 60;

/// One reading, or the mean of the readings in one minute bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetricsSample {
    pub at: DateTime<Utc>,
    /// Whole-machine utilization, 0..=100 (not a sum of per-core values).
    pub cpu_percent: f32,
    /// `total - available`, so reclaimable caches do not count as used.
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuSample>,
}

/// Fields a driver cannot report stay `None`; they are never shown as zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSample {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization_percent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuStatus {
    /// `nvidia-smi` reports at least one device.
    Available,
    /// No NVIDIA driver tooling or device was found (or it stopped answering).
    NotDetected,
    /// The platform has no supported GPU source (macOS).
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineMetrics {
    pub cpu_count: u32,
    pub gpu_status: GpuStatus,
    #[serde(default)]
    pub gpus: Vec<GpuDevice>,
    pub sample_seconds: u32,
    /// Spacing of `samples`: `sample_seconds`, or 60 for long windows.
    pub resolution_seconds: u32,
    pub window_seconds: u32,
    /// Oldest first. Missing readings are absent, never zero-filled.
    pub samples: Vec<MachineMetricsSample>,
    /// The newest raw reading, independent of the window's resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<MachineMetricsSample>,
}
