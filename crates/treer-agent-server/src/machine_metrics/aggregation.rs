//! Bounded, source-local bucket arithmetic. No collection, persistence or routes.
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};
use treer_protocol::{
    MachineMetricBucket, MachineMetricStatus, MachineMetricStatusCount, MachineMetricUnit,
    MachineMetricsQuery, MACHINE_METRICS_SAMPLE_INTERVAL_MS,
};

/// One actual field acquisition, not a cached value repeated on every tick.
/// The adapter must deduplicate by acquisition before calling this function.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// Collector-owned acquisition identity, independent of caller query bounds.
    pub epoch: u64,
    pub tick: u64,
    pub at: DateTime<Utc>,
    pub status: MachineMetricStatus,
    pub value: Option<f64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AggregationError {
    InvalidQuery,
    InvalidObservation,
    TooManyObservations,
    DuplicateAcquisition,
}

pub struct Buckets {
    pub width_ms: u64,
    pub points: Vec<MachineMetricBucket>,
}

/// Caller must reduce max_points further for the response's total-series budget.
/// At most 17,280 input observations are visited, including out-of-range ones.
/// True duplicate acquisition identities are refused. Jittered acquisitions in
/// one 5 s slot coalesce to the latest observation (including a later failure).
/// Missing slots stay missing and break continuity.
pub fn aggregate(
    query: &MachineMetricsQuery,
    now: DateTime<Utc>,
    unit: MachineMetricUnit,
    observations: impl IntoIterator<Item = Observation>,
) -> Result<Buckets, AggregationError> {
    let span = query.to.signed_duration_since(query.from);
    let span_ms = span.num_milliseconds();
    if !query.valid_at(now) || span_ms < 1 || span != Duration::milliseconds(span_ms) {
        return Err(AggregationError::InvalidQuery);
    }
    let interval = MACHINE_METRICS_SAMPLE_INTERVAL_MS;
    let width_ms = (span_ms as u64)
        .div_ceil(u64::from(query.max_points))
        .div_ceil(interval)
        * interval;
    let mut points = Vec::new();
    let mut start = query.from;
    while start < query.to {
        let end = start
            .checked_add_signed(Duration::milliseconds(width_ms as i64))
            .unwrap_or(query.to)
            .min(query.to);
        let expected = end.signed_duration_since(start).num_milliseconds();
        // The current wire validator requires a positive whole-ms bucket span.
        if expected < 1 {
            return Err(AggregationError::InvalidQuery);
        }
        points.push(MachineMetricBucket {
            from: start,
            to: end,
            expected_count: (expected as u64).div_ceil(interval) as u32,
            valid_count: 0,
            status_counts: Vec::new(),
            first_sample_at: None,
            last_sample_at: None,
            mean: None,
            min: None,
            max: None,
            partial: true,
        });
        start = end;
    }
    let mut identities = HashSet::new();
    let mut slots: HashMap<u64, Observation> = HashMap::new();
    let mut statuses: Vec<HashMap<MachineMetricStatus, u32>> =
        (0..points.len()).map(|_| HashMap::new()).collect();
    for (count, observation) in observations.into_iter().enumerate() {
        if count >= super::history::MAX_RECORDS {
            return Err(AggregationError::TooManyObservations);
        }
        if observation.at < query.from || observation.at >= query.to {
            continue;
        }
        let offset = observation
            .at
            .signed_duration_since(query.from)
            .num_milliseconds() as u64;
        let slot = offset / interval;
        if !identities.insert((observation.epoch, observation.tick)) {
            return Err(AggregationError::DuplicateAcquisition);
        }
        // Validate even an older observation that will be coalesced away.
        let valid = match (observation.status, observation.value) {
            (MachineMetricStatus::Ok, Some(value)) => {
                value.is_finite()
                    && value >= 0.0
                    && match unit {
                        MachineMetricUnit::Percent => value <= 100.0,
                        MachineMetricUnit::Bytes => {
                            value <= 9_007_199_254_740_991.0 && value.fract() == 0.0
                        }
                    }
            }
            (status, None) => status != MachineMetricStatus::Ok,
            _ => false,
        };
        if !valid {
            return Err(AggregationError::InvalidObservation);
        }
        let key = |v: &Observation| (v.at, v.epoch, v.tick);
        slots
            .entry(slot)
            .and_modify(|old| {
                if key(&observation) > key(old) {
                    *old = observation;
                }
            })
            .or_insert(observation);
    }
    let mut selected: Vec<_> = slots.into_values().collect();
    selected.sort_by_key(|v| (v.at, v.epoch, v.tick));
    for observation in selected {
        let offset = observation
            .at
            .signed_duration_since(query.from)
            .num_milliseconds() as u64;
        let index = (offset / width_ms) as usize;
        let bucket = &mut points[index];
        match (observation.status, observation.value) {
            (MachineMetricStatus::Ok, Some(value))
                if value.is_finite()
                    && value >= 0.0
                    && match unit {
                        MachineMetricUnit::Percent => value <= 100.0,
                        MachineMetricUnit::Bytes => {
                            value <= 9_007_199_254_740_991.0 && value.fract() == 0.0
                        }
                    } =>
            {
                bucket.valid_count += 1;
                bucket.mean = Some(bucket.mean.map_or(value, |mean| {
                    mean + (value - mean) / f64::from(bucket.valid_count)
                }));
                bucket.min = Some(bucket.min.map_or(value, |min| min.min(value)));
                bucket.max = Some(bucket.max.map_or(value, |max| max.max(value)));
                bucket.first_sample_at = Some(
                    bucket
                        .first_sample_at
                        .map_or(observation.at, |at| at.min(observation.at)),
                );
                bucket.last_sample_at = Some(
                    bucket
                        .last_sample_at
                        .map_or(observation.at, |at| at.max(observation.at)),
                );
            }
            (status, None) if status != MachineMetricStatus::Ok => {}
            _ => return Err(AggregationError::InvalidObservation),
        }
        *statuses[index].entry(observation.status).or_default() += 1;
    }
    for (bucket, counts) in points.iter_mut().zip(statuses) {
        bucket.status_counts = counts
            .into_iter()
            .map(|(status, count)| MachineMetricStatusCount { status, count })
            .collect();
        // Stable output independent of hash randomization/input ordering.
        bucket.status_counts.sort_by_key(|entry| entry.status as u8);
        bucket.partial = bucket.valid_count != bucket.expected_count;
    }
    Ok(Buckets { width_ms, points })
}

#[cfg(test)]
mod tests;
