//! Inert system-only response assembly. GPU inventory requires a later path.
use super::*;
use crate::machine_metrics::response_encoding::{self, EncodingError};
use treer_protocol::{
    MachineMemoryMethod, MachineMetricCurrent, MachineMetricSeries, MachineMetricStatus,
    MachineMetricsResponse, CAPABILITY_MACHINE_METRICS, MACHINE_METRICS_SAMPLE_INTERVAL_MS,
    MACHINE_METRICS_STALE_AFTER_MS,
};

/// Static collector/platform metadata, never request-provided measurements.
pub struct SystemMetadata {
    pub logical_cpus: Option<u32>,
    pub memory_method: MachineMemoryMethod,
    pub gpu_status: MachineMetricStatus,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SystemResponseError {
    InventoryRequired,
    InvalidMetadata,
    Query(FieldQueryError),
    Encoding(EncodingError),
}

pub struct SystemResponse {
    /// Caller must compare the live collector epoch again before publication.
    pub epoch: u64,
    /// Exact-length private bytes; the encoder's spare 1 MiB is not retained.
    pub bytes: Box<[u8]>,
}

impl DetachedFieldQuery {
    pub fn assemble_system_response(
        &mut self,
        query: &MachineMetricsQuery,
        metadata: SystemMetadata,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<SystemResponse, SystemResponseError> {
        aggregation::check_cancelled(cancelled)
            .map_err(|error| SystemResponseError::Query(FieldQueryError::Aggregation(error)))?;
        if !matches!(
            metadata.gpu_status,
            MachineMetricStatus::NoDevice
                | MachineMetricStatus::DriverUnavailable
                | MachineMetricStatus::UnsupportedPlatform
                | MachineMetricStatus::Disabled
        ) {
            return Err(SystemResponseError::InvalidMetadata);
        }
        if metadata.logical_cpus.is_some_and(|count| {
            !(1..=treer_protocol::MACHINE_METRICS_MAX_LOGICAL_CPUS).contains(&count)
        }) {
            return Err(SystemResponseError::InvalidMetadata);
        }
        if self.current.iter().any(|field| field.gpu_id.is_some())
            || self
                .inventory
                .as_ref()
                .is_some_and(|capture| !capture.gpus.is_empty())
        {
            return Err(SystemResponseError::InventoryRequired);
        }
        let fields: Vec<_> = [
            MachineMetricKind::CpuUtilization,
            MachineMetricKind::RamUsed,
            MachineMetricKind::RamAvailable,
            MachineMetricKind::RamTotal,
        ]
        .into_iter()
        .map(|kind| FieldKey { kind, gpu_id: None })
        .collect();
        let history = self
            .query_fields(query, &fields, cancelled)
            .map_err(SystemResponseError::Query)?;
        if history.has_gpu_inventory {
            return Err(SystemResponseError::InventoryRequired);
        }
        // Conservative instance-wide discontinuity flag until per-range loss
        // metadata exists. Never claim complete coverage after a known loss.
        let truncated = self.history.truncated();
        let bucket_width_ms = history.fields[0].1.width_ms;
        let series = history
            .fields
            .into_iter()
            .map(|(key, mut buckets)| {
                if truncated {
                    for bucket in &mut buckets.points {
                        bucket.partial = true;
                    }
                }
                let current = self
                    .current
                    .iter()
                    .find(|field| field.kind == key.kind && field.gpu_id.is_none())
                    .map(|field| field.current.clone())
                    .unwrap_or(MachineMetricCurrent {
                        status: MachineMetricStatus::WarmingUp,
                        value: None,
                        sampled_at: None,
                        age_ms: None,
                    });
                Ok(MachineMetricSeries {
                    kind: key.kind,
                    gpu_id: None,
                    unit: field_unit(key.kind, None).map_err(|error| {
                        SystemResponseError::Query(FieldQueryError::Cache(error))
                    })?,
                    current,
                    points: buckets.points,
                })
            })
            .collect::<Result<Vec<_>, SystemResponseError>>()?;
        let response = MachineMetricsResponse {
            schema: CAPABILITY_MACHINE_METRICS.into(),
            server_id: self.history.server_id().into(),
            generated_at: self.clock.wall,
            from: query.from,
            to: query.to,
            retained_from: self.history.retained_from(),
            truncated,
            sample_interval_ms: MACHINE_METRICS_SAMPLE_INTERVAL_MS,
            bucket_width_ms,
            stale_after_ms: MACHINE_METRICS_STALE_AFTER_MS,
            logical_cpus: metadata.logical_cpus,
            memory_method: metadata.memory_method,
            scope: "os_visible_machine".into(),
            gpus: Vec::new(),
            inventory_truncated: false,
            gpu_status: metadata.gpu_status,
            series,
        };
        let bytes = response_encoding::encode_bounded(
            &response,
            query,
            self.history.server_id(),
            self.clock.wall,
            cancelled,
        )
        .map_err(SystemResponseError::Encoding)?
        .into_boxed_slice();
        // Compacting the Vec can move bytes: check again before returning them.
        aggregation::check_cancelled(cancelled)
            .map_err(|error| SystemResponseError::Query(FieldQueryError::Aggregation(error)))?;
        Ok(SystemResponse {
            epoch: history.epoch,
            bytes,
        })
    }
}
