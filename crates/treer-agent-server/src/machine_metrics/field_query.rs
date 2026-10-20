//! Single-field staging reader. Not the public multi-series query assembler.
use super::{
    aggregation::{self, AggregationError, Buckets, Observation},
    field_cache::{field_unit, CacheError},
    history::{Clock, History, HistoryError, MAX_RECORDS},
    stored_frame::StoredFrame,
};
use std::collections::HashMap;
use treer_protocol::{MachineMetricKind, MachineMetricsQuery};

#[derive(Debug, PartialEq, Eq)]
pub enum FieldQueryError {
    History(HistoryError),
    Cache(CacheError),
    Aggregation(AggregationError),
    ConflictingAcquisition,
}

pub struct FieldHistory {
    pub epoch: u64,
    pub reset: bool,
    pub buckets: Buckets,
}

pub(super) fn aggregate_field(
    history: &mut History,
    clock: Clock,
    query: &MachineMetricsQuery,
    kind: MachineMetricKind,
    gpu_id: Option<&str>,
) -> Result<Buckets, FieldQueryError> {
    let unit = field_unit(kind, gpu_id).map_err(FieldQueryError::Cache)?;
    // Reject invalid queries before scanning or parsing retained frames.
    let empty = aggregation::aggregate(query, clock.wall, unit, [])
        .map_err(FieldQueryError::Aggregation)?;
    history.maintain(clock);
    history.ensure_live().map_err(FieldQueryError::History)?;
    let server_id = history.server_id().to_owned();
    let mut acquisitions: HashMap<(u64, u64), Observation> = HashMap::new();
    for frame in history.staged_frames(clock) {
        let stored = StoredFrame::decode(frame.bytes, &server_id, frame.clock, frame.epoch)
            .map_err(FieldQueryError::History)?;
        let Some(field) = stored
            .fields
            .into_iter()
            .find(|field| field.kind == kind && field.gpu_id.as_deref() == gpu_id)
        else {
            continue;
        };
        let observation = field.observation;
        let identity = (observation.epoch, observation.tick);
        if let Some(previous) = acquisitions.get(&identity) {
            if *previous != observation {
                return Err(FieldQueryError::ConflictingAcquisition);
            }
        } else {
            if acquisitions.len() >= MAX_RECORDS {
                return Err(FieldQueryError::Aggregation(
                    AggregationError::TooManyObservations,
                ));
            }
            acquisitions.insert(identity, observation);
        }
    }
    if acquisitions.is_empty() {
        return Ok(empty);
    }
    aggregation::aggregate(query, clock.wall, unit, acquisitions.into_values())
        .map_err(FieldQueryError::Aggregation)
}
