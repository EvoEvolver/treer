use super::*;
use treer_protocol::*;

fn clock(ms: u64) -> Clock {
    Clock {
        wall: DateTime::from_timestamp(1_792_500_000, 0).unwrap()
            + Duration::milliseconds(ms as i64),
        monotonic_ms: ms,
    }
}

fn sample(clock: Clock) -> MachineMetricsResponse {
    let series = [
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
        current: MachineMetricCurrent {
            status: MachineMetricStatus::WarmingUp,
            value: None,
            sampled_at: None,
            age_ms: None,
        },
        points: vec![],
    })
    .collect();
    MachineMetricsResponse {
        schema: CAPABILITY_MACHINE_METRICS.into(),
        server_id: "fixture-machine".into(),
        generated_at: clock.wall,
        from: clock.wall - Duration::seconds(5),
        to: clock.wall,
        retained_from: None,
        truncated: false,
        sample_interval_ms: 5000,
        bucket_width_ms: 5000,
        stale_after_ms: 15000,
        logical_cpus: Some(8),
        memory_method: MachineMemoryMethod::LinuxMemAvailable,
        scope: "os_visible_machine".into(),
        gpus: vec![],
        inventory_truncated: false,
        gpu_status: MachineMetricStatus::NoDevice,
        series,
    }
}

#[test]
fn history_evicts_oldest_on_point_and_exact_byte_limits() {
    let size = serde_json::to_vec(&sample(clock(0))).unwrap().len();
    let mut history = History::new(
        "fixture-machine".into(),
        Limits {
            records: 2,
            bytes: size * 2,
        },
    )
    .unwrap();
    for tick in 0..3 {
        let at = clock(tick * 5000);
        history.push(at, &sample(at)).unwrap();
    }
    assert_eq!(history.record_count(), 2);
    assert!(history.serialized_bytes() <= size * 2);
    assert!(history.truncated());
    let first: MachineMetricsResponse =
        serde_json::from_slice(history.frames(clock(10000)).next().unwrap()).unwrap();
    assert_eq!(first.generated_at, clock(5000).wall);

    let mut byte_limited = History::new(
        "fixture-machine".into(),
        Limits {
            records: 100,
            bytes: size,
        },
    )
    .unwrap();
    for tick in 0..4 {
        let at = clock(tick * 5000);
        byte_limited.push(at, &sample(at)).unwrap();
        assert_eq!(byte_limited.record_count(), 1);
        assert!(byte_limited.serialized_bytes() <= size);
    }
}

#[test]
fn history_ages_without_reads_and_wake_does_not_backfill() {
    let mut history = History::new("fixture-machine".into(), Limits::default()).unwrap();
    history.push(clock(0), &sample(clock(0))).unwrap();
    let day = MACHINE_METRICS_MAX_RANGE_MS as u64;
    history.maintain(clock(day));
    assert_eq!(history.record_count(), 1);
    history.maintain(clock(day + 1));
    assert_eq!(history.record_count(), 0);
    assert_eq!(history.serialized_bytes(), 0);
    assert!(!history.truncated()); // Routine retention roll-off is not loss.
    let wake = clock(day + 5000);
    history.push(wake, &sample(wake)).unwrap();
    assert_eq!(history.record_count(), 1); // No synthetic missed samples.
    assert_eq!(history.epoch(), 0); // Consistent wall/monotonic gap, not a jump.
    assert!(!history.truncated());
}

#[test]
fn history_discards_epochs_on_clock_uncertainty_and_restart_is_empty() {
    for fault in 0..4 {
        let mut history = History::new("fixture-machine".into(), Limits::default()).unwrap();
        history.push(clock(10000), &sample(clock(10000))).unwrap();
        let mut changed = clock(15000);
        match fault {
            0 => changed.wall = clock(5000).wall,
            1 => changed.wall += Duration::hours(2),
            2 => changed.monotonic_ms = 100,
            _ => changed.monotonic_ms = u64::MAX,
        }
        assert_eq!(history.frames(changed).count(), 0);
        assert_eq!(history.epoch(), 1);
        assert!(history.truncated());
    }
    let restarted = History::new("fixture-machine".into(), Limits::default()).unwrap();
    assert_eq!(restarted.record_count(), 0);
    assert_eq!(restarted.serialized_bytes(), 0);
}

#[test]
fn history_rejects_queries_foreign_invalid_and_duplicate_samples() {
    let mut history = History::new("fixture-machine".into(), Limits::default()).unwrap();
    let at = clock(0);
    let original = sample(at);
    for invalid in 0..4 {
        let mut value = original.clone();
        match invalid {
            0 => value.server_id = "foreign".into(),
            1 => value.series[0].current.value = Some(f64::NAN),
            2 => value.generated_at += Duration::seconds(1),
            _ => value.from -= Duration::seconds(1),
        }
        assert_eq!(history.push(at, &value), Err(HistoryError::InvalidSample));
        assert_eq!(history.record_count(), 0);
    }
    history.push(at, &original).unwrap();
    assert_eq!(
        history.push(at, &original),
        Err(HistoryError::DuplicateSample)
    );
    let mut tiny = History::new(
        "fixture-machine".into(),
        Limits {
            records: 1,
            bytes: 1,
        },
    )
    .unwrap();
    assert_eq!(tiny.push(at, &original), Err(HistoryError::OversizedRecord));
    assert_eq!(tiny.serialized_bytes(), 0);
    assert!(History::new(
        "x".into(),
        Limits {
            records: MAX_RECORDS + 1,
            bytes: MAX_BYTES
        }
    )
    .is_err());
    assert!(History::new(
        "x".into(),
        Limits {
            records: 1,
            bytes: MAX_BYTES + 1
        }
    )
    .is_err());
}

#[test]
fn frame_ticks_are_append_owned_and_survive_eviction_and_epoch_reset() {
    let mut history = History::new(
        "fixture-machine".into(),
        Limits {
            records: 1,
            bytes: MAX_BYTES,
        },
    )
    .unwrap();
    history.push(clock(0), &sample(clock(0))).unwrap();
    let first = history.staged_frames(clock(0)).next().unwrap();
    assert_eq!(
        (first.epoch, first.tick, first.clock.monotonic_ms),
        (0, 0, 0)
    );
    assert!(!first.bytes.is_empty());
    assert_eq!(
        history.push(clock(0), &sample(clock(0))),
        Err(HistoryError::DuplicateSample)
    );
    history.push(clock(5000), &sample(clock(5000))).unwrap();
    assert_eq!(history.staged_frames(clock(5000)).next().unwrap().tick, 1);
    // Readers neither allocate ticks nor reset their sequence.
    assert_eq!(history.staged_frames(clock(5000)).next().unwrap().tick, 1);
    let reset = Clock {
        wall: clock(10000).wall,
        monotonic_ms: 1,
    };
    history.push(reset, &sample(reset)).unwrap();
    let after = history.staged_frames(reset).next().unwrap();
    assert_eq!((after.epoch, after.tick), (1, 2));
    history.next_tick = u64::MAX; // Test-only exhaustion, never wrap/reuse.
    let later = Clock {
        wall: reset.wall + Duration::seconds(5),
        monotonic_ms: 5001,
    };
    assert_eq!(
        history.push(later, &sample(later)),
        Err(HistoryError::SequenceExhausted)
    );
    assert_eq!(history.record_count(), 1);
    assert_eq!(history.staged_frames(later).next().unwrap().tick, 2);
}
