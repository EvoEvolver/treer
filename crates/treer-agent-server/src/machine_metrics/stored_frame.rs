//! Bounded local staging format, not a public response or durable journal.
use super::{
    aggregation::Observation,
    field_cache::{FieldCache, FieldSnapshot},
    history::{Clock, HistoryError, MAX_RECORD_BYTES},
};
use chrono::Duration;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use treer_protocol::{
    MachineMetricCurrent, MachineMetricKind, MachineMetricStatus, MachineMetricsQuery,
    MachineMetricsResponse, MACHINE_METRICS_MAX_RANGE_MS, MACHINE_METRICS_MAX_SERIES,
    MACHINE_METRICS_STALE_AFTER_MS,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredField {
    pub kind: MachineMetricKind,
    pub gpu_id: Option<String>,
    pub observation: Observation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFrame {
    version: u8,
    pub epoch: u64,
    pub frame: MachineMetricsResponse,
    pub fields: Vec<StoredField>,
}

impl StoredFrame {
    pub(super) fn encode(
        frame: &MachineMetricsResponse,
        fields: &[FieldSnapshot],
        epoch: u64,
        server_id: &str,
        clock: Clock,
    ) -> Result<Vec<u8>, HistoryError> {
        // Refuse oversized model shapes before cloning caller-owned data.
        let query = MachineMetricsQuery {
            from: clock
                .wall
                .checked_sub_signed(Duration::seconds(5))
                .ok_or(HistoryError::InvalidSample)?,
            to: clock.wall,
            max_points: 1,
        };
        if fields.len() > MACHINE_METRICS_MAX_SERIES
            || fields
                .iter()
                .any(|field| field.gpu_id.as_ref().is_some_and(|id| id.len() > 64))
            || frame.series.iter().any(|series| !series.points.is_empty())
            || frame.validate(&query, server_id, clock.wall).is_err()
        {
            return Err(HistoryError::InvalidSample);
        }
        let stored = Self {
            version: 1,
            epoch,
            frame: frame.clone(),
            fields: fields
                .iter()
                .map(|field| StoredField {
                    kind: field.kind,
                    gpu_id: field.gpu_id.clone(),
                    observation: field.observation,
                })
                .collect(),
        };
        stored.validate(server_id, clock, epoch)?;
        let bytes = serde_json::to_vec(&stored).map_err(|_| HistoryError::InvalidSample)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(HistoryError::OversizedRecord);
        }
        Ok(bytes)
    }

    /// Bound before parsing; revalidate the typed frame and original field
    /// identity/value links. Call only for the matching collector epoch.
    pub fn decode(
        bytes: &[u8],
        server_id: &str,
        clock: Clock,
        epoch: u64,
    ) -> Result<Self, HistoryError> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(HistoryError::OversizedRecord);
        }
        let stored: Self =
            serde_json::from_slice(bytes).map_err(|_| HistoryError::InvalidSample)?;
        stored.validate(server_id, clock, epoch)?;
        Ok(stored)
    }

    fn validate(&self, server_id: &str, clock: Clock, epoch: u64) -> Result<(), HistoryError> {
        let invalid = || HistoryError::InvalidSample;
        let query = MachineMetricsQuery {
            from: clock
                .wall
                .checked_sub_signed(Duration::seconds(5))
                .ok_or_else(invalid)?,
            to: clock.wall,
            max_points: 1,
        };
        if self.version != 1
            || self.epoch != epoch
            || self.frame.generated_at != clock.wall
            || self
                .frame
                .series
                .iter()
                .any(|series| !series.points.is_empty())
            || self.fields.len() > MACHINE_METRICS_MAX_SERIES
            || self.frame.validate(&query, server_id, clock.wall).is_err()
        {
            return Err(invalid());
        }
        let mut keys = HashSet::new();
        let mut ticks = HashSet::new();
        // Reuse acquisition value/key validation without allocating replacement
        // identities in the stored frame. This temporary cache is not retained.
        let mut validation = FieldCache::new(epoch);
        for field in &self.fields {
            let observation = field.observation;
            let age = clock.wall.signed_duration_since(observation.at);
            if observation.epoch != epoch
                || observation.tick == u64::MAX
                || age < Duration::zero()
                || age > Duration::milliseconds(MACHINE_METRICS_MAX_RANGE_MS)
                || !keys.insert((field.kind, field.gpu_id.as_deref()))
                || !ticks.insert(observation.tick)
            {
                return Err(invalid());
            }
            validation
                .record(
                    Clock {
                        wall: observation.at,
                        monotonic_ms: 0,
                    },
                    field.kind,
                    field.gpu_id.as_deref(),
                    observation.status,
                    observation.value,
                )
                .map_err(|_| invalid())?;
            let series = self
                .frame
                .series
                .iter()
                .find(|series| series.kind == field.kind && series.gpu_id == field.gpu_id)
                .ok_or_else(invalid)?;
            let stale = observation.status == MachineMetricStatus::Ok
                && age.num_milliseconds() as u64 > MACHINE_METRICS_STALE_AFTER_MS;
            let current = MachineMetricCurrent {
                status: if stale {
                    MachineMetricStatus::Stale
                } else {
                    observation.status
                },
                value: if stale { None } else { observation.value },
                sampled_at: Some(observation.at),
                age_ms: Some(age.num_milliseconds() as u64),
            };
            if series.current != current {
                return Err(invalid());
            }
        }
        for series in &self.frame.series {
            if !keys.contains(&(series.kind, series.gpu_id.as_deref()))
                && series.current
                    != (MachineMetricCurrent {
                        status: MachineMetricStatus::WarmingUp,
                        value: None,
                        sampled_at: None,
                        age_ms: None,
                    })
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
