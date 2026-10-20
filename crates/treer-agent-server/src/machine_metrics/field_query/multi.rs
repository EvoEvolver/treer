//! Decode-once staging assembler; not a public response or runtime route.
use super::*;
use std::collections::HashSet;
use treer_protocol::{MACHINE_METRICS_MAX_SERIES, MACHINE_METRICS_MAX_TOTAL_BUCKETS};

/// Aggregate entry cap across all requested fields, independent of viewers.
/// This bounds entries, not allocator overhead or process RSS.
pub const MAX_INDEX_ACQUISITIONS: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldKey {
    pub kind: MachineMetricKind,
    pub gpu_id: Option<String>,
}

pub struct MultiFieldHistory {
    pub epoch: u64,
    /// Best-effort maintenance edge; epoch is authoritative across Busy calls.
    pub reset: bool,
    /// Preserves request order. Missing fields still have empty partial buckets.
    pub fields: Vec<(FieldKey, Buckets)>,
}

impl DetachedFieldQuery {
    pub fn query_fields(
        &mut self,
        query: &MachineMetricsQuery,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<MultiFieldHistory, FieldQueryError> {
        self.query_fields_bounded(query, fields, cancelled, MAX_INDEX_ACQUISITIONS)
    }

    pub(in crate::machine_metrics) fn query_fields_bounded(
        &mut self,
        query: &MachineMetricsQuery,
        fields: &[FieldKey],
        cancelled: &mut impl FnMut() -> bool,
        limit: usize,
    ) -> Result<MultiFieldHistory, FieldQueryError> {
        let limit = limit.min(MAX_INDEX_ACQUISITIONS);
        let check = |cancelled: &mut _| {
            aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)
        };
        check(cancelled)?;
        if fields.is_empty() || fields.len() > MACHINE_METRICS_MAX_SERIES {
            return Err(FieldQueryError::InvalidFields);
        }
        let mut keys = HashSet::new();
        let mut units = Vec::with_capacity(fields.len());
        for field in fields {
            let unit =
                field_unit(field.kind, field.gpu_id.as_deref()).map_err(FieldQueryError::Cache)?;
            if !keys.insert((field.kind, field.gpu_id.as_deref())) {
                return Err(FieldQueryError::InvalidFields);
            }
            units.push(unit);
        }
        // Validate the original request BEFORE reducing its per-series budget.
        if !query.valid_at(self.clock.wall) {
            return Err(FieldQueryError::Aggregation(AggregationError::InvalidQuery));
        }
        let query = MachineMetricsQuery {
            max_points: query
                .max_points
                .min((MACHINE_METRICS_MAX_TOTAL_BUCKETS / fields.len()) as u16),
            ..query.clone()
        };
        aggregation::aggregate_cancellable(&query, self.clock.wall, units[0], [], cancelled)
            .map_err(FieldQueryError::Aggregation)?;
        let positions: HashMap<_, _> = fields
            .iter()
            .enumerate()
            .map(|(index, field)| ((field.kind, field.gpu_id.as_deref()), index))
            .collect();
        // Collector ticks are globally unique across fields in one epoch.
        // Store each original acquisition once, including cached references
        // outside the requested range so identity conflicts cannot be hidden.
        let mut index: HashMap<(u64, u64), (usize, Observation)> = HashMap::new();
        let mut counts = vec![0; fields.len()];
        for (clock, bytes) in self.history.frames() {
            check(cancelled)?;
            let stored =
                StoredFrame::decode(bytes, self.history.server_id(), clock, self.history.epoch())
                    .map_err(FieldQueryError::History)?;
            for field in stored.fields {
                check(cancelled)?;
                let Some(&position) = positions.get(&(field.kind, field.gpu_id.as_deref())) else {
                    continue;
                };
                let observation = field.observation;
                let identity = (observation.epoch, observation.tick);
                if let Some(previous) = index.get(&identity) {
                    if *previous != (position, observation) {
                        return Err(FieldQueryError::ConflictingAcquisition);
                    }
                } else {
                    if index.len() >= limit || counts[position] >= MAX_RECORDS {
                        return Err(FieldQueryError::IndexLimit);
                    }
                    index.insert(identity, (position, observation));
                    counts[position] += 1;
                }
            }
        }
        let mut observations: Vec<Vec<Observation>> =
            counts.into_iter().map(Vec::with_capacity).collect();
        for (_, (position, observation)) in index {
            check(cancelled)?;
            observations[position].push(observation);
        }
        let mut output = Vec::with_capacity(fields.len());
        for ((field, unit), observations) in fields.iter().zip(units).zip(observations) {
            check(cancelled)?;
            let buckets = aggregation::aggregate_cancellable(
                &query,
                self.clock.wall,
                unit,
                observations,
                cancelled,
            )
            .map_err(FieldQueryError::Aggregation)?;
            output.push((field.clone(), buckets));
        }
        check(cancelled)?;
        Ok(MultiFieldHistory {
            epoch: self.history.epoch(),
            reset: self.reset,
            fields: output,
        })
    }
}
