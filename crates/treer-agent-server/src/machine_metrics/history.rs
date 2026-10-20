//! Bounded in-memory staging for local history; no filesystem or network I/O.
//! A future private journal adapter must enforce its own metadata/temp-file
//! budget as well. These byte counts do not claim a process RSS ceiling.
use super::{field_cache::FieldSnapshot, stored_frame::StoredFrame};
use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;
use treer_protocol::{MachineMetricsResponse, MACHINE_METRICS_MAX_RANGE_MS};

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
    SequenceExhausted,
}

struct Record {
    tick: u64,
    clock: Clock,
    oldest_acquisition: DateTime<Utc>,
    bytes: Vec<u8>,
}

/// Borrowed collector frame identity. Scoped to one History instance, not a
/// durable boot identifier or proof that every cached field was reacquired.
pub struct StagedFrame<'a> {
    pub epoch: u64,
    pub tick: u64,
    pub clock: Clock,
    pub bytes: &'a [u8],
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
    next_tick: u64,
    exhausted: bool,
    truncated: bool,
}

impl History {
    pub(super) fn server_id(&self) -> &str {
        &self.server_id
    }

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
            next_tick: 0,
            exhausted: false,
            truncated: false,
        })
    }

    fn remove_oldest(&mut self) {
        if let Some(record) = self.records.pop_front() {
            self.bytes -= record.bytes.len();
        }
    }

    fn reset_epoch(&mut self) {
        self.records.clear();
        self.bytes = 0;
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => self.exhausted = true,
        }
        self.truncated = true;
    }

    pub(super) fn invalidate(&mut self, clock: Clock) {
        self.reset_epoch();
        self.last_clock = Some(clock);
    }

    pub(super) fn ensure_live(&self) -> Result<(), HistoryError> {
        if self.exhausted {
            Err(HistoryError::SequenceExhausted)
        } else {
            Ok(())
        }
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
        // Field age can differ from frame age. Do not assume that the oldest
        // acquisition is ordered across frames when inventory changes.
        let mut expired_bytes = 0;
        self.records.retain(|record| {
            let expired = clock.wall.signed_duration_since(record.oldest_acquisition)
                > Duration::milliseconds(MACHINE_METRICS_MAX_RANGE_MS)
                || clock.monotonic_ms.saturating_sub(record.clock.monotonic_ms)
                    > MACHINE_METRICS_MAX_RANGE_MS as u64;
            if expired {
                expired_bytes += record.bytes.len();
            }
            !expired
        });
        self.bytes -= expired_bytes;
    }

    #[cfg(test)]
    pub fn push(
        &mut self,
        clock: Clock,
        sample: &MachineMetricsResponse,
    ) -> Result<(), HistoryError> {
        self.push_with_fields(clock, sample, &[])
    }

    pub(super) fn push_with_fields(
        &mut self,
        clock: Clock,
        sample: &MachineMetricsResponse,
        fields: &[FieldSnapshot],
    ) -> Result<(), HistoryError> {
        self.maintain(clock);
        self.ensure_live()?;
        // The caller must project an actual collected frame, not aggregate it
        // from a chart query or timestamp it when a viewer happens to ask.
        let bytes = StoredFrame::encode(sample, fields, self.epoch, &self.server_id, clock)?;
        if self.records.back().is_some_and(|record| {
            clock.monotonic_ms <= record.clock.monotonic_ms || clock.wall <= record.clock.wall
        }) {
            return Err(HistoryError::DuplicateSample);
        }
        if bytes.len() > MAX_RECORD_BYTES || bytes.len() > self.limits.bytes {
            return Err(HistoryError::OversizedRecord);
        }
        let tick = self.next_tick;
        let next_tick = tick.checked_add(1).ok_or(HistoryError::SequenceExhausted)?;
        while self.records.len() >= self.limits.records
            || self.bytes + bytes.len() > self.limits.bytes
        {
            self.remove_oldest();
            self.truncated = true;
        }
        self.bytes += bytes.len();
        let oldest_acquisition = fields
            .iter()
            .map(|field| field.observation.at)
            .min()
            .unwrap_or(clock.wall);
        self.records.push_back(Record {
            tick,
            clock,
            oldest_acquisition,
            bytes,
        });
        self.next_tick = next_tick;
        Ok(())
    }

    /// Borrowed raw staging frames, not a public query response. Every read
    /// reaps first; the future query adapter must parse and aggregate locally.
    pub fn frames(&mut self, clock: Clock) -> impl Iterator<Item = &[u8]> {
        self.staged_frames(clock).map(|frame| frame.bytes)
    }

    /// Identities are assigned at successful append, never by a reader. Tick
    /// increases across eviction and epoch resets and refuses overflow. Within
    /// an epoch, admission also enforces increasing wall and monotonic times.
    /// Bytes contain StoredFrame with ORIGINAL field acquisition identities;
    /// adapters must not relabel them with the outer frame tick.
    pub fn staged_frames(&mut self, clock: Clock) -> impl Iterator<Item = StagedFrame<'_>> {
        self.maintain(clock);
        let epoch = self.epoch;
        self.records.iter().map(move |record| StagedFrame {
            epoch,
            tick: record.tick,
            clock: record.clock,
            bytes: record.bytes.as_slice(),
        })
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

    /// Capacity loss or clock uncertainty in this collector instance. Normal
    /// expiration at the retention boundary does not set or clear this flag.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod tests;
