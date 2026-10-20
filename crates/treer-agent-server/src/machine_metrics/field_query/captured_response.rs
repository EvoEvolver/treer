//! Inert complete-series assembly with known enumeration; no runtime route.
use super::*;
use crate::machine_metrics::response_encoding::{self, EncodingError};
use treer_protocol::{
    MachineMemoryMethod, MachineMetricCurrent, MachineMetricSeries, MachineMetricStatus,
    MachineMetricsResponse, CAPABILITY_MACHINE_METRICS, MACHINE_METRICS_SAMPLE_INTERVAL_MS,
    MACHINE_METRICS_STALE_AFTER_MS,
};

/// Collector/platform metadata, never measurements or device presence.
pub struct CapturedMetadata {
    pub logical_cpus: Option<u32>,
    pub memory_method: MachineMemoryMethod,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CapturedResponseError {
    UnknownInventory,
    IncompleteSeries,
    Query(FieldQueryError),
    Encoding(EncodingError),
}

impl DetachedFieldQuery {
    /// The local caller supplies the complete field plan. Missing/extra keys
    /// refuse rather than silently publishing a partial inventory or series set.
    /// Query decoding runs once; automatic field-plan discovery is still open.
    pub fn assemble_captured_response(
        &mut self,
        query: &MachineMetricsQuery,
        metadata: CapturedMetadata,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<SystemResponse, CapturedResponseError> {
        let history = self
            .query_fields(query, fields, cancelled)
            .map_err(CapturedResponseError::Query)?;
        let inventory = history
            .inventory
            .ok_or(CapturedResponseError::UnknownInventory)?;
        let gpu_current = history
            .gpu_current
            .ok_or(CapturedResponseError::UnknownInventory)?;
        let mut required: Vec<_> = [
            MachineMetricKind::CpuUtilization,
            MachineMetricKind::RamUsed,
            MachineMetricKind::RamAvailable,
            MachineMetricKind::RamTotal,
        ]
        .into_iter()
        .map(|kind| FieldKey { kind, gpu_id: None })
        .collect();
        required.extend(gpu_current.iter().map(|(key, _)| key.clone()));
        if history.fields.len() != required.len()
            || required
                .iter()
                .any(|key| !history.fields.iter().any(|(actual, _)| actual == key))
        {
            return Err(CapturedResponseError::IncompleteSeries);
        }
        let truncated = self.history.truncated();
        let bucket_width_ms = history.fields[0].1.width_ms;
        let mut series = Vec::with_capacity(required.len());
        // Required keys are system-first, then sorted GPU id and utilization,
        // used, total, as produced by the captured GPU projection. The caller's
        // plan controls neither the wire order nor its byte representation.
        let mut remaining = history.fields;
        for key in required {
            let position = remaining
                .iter()
                .position(|(actual, _)| *actual == key)
                .ok_or(CapturedResponseError::IncompleteSeries)?;
            let (_, mut buckets) = remaining.swap_remove(position);
            aggregation::check_cancelled(cancelled).map_err(|error| {
                CapturedResponseError::Query(FieldQueryError::Aggregation(error))
            })?;
            if truncated {
                for bucket in &mut buckets.points {
                    bucket.partial = true;
                }
            }
            let current = if key.gpu_id.is_some() {
                gpu_current
                    .iter()
                    .find(|(actual, _)| *actual == key)
                    .map(|(_, current)| current.clone())
                    .ok_or(CapturedResponseError::IncompleteSeries)?
            } else {
                self.current
                    .iter()
                    .find(|field| field.kind == key.kind && field.gpu_id.is_none())
                    .map(|field| field.current.clone())
                    .unwrap_or(MachineMetricCurrent {
                        status: MachineMetricStatus::WarmingUp,
                        value: None,
                        sampled_at: None,
                        age_ms: None,
                    })
            };
            let unit = field_unit(key.kind, key.gpu_id.as_deref())
                .map_err(|error| CapturedResponseError::Query(FieldQueryError::Cache(error)))?;
            series.push(MachineMetricSeries {
                kind: key.kind,
                gpu_id: key.gpu_id,
                unit,
                current,
                points: buckets.points,
            });
        }
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
            gpus: inventory.gpus,
            inventory_truncated: false,
            gpu_status: inventory.gpu_status,
            series,
        };
        let bytes = response_encoding::encode_bounded(
            &response,
            query,
            self.history.server_id(),
            self.clock.wall,
            cancelled,
        )
        .map_err(CapturedResponseError::Encoding)?
        .into_boxed_slice();
        aggregation::check_cancelled(cancelled)
            .map_err(|error| CapturedResponseError::Query(FieldQueryError::Aggregation(error)))?;
        Ok(SystemResponse {
            epoch: history.epoch,
            bytes,
        })
    }
}
