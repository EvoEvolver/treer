use super::*;
use chrono::{Duration, TimeZone, Utc};

fn clock(wall_ms: i64, monotonic_ms: u64) -> Clock {
    Clock {
        wall: Utc.timestamp_opt(1_800_000_000, 0).unwrap() + Duration::milliseconds(wall_ms),
        monotonic_ms,
    }
}

fn state() -> CollectorState {
    CollectorState::new("machine".into(), Limits::default()).unwrap()
}

fn record(state: &mut CollectorState, clock: Clock, value: f64) -> bool {
    state
        .record(
            clock,
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(value),
        )
        .unwrap()
}

#[test]
fn history_discontinuity_clears_fields_and_reacquisition_keeps_identity_distinct() {
    let mut state = state();
    assert_eq!(
        state.reset_diagnostics(),
        ResetDiagnostics {
            count: 0,
            last_cause: None
        }
    );
    record(&mut state, clock(0, 0), 12.0);
    let original = state.snapshot(clock(5_000, 5_000)).unwrap();
    let reset = state.snapshot(clock(10_000, 1)).unwrap();
    assert!(reset.reset);
    assert_eq!(reset.epoch, 1);
    assert_eq!(
        state.reset_diagnostics(),
        ResetDiagnostics {
            count: 1,
            last_cause: Some(ResetCause::HistoryClock),
        }
    );
    assert!(reset.fields.is_empty());
    assert_eq!(state.history.record_count(), 0);
    record(&mut state, clock(10_000, 1), 25.0);
    let fresh = state.snapshot(clock(10_000, 1)).unwrap();
    assert!(!fresh.reset);
    assert_eq!(fresh.fields[0].current.value, Some(25.0));
    assert_eq!(fresh.fields[0].observation.epoch, 1);
    assert!(fresh.fields[0].observation.tick > original.fields[0].observation.tick);
}

#[test]
fn cumulative_cache_drift_clears_both_sides_even_when_each_history_step_is_small() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    // Insert a validated empty-history frame using the existing fixture shape.
    let frame = frame(&mut state, clock(0, 0));
    state.append_frame(clock(0, 0), &frame).unwrap();
    for tick in 1..=10 {
        let snapshot = state
            .snapshot(clock(6_000 * tick, 5_000 * tick as u64))
            .unwrap();
        assert!(!snapshot.reset);
        assert_eq!(snapshot.fields.len(), 1);
    }
    let snapshot = state.snapshot(clock(66_000, 55_000)).unwrap();
    assert!(snapshot.reset);
    assert_eq!(snapshot.epoch, 1);
    assert!(snapshot.fields.is_empty());
    assert_eq!(state.history.record_count(), 0);
    assert!(state.history.truncated());
    assert_eq!(
        state.reset_diagnostics(),
        ResetDiagnostics {
            count: 1,
            last_cause: Some(ResetCause::FieldClock),
        }
    );
}

#[test]
fn repeated_conflicting_completions_are_visible_without_diagnostic_read_side_effects() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    for count in 1..=3 {
        // Simulates a buggy caller reusing an earlier completion timestamp.
        assert!(record(&mut state, clock(0, 0), 25.0));
        let expected = ResetDiagnostics {
            count,
            last_cause: Some(ResetCause::AcquisitionClock),
        };
        assert_eq!(state.reset_diagnostics(), expected);
        assert_eq!(state.reset_diagnostics(), expected);
        let snapshot = state.snapshot(clock(0, 0)).unwrap();
        assert_eq!(snapshot.epoch, count);
        assert!(!snapshot.reset);
        assert_eq!(snapshot.fields[0].observation.tick, count);
    }
}

fn frame(state: &mut CollectorState, clock: Clock) -> MachineMetricsResponse {
    use treer_protocol::*;
    let fields = state.snapshot(clock).unwrap().fields;
    MachineMetricsResponse {
        schema: "machine.metrics.v1".into(),
        server_id: "machine".into(),
        generated_at: clock.wall,
        from: clock.wall - Duration::seconds(5),
        to: clock.wall,
        retained_from: None,
        truncated: false,
        sample_interval_ms: 5_000,
        bucket_width_ms: 5_000,
        stale_after_ms: 15_000,
        logical_cpus: None,
        memory_method: MachineMemoryMethod::Unavailable,
        scope: "os_visible_machine".into(),
        gpus: vec![],
        inventory_truncated: false,
        gpu_status: MachineMetricStatus::NoDevice,
        series: [
            MachineMetricKind::CpuUtilization,
            MachineMetricKind::RamUsed,
            MachineMetricKind::RamAvailable,
            MachineMetricKind::RamTotal,
        ]
        .into_iter()
        .map(|kind| MachineMetricSeries {
            kind,
            gpu_id: None,
            unit: if kind == MachineMetricKind::CpuUtilization {
                MachineMetricUnit::Percent
            } else {
                MachineMetricUnit::Bytes
            },
            current: fields
                .iter()
                .find(|f| f.kind == kind)
                .map(|f| f.current.clone())
                .unwrap_or(MachineMetricCurrent {
                    status: MachineMetricStatus::WarmingUp,
                    value: None,
                    sampled_at: None,
                    age_ms: None,
                }),
            points: vec![],
        })
        .collect(),
    }
}

#[test]
fn pre_reset_frame_is_refused_and_only_fresh_capture_can_replace_it() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let old = frame(&mut state, clock(5_000, 5_000));
    assert_eq!(
        state.append_frame(clock(10_000, 1), &old),
        Err(StateError::FrameMismatch)
    );
    assert_eq!(state.history.record_count(), 0);
    assert!(state.snapshot(clock(10_000, 1)).unwrap().fields.is_empty());
    record(&mut state, clock(10_000, 1), 25.0);
    let fresh = frame(&mut state, clock(10_000, 1));
    state.append_frame(clock(10_000, 1), &fresh).unwrap();
    assert_eq!(state.history.record_count(), 1);
}
