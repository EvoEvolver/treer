//! Bounded in-memory staging for local history; no filesystem or network I/O.
//! A future private journal adapter must enforce its own metadata/temp-file
//! budget as well. These byte counts do not claim a process RSS ceiling.
use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;
use treer_protocol::{
    MachineMetricsQuery, MachineMetricsResponse, MACHINE_METRICS_MAX_RANGE_MS,
    MACHINE_METRICS_SAMPLE_INTERVAL_MS,
};

pub const MAX_RECORDS: usize = 17_280;
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 64 * 1024;
const CLOCK_TOLERANCE_MS: i64 = 10_000;

/// Collector-owned clocks, never supplied through a public query. Monotonic
/// milliseconds are from one collector instance; a new instance starts empty.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub wall: DateTime<Utc>,
    pub monotonic_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub records: usize,
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            records: MAX_RECORDS,
            bytes: MAX_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryError {
    InvalidLimits,
    InvalidSample,
    OversizedRecord,
    DuplicateSample,
}

struct Record {
    clock: Clock,
    bytes: Vec<u8>,
}

/// Exactly one collector owns this ring. Viewers do not create or refresh it.
/// Only bounded serialized current samples are retained; historical bucket
/// responses may never be fed back as raw observations.
pub struct History {
    server_id: String,
    limits: Limits,
    records: VecDeque<Record>,
    bytes: usize,
    last_clock: Option<Clock>,
    epoch: u64,
    truncated: bool,
}

impl History {
    pub fn new(server_id: String, limits: Limits) -> Result<Self, HistoryError> {
        if server_id.is_empty()
            || server_id.len() > 128
            || limits.records == 0
            || limits.records > MAX_RECORDS
            || limits.bytes == 0
            || limits.bytes > MAX_BYTES
        {
            return Err(HistoryError::InvalidLimits);
        }
        Ok(Self {
            server_id,
            limits,
            records: VecDeque::new(),
            bytes: 0,
            last_clock: None,
            epoch: 0,
            truncated: false,
        })
    }

    fn remove_oldest(&mut self) {
        if let Some(record) = self.records.pop_front() {
            self.bytes -= record.bytes.len();
            self.truncated = true;
        }
    }

    fn reset_epoch(&mut self) {
        self.records.clear();
        self.bytes = 0;
        self.epoch = self.epoch.wrapping_add(1);
        self.truncated = true;
    }

    /// Invoke on every collector tick and before append/query, independent of
    /// viewers. It must run before serving after wake. Exact 24h age is allowed;
    /// anything older by either clock is evicted. No missed-tick backfilling.
    pub fn maintain(&mut self, clock: Clock) {
        if let Some(previous) = self.last_clock {
            let wall_delta = clock.wall.signed_duration_since(previous.wall);
            let monotonic_delta = clock.monotonic_ms.checked_sub(previous.monotonic_ms);
            let discontinuity = match monotonic_delta {
                Some(delta) => {
                    wall_delta < Duration::zero()
                        || (i128::from(wall_delta.num_milliseconds()) - i128::from(delta)).abs()
                            > i128::from(CLOCK_TOLERANCE_MS)
                }
                _ => true,
            };
            if discontinuity {
                self.reset_epoch();
            }
        }
        self.last_clock = Some(clock);
        while self.records.front().is_some_and(|record| {
            clock.wall.signed_duration_since(record.clock.wall)
                > Duration::milliseconds(MACHINE_METRICS_MAX_RANGE_MS)
                || clock.monotonic_ms.saturating_sub(record.clock.monotonic_ms)
                    > MACHINE_METRICS_MAX_RANGE_MS as u64
        }) {
            self.remove_oldest();
        }
    }

    pub fn push(
        &mut self,
        clock: Clock,
        sample: &MachineMetricsResponse,
    ) -> Result<(), HistoryError> {
        self.maintain(clock);
        // The caller must project an actual collected frame, not aggregate it
        // from a chart query or timestamp it when a viewer happens to ask.
        let query = MachineMetricsQuery {
            from: clock
                .wall
                .checked_sub_signed(Duration::milliseconds(
                    MACHINE_METRICS_SAMPLE_INTERVAL_MS as i64,
                ))
                .ok_or(HistoryError::InvalidSample)?,
            to: clock.wall,
            max_points: 1,
        };
        if sample.generated_at != clock.wall
            || sample.series.iter().any(|series| !series.points.is_empty())
            || sample
                .validate(&query, &self.server_id, clock.wall)
                .is_err()
        {
            return Err(HistoryError::InvalidSample);
        }
        if self.records.back().is_some_and(|record| {
            clock.monotonic_ms <= record.clock.monotonic_ms || clock.wall <= record.clock.wall
        }) {
            return Err(HistoryError::DuplicateSample);
        }
        let bytes = serde_json::to_vec(sample).map_err(|_| HistoryError::InvalidSample)?;
        if bytes.len() > MAX_RECORD_BYTES || bytes.len() > self.limits.bytes {
            return Err(HistoryError::OversizedRecord);
        }
        while self.records.len() >= self.limits.records
            || self.bytes + bytes.len() > self.limits.bytes
        {
            self.remove_oldest();
        }
        self.bytes += bytes.len();
        self.records.push_back(Record { clock, bytes });
        Ok(())
    }

    /// Borrowed raw staging frames, not a public query response. Every read
    /// reaps first; the future query adapter must parse and aggregate locally.
    pub fn frames(&mut self, clock: Clock) -> impl Iterator<Item = &[u8]> {
        self.maintain(clock);
        self.records.iter().map(|record| record.bytes.as_slice())
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn serialized_bytes(&self) -> usize {
        self.bytes
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod tests;
