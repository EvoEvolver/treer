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
fn expired_static_field_retires_without_wedging_append_or_relabeling_fresh_fields() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    state
        .record(
            clock(0, 0),
            MachineMetricKind::RamTotal,
            None,
            MachineMetricStatus::Ok,
            Some(1024.0),
        )
        .unwrap();
    let day = 86_400_000;
    assert_eq!(
        state.snapshot(clock(day, day as u64)).unwrap().fields.len(),
        2
    );
    record(&mut state, clock(day, day as u64), 25.0);
    let snapshot = state.snapshot(clock(day + 1, day as u64 + 1)).unwrap();
    assert!(!snapshot.reset);
    assert_eq!(snapshot.epoch, 0);
    assert_eq!(snapshot.fields.len(), 1);
    assert_eq!(snapshot.fields[0].observation.tick, 2);
    assert_eq!(snapshot.fields[0].current.value, Some(25.0));
    assert_eq!(state.expired_field_count(), 1);
    assert_eq!(state.expired_field_count(), 1);
    let sample = frame(&mut state, clock(day + 1, day as u64 + 1));
    assert_eq!(
        sample.series[3].current.status,
        MachineMetricStatus::WarmingUp
    );
    assert_eq!(sample.series[3].current.sampled_at, None);
    state
        .append_frame(clock(day + 1, day as u64 + 1), &sample)
        .unwrap();
    assert_eq!(state.history.record_count(), 1);
    assert_eq!(state.expired_field_count(), 1);
    // A fresh read of the same capacity is a new acquisition, not cache reuse.
    state
        .record(
            clock(day + 2, day as u64 + 2),
            MachineMetricKind::RamTotal,
            None,
            MachineMetricStatus::Ok,
            Some(1024.0),
        )
        .unwrap();
    let fresh = state.snapshot(clock(day + 2, day as u64 + 2)).unwrap();
    let ram = fresh
        .fields
        .iter()
        .find(|field| field.kind == MachineMetricKind::RamTotal)
        .unwrap();
    assert_eq!(ram.observation.tick, 3);
    assert_eq!(ram.current.age_ms, Some(0));
    assert_eq!(ram.current.value, Some(1024.0));
    assert_eq!(state.expired_field_count(), 1);
}

#[test]
fn stored_frames_preserve_cached_identity_and_original_value_after_staleness() {
    use crate::machine_metrics::stored_frame::StoredFrame;
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let original = state.snapshot(clock(0, 0)).unwrap().fields[0].observation;
    for ms in [0, 5_000, 20_000] {
        let sample = frame(&mut state, clock(ms, ms as u64));
        state.append_frame(clock(ms, ms as u64), &sample).unwrap();
    }
    let frames: Vec<_> = state
        .history
        .staged_frames(clock(20_000, 20_000))
        .map(|frame| {
            (
                frame.tick,
                StoredFrame::decode(frame.bytes, "machine", frame.clock, frame.epoch).unwrap(),
            )
        })
        .collect();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[2].0, 2);
    for (_, stored) in &frames {
        assert_eq!(stored.fields[0].observation, original);
    }
    assert_eq!(
        frames[2].1.frame.series[0].current.status,
        MachineMetricStatus::Stale
    );
    assert_eq!(frames[2].1.frame.series[0].current.value, None);
    assert_eq!(frames[2].1.fields[0].observation.value, Some(12.0));
    state
        .record(
            clock(25_000, 25_000),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::TimedOut,
            None,
        )
        .unwrap();
    let failed = frame(&mut state, clock(25_000, 25_000));
    state.append_frame(clock(25_000, 25_000), &failed).unwrap();
    let stored = state
        .history
        .staged_frames(clock(25_000, 25_000))
        .last()
        .unwrap();
    let decoded = StoredFrame::decode(stored.bytes, "machine", stored.clock, stored.epoch).unwrap();
    assert_eq!(
        decoded.fields[0].observation.status,
        MachineMetricStatus::TimedOut
    );
    assert_eq!(decoded.fields[0].observation.value, None);
    assert_eq!(decoded.fields[0].observation.tick, original.tick + 1);
}

#[test]
fn stored_frame_refuses_corrupt_identity_links_and_bounds_before_parsing() {
    use crate::machine_metrics::{history::MAX_RECORD_BYTES, stored_frame::StoredFrame};
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let sample = frame(&mut state, clock(0, 0));
    state.append_frame(clock(0, 0), &sample).unwrap();
    let bytes = state.history.frames(clock(0, 0)).next().unwrap().to_vec();
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mutations: Vec<(&str, serde_json::Value)> = vec![
        ("version", 2.into()),
        ("epoch", 1.into()),
        ("fields.0.observation.epoch", 1.into()),
        ("fields.0.observation.tick", u64::MAX.into()),
        ("fields.0.observation.value", 99.into()),
        ("fields.0.observation.at", "2030-01-01T00:00:00Z".into()),
        ("fields.0.observation.status", "stale".into()),
        ("fields.0.observation.extra", true.into()),
        ("extra", true.into()),
    ];
    for (path, value) in mutations {
        let mut changed = original.clone();
        let mut target = &mut changed;
        let parts: Vec<_> = path.split('.').collect();
        for key in &parts[..parts.len() - 1] {
            target = if let Ok(index) = key.parse::<usize>() {
                &mut target[index]
            } else {
                &mut target[*key]
            };
        }
        target[parts[parts.len() - 1]] = value;
        assert!(
            StoredFrame::decode(
                &serde_json::to_vec(&changed).unwrap(),
                "machine",
                clock(0, 0),
                0
            )
            .is_err(),
            "{path}"
        );
    }
    let mut missing = original.clone();
    missing["fields"] = serde_json::json!([]);
    assert!(StoredFrame::decode(
        &serde_json::to_vec(&missing).unwrap(),
        "machine",
        clock(0, 0),
        0
    )
    .is_err());
    let mut duplicate = original.clone();
    duplicate["fields"]
        .as_array_mut()
        .unwrap()
        .push(original["fields"][0].clone());
    assert!(StoredFrame::decode(
        &serde_json::to_vec(&duplicate).unwrap(),
        "machine",
        clock(0, 0),
        0
    )
    .is_err());
    assert!(matches!(
        StoredFrame::decode(&vec![b'x'; MAX_RECORD_BYTES + 1], "machine", clock(0, 0), 0),
        Err(HistoryError::OversizedRecord)
    ));
    assert!(StoredFrame::decode(&bytes, "other-machine", clock(0, 0), 0).is_err());
}

#[test]
fn recent_frame_does_not_extend_original_field_retention() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let sample = frame(&mut state, clock(5_000, 5_000));
    state.append_frame(clock(5_000, 5_000), &sample).unwrap();
    state.history.maintain(clock(86_400_000, 86_400_000));
    assert_eq!(state.history.record_count(), 1);
    state.history.maintain(clock(86_400_001, 86_400_001));
    assert_eq!(state.history.record_count(), 0);
    assert!(!state.history.truncated());
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
