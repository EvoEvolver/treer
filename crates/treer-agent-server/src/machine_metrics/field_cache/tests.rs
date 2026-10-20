use super::*;
use chrono::{DateTime, Duration};

#[test]
fn batch_failure_preserves_all_values_and_sequence_even_on_last_field() {
    let mut cache = FieldCache::new(0);
    let updates = [
        FieldUpdate {
            kind: MachineMetricKind::RamTotal,
            gpu_id: None,
            status: MachineMetricStatus::Ok,
            value: Some(100.0),
        },
        FieldUpdate {
            kind: MachineMetricKind::RamAvailable,
            gpu_id: None,
            status: MachineMetricStatus::Ok,
            value: Some(50.0),
        },
        FieldUpdate {
            kind: MachineMetricKind::RamUsed,
            gpu_id: None,
            status: MachineMetricStatus::Ok,
            value: Some(50.0),
        },
    ];
    cache.record_batch(clock(0), &updates).unwrap();
    let before: Vec<_> = cache
        .snapshot(clock(0))
        .unwrap()
        .into_iter()
        .map(|f| f.observation)
        .collect();
    let mut invalid = updates;
    invalid[0].value = Some(200.0);
    invalid[2].value = Some(f64::NAN);
    assert_eq!(
        cache.record_batch(clock(5000), &invalid),
        Err(CacheError::InvalidObservation)
    );
    assert_eq!(cache.next_tick, 3);
    assert_eq!(
        cache
            .snapshot(clock(5000))
            .unwrap()
            .into_iter()
            .map(|f| f.observation)
            .collect::<Vec<_>>(),
        before
    );
    assert_eq!(
        cache.record_batch(clock(5000), &[updates[0], updates[0]]),
        Err(CacheError::InvalidField)
    );
    assert_eq!(
        cache.record_batch(clock(5000), &[]),
        Err(CacheError::InvalidField)
    );
    assert_eq!(
        cache.record_batch(
            clock(5000),
            &vec![updates[0]; MACHINE_METRICS_MAX_SERIES + 1]
        ),
        Err(CacheError::InvalidField)
    );
    cache.next_tick = u64::MAX - 1;
    assert_eq!(
        cache.record_batch(clock(5000), &updates[..2]),
        Err(CacheError::SequenceExhausted)
    );
    assert_eq!(cache.next_tick, u64::MAX - 1);
    assert_eq!(
        cache
            .snapshot(clock(5000))
            .unwrap()
            .into_iter()
            .map(|f| f.observation)
            .collect::<Vec<_>>(),
        before
    );

    let mut full = FieldCache::new(0);
    for index in 0..99 {
        full.record(
            clock(0),
            MachineMetricKind::GpuUtilization,
            Some(&format!("{index:064x}")),
            MachineMetricStatus::Ok,
            Some(1.0),
        )
        .unwrap();
    }
    assert_eq!(
        full.record_batch(clock(5000), &updates),
        Err(CacheError::Capacity)
    );
    assert_eq!(full.fields.len(), 99);
    assert_eq!(full.next_tick, 99);
    assert!(full
        .snapshot(clock(5000))
        .unwrap()
        .iter()
        .all(|f| f.kind == MachineMetricKind::GpuUtilization));
}

#[test]
fn expiration_validates_clocks_first_and_never_relabels_or_repeats_retirement() {
    let mut cache = FieldCache::new(7);
    cache
        .record(
            clock(0),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(12.0),
        )
        .unwrap();
    let day = MACHINE_METRICS_MAX_RANGE_MS as u64;
    let uncertain = Clock {
        wall: clock(day + 1).wall,
        monotonic_ms: 1,
    };
    assert_eq!(
        cache.retire_expired(uncertain),
        Err(CacheError::ClockReversed)
    );
    assert_eq!(cache.fields.len(), 1);
    assert_eq!(cache.retire_expired(clock(day)), Ok(0));
    let fractional = Clock {
        wall: clock(day).wall + Duration::nanoseconds(1),
        monotonic_ms: day,
    };
    assert_eq!(cache.retire_expired(fractional), Ok(1));
    assert_eq!(cache.retire_expired(fractional), Ok(0));
    assert!(cache.snapshot(fractional).unwrap().is_empty());
    cache
        .record(
            clock(day + 1),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(12.0),
        )
        .unwrap();
    let fresh = cache.snapshot(clock(day + 1)).unwrap();
    assert_eq!(
        (fresh[0].observation.epoch, fresh[0].observation.tick),
        (7, 1)
    );
}

fn clock(ms: u64) -> Clock {
    Clock {
        wall: DateTime::from_timestamp(1_792_500_000, 0).unwrap()
            + Duration::milliseconds(ms as i64),
        monotonic_ms: ms,
    }
}

#[test]
fn cached_reads_do_not_relabel_acquisitions_and_stale_keeps_history_value() {
    let mut cache = FieldCache::new(7);
    let id = "a".repeat(64);
    cache
        .record(
            clock(0),
            MachineMetricKind::GpuUtilization,
            Some(&id),
            MachineMetricStatus::Ok,
            Some(25.0),
        )
        .unwrap();
    for now in [0, 5000, 15000] {
        let fields = cache.snapshot(clock(now)).unwrap();
        let field = &fields[0];
        assert_eq!((field.observation.epoch, field.observation.tick), (7, 0));
        assert_eq!(field.observation.at, clock(0).wall);
        assert_eq!(field.current.value, Some(25.0));
        assert_eq!(field.current.age_ms, Some(now));
    }
    let stale = cache.snapshot(clock(15001)).unwrap();
    assert_eq!(stale[0].current.status, MachineMetricStatus::Stale);
    assert_eq!(stale[0].current.value, None);
    assert_eq!(stale[0].observation.value, Some(25.0));
    assert_eq!(stale[0].observation.tick, 0);
    cache
        .record(
            clock(20000),
            MachineMetricKind::GpuUtilization,
            Some(&id),
            MachineMetricStatus::TimedOut,
            None,
        )
        .unwrap();
    let failed = cache.snapshot(clock(20000)).unwrap();
    assert_eq!(failed[0].observation.tick, 1);
    assert_eq!(failed[0].current.status, MachineMetricStatus::TimedOut);
    assert_eq!(failed[0].current.value, None);
    assert_eq!(failed[0].current.age_ms, Some(0));
}

#[test]
fn rejected_updates_consume_no_identity_and_epoch_clears_old_fields() {
    let mut cache = FieldCache::new(0);
    cache
        .record(
            clock(10000),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(0.0),
        )
        .unwrap();
    for at in [clock(10000), clock(5000)] {
        assert_eq!(
            cache.record(
                at,
                MachineMetricKind::CpuUtilization,
                None,
                MachineMetricStatus::Ok,
                Some(10.0)
            ),
            Err(CacheError::ClockReversed)
        );
    }
    assert_eq!(
        cache.record(
            clock(15000),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Stale,
            None
        ),
        Err(CacheError::InvalidObservation)
    );
    assert_eq!(
        cache.record(
            clock(15000),
            MachineMetricKind::RamUsed,
            Some("raw-id"),
            MachineMetricStatus::Ok,
            Some(1.0)
        ),
        Err(CacheError::InvalidField)
    );
    assert_eq!(
        cache.record(
            clock(15000),
            MachineMetricKind::RamUsed,
            None,
            MachineMetricStatus::Ok,
            Some(0.5)
        ),
        Err(CacheError::InvalidObservation)
    );
    assert_eq!(cache.start_epoch(0), Err(CacheError::InvalidEpoch));
    cache.start_epoch(1).unwrap();
    assert!(cache.snapshot(clock(0)).unwrap().is_empty());
    cache
        .record(
            clock(0),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(10.0),
        )
        .unwrap();
    let fields = cache.snapshot(clock(0)).unwrap();
    assert_eq!(
        (fields[0].observation.epoch, fields[0].observation.tick),
        (1, 1)
    );
    assert!(matches!(
        cache.snapshot(Clock {
            wall: clock(20000).wall,
            monotonic_ms: 1
        }),
        Err(CacheError::ClockReversed)
    ));
}

#[test]
fn capacity_and_sequence_exhaustion_preserve_existing_fields() {
    let mut cache = FieldCache::new(0);
    for i in 0..MACHINE_METRICS_MAX_SERIES {
        cache
            .record(
                clock(0),
                MachineMetricKind::GpuUtilization,
                Some(&format!("{i:064x}")),
                MachineMetricStatus::Ok,
                Some(1.0),
            )
            .unwrap();
    }
    assert_eq!(
        cache.record(
            clock(0),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(1.0)
        ),
        Err(CacheError::Capacity)
    );
    assert_eq!(
        cache.snapshot(clock(0)).unwrap().len(),
        MACHINE_METRICS_MAX_SERIES
    );
    cache.next_tick = u64::MAX;
    assert_eq!(
        cache.record(
            clock(5000),
            MachineMetricKind::GpuUtilization,
            Some(&format!("{:064x}", 0)),
            MachineMetricStatus::Ok,
            Some(2.0)
        ),
        Err(CacheError::SequenceExhausted)
    );
    let fields = cache.snapshot(clock(5000)).unwrap();
    assert!(fields.iter().all(|f| f.observation.value == Some(1.0)));
}

#[test]
fn repeated_cached_projections_cannot_be_counted_as_new_samples() {
    let mut cache = FieldCache::new(0);
    cache
        .record(
            clock(0),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::Ok,
            Some(10.0),
        )
        .unwrap();
    let first = cache.snapshot(clock(5000)).unwrap()[0].observation;
    let again = cache.snapshot(clock(10000)).unwrap()[0].observation;
    let query = treer_protocol::MachineMetricsQuery {
        from: clock(0).wall,
        to: clock(10000).wall,
        max_points: 1,
    };
    assert!(matches!(
        super::super::aggregation::aggregate(
            &query,
            query.to,
            MachineMetricUnit::Percent,
            [first, again]
        ),
        Err(super::super::aggregation::AggregationError::DuplicateAcquisition)
    ));
}
