//! Bounded staging readers. Not the public response or runtime query route.
use super::{
    aggregation::{self, AggregationError, Buckets, Observation},
    field_cache::{field_unit, CacheError, FieldSnapshot},
    history::{Clock, History, HistoryError, HistorySnapshot, MAX_RECORDS},
    stored_frame::StoredFrame,
};
use std::collections::HashMap;
use treer_protocol::{MachineMetricKind, MachineMetricsQuery};

mod multi;
mod system_response;
pub use multi::{FieldKey, MultiFieldHistory, MAX_INDEX_ACQUISITIONS};
pub use system_response::{SystemMetadata, SystemResponse, SystemResponseError};

#[derive(Debug, PartialEq, Eq)]
pub enum FieldQueryError {
    History(HistoryError),
    Cache(CacheError),
    Aggregation(AggregationError),
    ConflictingAcquisition,
    InvalidFields,
    IndexLimit,
    InventoryLimit,
}

pub struct FieldHistory {
    pub epoch: u64,
    pub reset: bool,
    pub buckets: Buckets,
}

/// Not Clone; owns the single snapshot admission until dropped. A future
/// worker must release collector access before calling query_field.
pub struct DetachedFieldQuery {
    pub(super) history: HistorySnapshot,
    pub(super) clock: Clock,
    pub(super) reset: bool,
    pub(super) current: Vec<FieldSnapshot>,
}

impl DetachedFieldQuery {
    pub fn query_field(
        &mut self,
        query: &MachineMetricsQuery,
        kind: MachineMetricKind,
        gpu_id: Option<&str>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<FieldHistory, FieldQueryError> {
        let buckets = aggregate_frames(
            self.history.server_id(),
            self.history
                .frames()
                .map(|(clock, bytes)| (clock, self.history.epoch(), bytes)),
            self.clock,
            query,
            kind,
            gpu_id,
            cancelled,
        )?;
        Ok(FieldHistory {
            epoch: self.history.epoch(),
            reset: self.reset,
            buckets,
        })
    }
}

pub(super) fn aggregate_field(
    history: &mut History,
    clock: Clock,
    query: &MachineMetricsQuery,
    kind: MachineMetricKind,
    gpu_id: Option<&str>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Buckets, FieldQueryError> {
    history.maintain(clock);
    history.ensure_live().map_err(FieldQueryError::History)?;
    let server_id = history.server_id().to_owned();
    aggregate_frames(
        &server_id,
        history
            .staged_frames(clock)
            .map(|frame| (frame.clock, frame.epoch, frame.bytes)),
        clock,
        query,
        kind,
        gpu_id,
        cancelled,
    )
}

fn aggregate_frames<'a>(
    server_id: &str,
    frames: impl Iterator<Item = (Clock, u64, &'a [u8])>,
    clock: Clock,
    query: &MachineMetricsQuery,
    kind: MachineMetricKind,
    gpu_id: Option<&str>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Buckets, FieldQueryError> {
    let unit = field_unit(kind, gpu_id).map_err(FieldQueryError::Cache)?;
    // Reject invalid queries before scanning or parsing retained frames.
    let empty = aggregation::aggregate_cancellable(query, clock.wall, unit, [], cancelled)
        .map_err(FieldQueryError::Aggregation)?;
    let mut acquisitions: HashMap<(u64, u64), Observation> = HashMap::new();
    for (frame_clock, epoch, bytes) in frames {
        aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)?;
        let stored = StoredFrame::decode(bytes, server_id, frame_clock, epoch)
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
        aggregation::check_cancelled(cancelled).map_err(FieldQueryError::Aggregation)?;
        return Ok(empty);
    }
    aggregation::aggregate_cancellable(
        query,
        clock.wall,
        unit,
        acquisitions.into_values(),
        cancelled,
    )
    .map_err(FieldQueryError::Aggregation)
}
