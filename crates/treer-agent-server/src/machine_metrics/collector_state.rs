//! Inert collector state boundary. Owns both clocks' invalidation domain;
//! it does not collect hardware samples or expose a query route.
use super::{
    field_cache::{CacheError, FieldCache, FieldSnapshot, FieldUpdate},
    field_query::{self, FieldHistory, FieldQueryError},
    history::{Clock, History, HistoryError, Limits},
};
use treer_protocol::{
    MachineMetricKind, MachineMetricStatus, MachineMetricsQuery, MachineMetricsResponse,
};

#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    History(HistoryError),
    Cache(CacheError),
    FrameMismatch,
    Query(FieldQueryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetCause {
    HistoryClock,
    FieldClock,
    AcquisitionClock,
}

/// Fixed, content-free diagnostics for a future collector health surface.
/// Scoped to this state instance, not persisted or advertised over an API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetDiagnostics {
    pub count: u64,
    pub last_cause: Option<ResetCause>,
}

pub struct Snapshot {
    pub epoch: u64,
    pub reset: bool,
    /// Empty after uncertainty; an assembler must use warming_up/no observation.
    pub fields: Vec<FieldSnapshot>,
}

/// History owns the authoritative epoch. Neither component is exposed for
/// independent mutation, so a reset cannot leave the other side's data live.
pub struct CollectorState {
    history: History,
    cache: FieldCache,
    epoch: u64,
    last_reset_cause: Option<ResetCause>,
    expired_fields: u64,
}

impl CollectorState {
    /// Reads cached acquisitions only; never performs native acquisition.
    /// This single-field adapter is not a multi-series/public response.
    pub fn query_field(
        &mut self,
        clock: Clock,
        query: &MachineMetricsQuery,
        kind: MachineMetricKind,
        gpu_id: Option<&str>,
    ) -> Result<FieldHistory, StateError> {
        let snapshot = self.snapshot(clock)?;
        let buckets = field_query::aggregate_field(&mut self.history, clock, query, kind, gpu_id)
            .map_err(StateError::Query)?;
        Ok(FieldHistory {
            epoch: snapshot.epoch,
            reset: snapshot.reset,
            buckets,
        })
    }

    pub fn new(server_id: String, limits: Limits) -> Result<Self, StateError> {
        Ok(Self {
            history: History::new(server_id, limits).map_err(StateError::History)?,
            cache: FieldCache::new(0),
            epoch: 0,
            last_reset_cause: None,
            expired_fields: 0,
        })
    }

    fn synchronize(&mut self, clock: Clock) -> Result<bool, StateError> {
        self.history.maintain(clock);
        self.history.ensure_live().map_err(StateError::History)?;
        if self.epoch == self.history.epoch() {
            return Ok(false);
        }
        self.cache
            .start_epoch(self.history.epoch())
            .map_err(StateError::Cache)?;
        self.epoch = self.history.epoch();
        self.last_reset_cause = Some(ResetCause::HistoryClock);
        Ok(true)
    }

    fn invalidate(&mut self, clock: Clock, cause: ResetCause) -> Result<(), StateError> {
        self.history.invalidate(clock);
        self.synchronize(clock)?;
        self.last_reset_cause = Some(cause);
        Ok(())
    }

    /// Reading diagnostics does not maintain clocks or create observations.
    /// The authoritative epoch already counts successful resets without wrap.
    pub fn reset_diagnostics(&self) -> ResetDiagnostics {
        ResetDiagnostics {
            count: self.epoch,
            last_cause: self.last_reset_cause,
        }
    }

    /// Instance-local, saturating count of expired fields retired by maintenance.
    /// Contains no keys, samples or clock values; reading it has no side effects.
    pub fn expired_field_count(&self) -> u64 {
        self.expired_fields
    }

    /// Also the maintenance entry point for ticks with no completed capture.
    /// Cache-only accumulated clock drift invalidates history as well.
    pub fn snapshot(&mut self, clock: Clock) -> Result<Snapshot, StateError> {
        let mut reset = self.synchronize(clock)?;
        let retired = match self.cache.retire_expired(clock) {
            Ok(retired) => retired,
            Err(CacheError::ClockReversed) => {
                self.invalidate(clock, ResetCause::FieldClock)?;
                reset = true;
                0
            }
            Err(error) => return Err(StateError::Cache(error)),
        };
        self.expired_fields = self.expired_fields.saturating_add(retired as u64);
        let fields = self.cache.snapshot(clock).map_err(StateError::Cache)?;
        Ok(Snapshot {
            epoch: self.epoch,
            reset,
            fields,
        })
    }

    /// Only real completed acquisitions/attempts may enter here. A conflicting
    /// acquisition clock clears both sides before accepting that new capture.
    /// Stamp completion with the collector's own clock, never a helper clock or
    /// the acquisition's earlier dispatch time. Repeated resets are diagnosable.
    pub fn record(
        &mut self,
        clock: Clock,
        kind: MachineMetricKind,
        gpu_id: Option<&str>,
        status: MachineMetricStatus,
        value: Option<f64>,
    ) -> Result<bool, StateError> {
        self.record_batch(
            clock,
            &[FieldUpdate {
                kind,
                gpu_id,
                status,
                value,
            }],
        )
    }

    fn record_batch(
        &mut self,
        clock: Clock,
        updates: &[FieldUpdate<'_>],
    ) -> Result<bool, StateError> {
        let mut reset = self.snapshot(clock)?.reset;
        match self.cache.record_batch(clock, updates) {
            Err(CacheError::ClockReversed) => {
                self.invalidate(clock, ResetCause::AcquisitionClock)?;
                reset = true;
                self.cache
                    .record_batch(clock, updates)
                    .map_err(StateError::Cache)?;
            }
            result => result.map_err(StateError::Cache)?,
        }
        Ok(reset)
    }

    /// One completed RAM acquisition: (total bytes, available bytes).
    /// All three fields become unavailable together if this attempt failed.
    pub fn record_ram(
        &mut self,
        clock: Clock,
        status: MachineMetricStatus,
        reading: Option<(u64, u64)>,
    ) -> Result<bool, StateError> {
        let values = memory_values(status, reading)?;
        self.record_batch(
            clock,
            &[
                FieldUpdate {
                    kind: MachineMetricKind::RamTotal,
                    gpu_id: None,
                    status,
                    value: values.map(|(total, _)| total),
                },
                FieldUpdate {
                    kind: MachineMetricKind::RamAvailable,
                    gpu_id: None,
                    status,
                    value: values.map(|(_, available)| available),
                },
                FieldUpdate {
                    kind: MachineMetricKind::RamUsed,
                    gpu_id: None,
                    status,
                    value: values.map(|(total, available)| total - available),
                },
            ],
        )
    }

    /// One completed GPU memory acquisition: (total bytes, used bytes).
    /// Utilization is an independent acquisition with an independent status.
    pub fn record_vram(
        &mut self,
        clock: Clock,
        gpu_id: &str,
        status: MachineMetricStatus,
        reading: Option<(u64, u64)>,
    ) -> Result<bool, StateError> {
        let values = memory_values(status, reading)?;
        self.record_batch(
            clock,
            &[
                FieldUpdate {
                    kind: MachineMetricKind::GpuMemoryTotal,
                    gpu_id: Some(gpu_id),
                    status,
                    value: values.map(|(total, _)| total),
                },
                FieldUpdate {
                    kind: MachineMetricKind::GpuMemoryUsed,
                    gpu_id: Some(gpu_id),
                    status,
                    value: values.map(|(_, used)| used),
                },
            ],
        )
    }

    /// Refuse pre-reset or caller-invented current fields before staging a
    /// frame. Original acquisition identities are serialized inside each frame.
    pub fn append_frame(
        &mut self,
        clock: Clock,
        frame: &MachineMetricsResponse,
    ) -> Result<bool, StateError> {
        let snapshot = self.snapshot(clock)?;
        for series in &frame.series {
            let field = snapshot
                .fields
                .iter()
                .find(|field| field.kind == series.kind && field.gpu_id == series.gpu_id);
            let matches = match field {
                Some(field) => field.current == series.current,
                None => {
                    series.current.status == MachineMetricStatus::WarmingUp
                        && series.current.value.is_none()
                        && series.current.sampled_at.is_none()
                        && series.current.age_ms.is_none()
                }
            };
            if !matches {
                return Err(StateError::FrameMismatch);
            }
        }
        self.history
            .push_with_fields(clock, frame, &snapshot.fields)
            .map_err(StateError::History)?;
        Ok(snapshot.reset)
    }
}

fn memory_values(
    status: MachineMetricStatus,
    reading: Option<(u64, u64)>,
) -> Result<Option<(f64, f64)>, StateError> {
    match (status, reading) {
        (MachineMetricStatus::Ok, Some((total, part)))
            if part <= total && total <= 9_007_199_254_740_991 =>
        {
            Ok(Some((total as f64, part as f64)))
        }
        (status, None)
            if status != MachineMetricStatus::Ok && status != MachineMetricStatus::Stale =>
        {
            Ok(None)
        }
        _ => Err(StateError::Cache(CacheError::InvalidObservation)),
    }
}

#[cfg(test)]
mod tests;
