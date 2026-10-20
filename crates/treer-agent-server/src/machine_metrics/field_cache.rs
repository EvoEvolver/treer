//! Collector-owned field state, not a sampling task or public query adapter.
use super::{aggregation::Observation, history::Clock};
use std::collections::HashMap;
use treer_protocol::{
    MachineMetricCurrent, MachineMetricKind, MachineMetricStatus, MachineMetricUnit,
    MACHINE_METRICS_MAX_RANGE_MS, MACHINE_METRICS_MAX_SERIES, MACHINE_METRICS_STALE_AFTER_MS,
};

#[derive(Debug, PartialEq, Eq)]
pub enum CacheError {
    InvalidField,
    InvalidObservation,
    ClockReversed,
    Capacity,
    SequenceExhausted,
    InvalidEpoch,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    kind: MachineMetricKind,
    gpu_id: Option<String>,
}

#[derive(Clone)]
struct Entry {
    clock: Clock,
    observation: Observation,
}

pub struct FieldSnapshot {
    pub kind: MachineMetricKind,
    pub gpu_id: Option<String>,
    /// Original acquisition for history, even when current has become stale.
    pub observation: Observation,
    pub current: MachineMetricCurrent,
}

#[derive(Clone, Copy)]
pub(super) struct FieldUpdate<'a> {
    pub kind: MachineMetricKind,
    pub gpu_id: Option<&'a str>,
    pub status: MachineMetricStatus,
    pub value: Option<f64>,
}

/// One instance per collector. At most 100 fields; tick identifies real capture
/// completions, not reads or outer-frame serialization. No implicit eviction.
#[derive(Clone)]
pub struct FieldCache {
    epoch: u64,
    next_tick: u64,
    fields: HashMap<Key, Entry>,
}

impl FieldCache {
    /// Bounded transaction: all fields receive the same completion clock, but
    /// distinct acquisition ticks. Any failure preserves the original cache
    /// and sequence, including failure on the final field.
    pub(super) fn record_batch(
        &mut self,
        clock: Clock,
        updates: &[FieldUpdate<'_>],
    ) -> Result<(), CacheError> {
        if updates.is_empty() || updates.len() > MACHINE_METRICS_MAX_SERIES {
            return Err(CacheError::InvalidField);
        }
        let mut keys = std::collections::HashSet::new();
        for field in updates {
            if field.gpu_id.is_some_and(|id| id.len() != 64)
                || !keys.insert((field.kind, field.gpu_id))
            {
                return Err(CacheError::InvalidField);
            }
        }
        let mut staged = self.clone(); // At most 100 bounded keys/entries.
        for field in updates {
            staged.record(clock, field.kind, field.gpu_id, field.status, field.value)?;
        }
        *self = staged;
        Ok(())
    }

    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            next_tick: 0,
            fields: HashMap::new(),
        }
    }

    /// Called by the future collector after detecting clock/epoch uncertainty.
    /// Keep tick increasing; a new epoch cannot restore old cached values.
    pub fn start_epoch(&mut self, epoch: u64) -> Result<(), CacheError> {
        if epoch <= self.epoch {
            return Err(CacheError::InvalidEpoch);
        }
        self.fields.clear();
        self.epoch = epoch;
        Ok(())
    }

    /// Call only for an actual completed acquisition/attempt, never to refresh
    /// the timestamp of a cached value. The clock is collector-owned.
    pub fn record(
        &mut self,
        clock: Clock,
        kind: MachineMetricKind,
        gpu_id: Option<&str>,
        status: MachineMetricStatus,
        value: Option<f64>,
    ) -> Result<(), CacheError> {
        let gpu = matches!(
            kind,
            MachineMetricKind::GpuUtilization
                | MachineMetricKind::GpuMemoryUsed
                | MachineMetricKind::GpuMemoryTotal
        );
        if gpu != gpu_id.is_some()
            || gpu_id.is_some_and(|id| {
                id.len() != 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(CacheError::InvalidField);
        }
        let unit = if matches!(
            kind,
            MachineMetricKind::CpuUtilization | MachineMetricKind::GpuUtilization
        ) {
            MachineMetricUnit::Percent
        } else {
            MachineMetricUnit::Bytes
        };
        // Stale is a read-time projection, never a fresh acquisition.
        let valid = match (status, value) {
            (MachineMetricStatus::Ok, Some(v)) => {
                v.is_finite()
                    && v >= 0.0
                    && match unit {
                        MachineMetricUnit::Percent => v <= 100.0,
                        MachineMetricUnit::Bytes => {
                            v <= 9_007_199_254_740_991.0 && v.fract() == 0.0
                        }
                    }
            }
            (s, None) => s != MachineMetricStatus::Ok && s != MachineMetricStatus::Stale,
            _ => false,
        };
        if !valid {
            return Err(CacheError::InvalidObservation);
        }
        let key = Key {
            kind,
            gpu_id: gpu_id.map(str::to_owned),
        };
        if let Some(old) = self.fields.get(&key) {
            if clock.wall <= old.clock.wall || clock.monotonic_ms <= old.clock.monotonic_ms {
                return Err(CacheError::ClockReversed);
            }
        } else if self.fields.len() >= MACHINE_METRICS_MAX_SERIES {
            return Err(CacheError::Capacity);
        }
        let tick = self.next_tick;
        let next = tick.checked_add(1).ok_or(CacheError::SequenceExhausted)?;
        self.fields.insert(
            key,
            Entry {
                clock,
                observation: Observation {
                    epoch: self.epoch,
                    tick,
                    at: clock.wall,
                    status,
                    value,
                },
            },
        );
        self.next_tick = next;
        Ok(())
    }

    /// Pure read: compute age from the field's own acquisition clock, retaining
    /// its original identity and historical value. No returned stale value may
    /// be passed back through record as a new acquisition.
    pub fn snapshot(&self, now: Clock) -> Result<Vec<FieldSnapshot>, CacheError> {
        let mut fields = Vec::with_capacity(self.fields.len());
        for (key, entry) in &self.fields {
            if now.wall < entry.clock.wall || now.monotonic_ms < entry.clock.monotonic_ms {
                return Err(CacheError::ClockReversed);
            }
            let age = now
                .wall
                .signed_duration_since(entry.clock.wall)
                .num_milliseconds() as u64;
            // Large wall/monotonic disagreement requires the caller to reset
            // epoch. The wire age itself is specifically the wall-time delta.
            let monotonic_age = now.monotonic_ms - entry.clock.monotonic_ms;
            if age.abs_diff(monotonic_age) > 10_000 {
                return Err(CacheError::ClockReversed);
            }
            let stale = entry.observation.status == MachineMetricStatus::Ok
                && age > MACHINE_METRICS_STALE_AFTER_MS;
            fields.push(FieldSnapshot {
                kind: key.kind,
                gpu_id: key.gpu_id.clone(),
                observation: entry.observation,
                current: MachineMetricCurrent {
                    status: if stale {
                        MachineMetricStatus::Stale
                    } else {
                        entry.observation.status
                    },
                    value: if stale { None } else { entry.observation.value },
                    sampled_at: Some(entry.clock.wall),
                    age_ms: Some(age),
                },
            });
        }
        fields.sort_by(|a, b| (a.kind as u8, &a.gpu_id).cmp(&(b.kind as u8, &b.gpu_id)));
        Ok(fields)
    }

    /// Collector maintenance only. Validate ALL clocks before deleting any
    /// field, so expiry cannot conceal clock uncertainty or partly mutate on
    /// failure. Retirement neither creates observations nor resets their tick.
    pub(super) fn retire_expired(&mut self, now: Clock) -> Result<usize, CacheError> {
        self.snapshot(now)?;
        let before = self.fields.len();
        self.fields.retain(|_, entry| {
            now.wall.signed_duration_since(entry.clock.wall)
                <= chrono::Duration::milliseconds(MACHINE_METRICS_MAX_RANGE_MS)
                && now.monotonic_ms - entry.clock.monotonic_ms
                    <= MACHINE_METRICS_MAX_RANGE_MS as u64
        });
        Ok(before - self.fields.len())
    }
}

#[cfg(test)]
mod tests;
