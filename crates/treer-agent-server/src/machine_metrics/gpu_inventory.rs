//! Inert completed-enumeration cache. No helper execution or query input.
use super::history::Clock;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use treer_protocol::{MachineMetricsGpu, MACHINE_METRICS_MAX_GPUS, MACHINE_METRICS_STALE_AFTER_MS};

#[derive(Debug, PartialEq, Eq)]
pub enum InventoryError {
    InvalidCapture,
    DuplicateCapture,
    ClockReversed,
}

/// Only a fresh successful enumeration can establish the current inventory.
/// An empty successful enumeration differs from failure/unknown (None).
#[derive(Clone)]
pub struct InventorySnapshot {
    pub sampled_at: DateTime<Utc>,
    pub gpus: Vec<MachineMetricsGpu>,
}

#[derive(Default)]
pub(super) struct InventoryCache {
    clock: Option<Clock>,
    capture: Option<Vec<MachineMetricsGpu>>,
}

impl InventoryCache {
    /// Some = complete successful enumeration; None = a failed attempt.
    /// No truncation policy is implied: oversized inventories refuse in full.
    pub(super) fn record(
        &mut self,
        clock: Clock,
        gpus: Option<&[MachineMetricsGpu]>,
    ) -> Result<(), InventoryError> {
        if let Some(gpus) = gpus {
            if gpus.len() > MACHINE_METRICS_MAX_GPUS {
                return Err(InventoryError::InvalidCapture);
            }
            let mut ids = HashSet::new();
            for gpu in gpus {
                if !gpu.present
                    || gpu.validate_descriptor().is_err()
                    || !ids.insert(gpu.id.as_str())
                {
                    return Err(InventoryError::InvalidCapture);
                }
            }
        }
        if self
            .clock
            .is_some_and(|old| clock.wall < old.wall || clock.monotonic_ms < old.monotonic_ms)
        {
            return Err(InventoryError::ClockReversed);
        }
        if self
            .clock
            .is_some_and(|old| clock.wall == old.wall || clock.monotonic_ms == old.monotonic_ms)
        {
            // A duplicate or low-resolution tie establishes no new capture,
            // but does not imply clock uncertainty or discard other history.
            return Err(InventoryError::DuplicateCapture);
        }
        let mut capture = gpus.map(<[_]>::to_vec);
        if let Some(gpus) = &mut capture {
            gpus.sort_by(|a, b| a.id.cmp(&b.id));
        }
        self.clock = Some(clock);
        self.capture = capture;
        Ok(())
    }

    pub(super) fn snapshot(&self, now: Clock) -> Result<Option<InventorySnapshot>, InventoryError> {
        let Some(clock) = self.clock else {
            return Ok(None);
        };
        if now.wall < clock.wall || now.monotonic_ms < clock.monotonic_ms {
            return Err(InventoryError::ClockReversed);
        }
        let age = now
            .wall
            .signed_duration_since(clock.wall)
            .num_milliseconds() as u64;
        let monotonic_age = now.monotonic_ms - clock.monotonic_ms;
        if age.abs_diff(monotonic_age) > 10_000 {
            return Err(InventoryError::ClockReversed);
        }
        // Stale or failed enumeration is unknown, NEVER an empty success.
        if age > MACHINE_METRICS_STALE_AFTER_MS || monotonic_age > MACHINE_METRICS_STALE_AFTER_MS {
            return Ok(None);
        }
        Ok(self.capture.as_ref().map(|gpus| InventorySnapshot {
            sampled_at: clock.wall,
            gpus: gpus.clone(),
        }))
    }
}
