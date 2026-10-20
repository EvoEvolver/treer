//! Inert collector state boundary. Owns both clocks' invalidation domain;
//! it does not collect hardware samples or expose a query route.
use super::{
    field_cache::{CacheError, FieldCache, FieldSnapshot},
    history::{Clock, History, HistoryError, Limits},
};
use treer_protocol::{MachineMetricKind, MachineMetricStatus, MachineMetricsResponse};

#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    History(HistoryError),
    Cache(CacheError),
    FrameMismatch,
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
}

impl CollectorState {
    pub fn new(server_id: String, limits: Limits) -> Result<Self, StateError> {
        Ok(Self {
            history: History::new(server_id, limits).map_err(StateError::History)?,
            cache: FieldCache::new(0),
            epoch: 0,
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
        Ok(true)
    }

    fn invalidate(&mut self, clock: Clock) -> Result<(), StateError> {
        self.history.invalidate(clock);
        self.synchronize(clock)?;
        Ok(())
    }

    /// Also the maintenance entry point for ticks with no completed capture.
    /// Cache-only accumulated clock drift invalidates history as well.
    pub fn snapshot(&mut self, clock: Clock) -> Result<Snapshot, StateError> {
        let mut reset = self.synchronize(clock)?;
        let fields = match self.cache.snapshot(clock) {
            Ok(fields) => fields,
            Err(CacheError::ClockReversed) => {
                self.invalidate(clock)?;
                reset = true;
                Vec::new()
            }
            Err(error) => return Err(StateError::Cache(error)),
        };
        Ok(Snapshot {
            epoch: self.epoch,
            reset,
            fields,
        })
    }

    /// Only real completed acquisitions/attempts may enter here. A conflicting
    /// acquisition clock clears both sides before accepting that new capture.
    pub fn record(
        &mut self,
        clock: Clock,
        kind: MachineMetricKind,
        gpu_id: Option<&str>,
        status: MachineMetricStatus,
        value: Option<f64>,
    ) -> Result<bool, StateError> {
        let mut reset = self.snapshot(clock)?.reset;
        match self.cache.record(clock, kind, gpu_id, status, value) {
            Err(CacheError::ClockReversed) => {
                self.invalidate(clock)?;
                reset = true;
                self.cache
                    .record(clock, kind, gpu_id, status, value)
                    .map_err(StateError::Cache)?;
            }
            result => result.map_err(StateError::Cache)?,
        }
        Ok(reset)
    }

    /// Refuse pre-reset or caller-invented current fields before staging a
    /// frame. This is not the future field-identity serialization adapter.
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
            .push(clock, frame)
            .map_err(StateError::History)?;
        Ok(snapshot.reset)
    }
}

#[cfg(test)]
mod tests;
