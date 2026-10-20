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

#[test]
fn response_encoding_validates_and_discards_every_cancelled_partial_buffer() {
    use crate::machine_metrics::response_encoding::{encode_bounded, EncodingError};
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let response = frame(&mut state, clock(5_000, 5_000));
    let query = MachineMetricsQuery {
        from: response.from,
        to: response.to,
        max_points: 1,
    };
    let mut checks = 0;
    let bytes = encode_bounded(
        &response,
        &query,
        "machine",
        response.generated_at,
        &mut || {
            checks += 1;
            false
        },
    )
    .unwrap();
    assert_eq!(
        MachineMetricsResponse::parse_bounded(&bytes, &query, "machine", response.generated_at)
            .unwrap(),
        response
    );
    assert_eq!(bytes, serde_json::to_vec(&response).unwrap());
    assert!(checks > 10);
    for stop_at in [1, 2, checks / 2, checks] {
        let mut calls = 0;
        assert_eq!(
            encode_bounded(
                &response,
                &query,
                "machine",
                response.generated_at,
                &mut || {
                    calls += 1;
                    calls == stop_at
                }
            ),
            Err(EncodingError::Cancelled)
        );
    }
    assert_eq!(
        encode_bounded(
            &response,
            &query,
            "another-machine",
            response.generated_at,
            &mut || false
        ),
        Err(EncodingError::InvalidResponse)
    );
    let mut bad = response;
    bad.series[0].current.value = Some(f64::NAN);
    assert_eq!(
        encode_bounded(&bad, &query, "machine", bad.generated_at, &mut || false),
        Err(EncodingError::InvalidResponse)
    );
}

#[test]
fn valid_model_exceeding_encoded_cap_is_refused_as_a_whole() {
    use crate::machine_metrics::response_encoding::{encode_bounded, EncodingError};
    use treer_protocol::{MachineMetricBucket, MachineMetricStatusCount};
    let mut state = state();
    let now = clock(86_400_000, 86_400_000);
    let mut response = frame(&mut state, now);
    response.from = clock(0, 0).wall;
    response.bucket_width_ms = 120_000;
    let query = MachineMetricsQuery {
        from: response.from,
        to: now.wall,
        max_points: 720,
    };
    // A semantically valid, status-rich maximum-point system response. The
    // source must refuse it rather than trim fields/buckets after assembly.
    for series in &mut response.series {
        for index in 0..720 {
            let from = query.from + Duration::seconds(index * 120);
            series.points.push(MachineMetricBucket {
                from,
                to: from + Duration::seconds(120),
                expected_count: 24,
                valid_count: 0,
                status_counts: [
                    MachineMetricStatus::WarmingUp,
                    MachineMetricStatus::NoDevice,
                    MachineMetricStatus::DriverUnavailable,
                    MachineMetricStatus::UnsupportedPlatform,
                    MachineMetricStatus::UnsupportedMetric,
                    MachineMetricStatus::CollectionFailed,
                    MachineMetricStatus::TimedOut,
                    MachineMetricStatus::Stale,
                    MachineMetricStatus::DeviceAbsent,
                    MachineMetricStatus::Disabled,
                ]
                .into_iter()
                .map(|status| MachineMetricStatusCount { status, count: 1 })
                .collect(),
                first_sample_at: None,
                last_sample_at: None,
                mean: None,
                min: None,
                max: None,
                partial: true,
            });
        }
    }
    response
        .validate_model(&query, "machine", now.wall)
        .unwrap();
    assert!(response.validate(&query, "machine", now.wall).is_err());
    // The unbounded serializer is used only as a test oracle, not by the encoder.
    assert!(
        serde_json::to_vec(&response).unwrap().len()
            > treer_protocol::MACHINE_METRICS_MAX_RESPONSE_BYTES
    );
    assert_eq!(
        encode_bounded(&response, &query, "machine", now.wall, &mut || false),
        Err(EncodingError::TooLarge)
    );
}

#[test]
fn multi_field_query_caps_combined_buckets_and_validates_before_reduction() {
    use crate::machine_metrics::field_query::FieldKey;
    let mut state = state();
    let now = clock(86_400_000, 86_400_000);
    let mut view = state.detached_query(now, &mut || false).unwrap();
    let query = MachineMetricsQuery {
        from: clock(0, 0).wall,
        to: now.wall,
        max_points: 720,
    };
    let mut fields: Vec<_> = [
        MachineMetricKind::CpuUtilization,
        MachineMetricKind::RamUsed,
        MachineMetricKind::RamAvailable,
        MachineMetricKind::RamTotal,
    ]
    .into_iter()
    .map(|kind| FieldKey { kind, gpu_id: None })
    .collect();
    for gpu in 0..32 {
        for kind in [
            MachineMetricKind::GpuUtilization,
            MachineMetricKind::GpuMemoryUsed,
            MachineMetricKind::GpuMemoryTotal,
        ] {
            fields.push(FieldKey {
                kind,
                gpu_id: Some(format!("{gpu:064x}")),
            });
        }
    }
    let result = view.query_fields(&query, &fields, &mut || false).unwrap();
    assert_eq!(result.fields.len(), 100);
    assert_eq!(
        result
            .fields
            .iter()
            .map(|(_, buckets)| buckets.points.len())
            .sum::<usize>(),
        7200
    );
    assert!(result
        .fields
        .iter()
        .all(|(_, buckets)| buckets.points.len() == 72
            && buckets.points.last().unwrap().to == query.to));
    let invalid = MachineMetricsQuery {
        max_points: 721,
        ..query
    };
    assert!(matches!(
        view.query_fields(&invalid, &fields, &mut || false),
        Err(FieldQueryError::Aggregation(
            crate::machine_metrics::aggregation::AggregationError::InvalidQuery
        ))
    ));
    fields.push(FieldKey {
        kind: MachineMetricKind::CpuUtilization,
        gpu_id: None,
    });
    assert!(matches!(
        view.query_fields(&invalid, &fields, &mut || false),
        Err(FieldQueryError::InvalidFields)
    ));
}

#[test]
fn multi_field_query_deduplicates_and_rejects_limits_without_partial_results() {
    use crate::machine_metrics::field_query::FieldKey;
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    state
        .record_ram(clock(0, 0), MachineMetricStatus::Ok, Some((100, 70)))
        .unwrap();
    for ms in [0, 5_000, 10_000] {
        let sample = frame(&mut state, clock(ms, ms as u64));
        state.append_frame(clock(ms, ms as u64), &sample).unwrap();
    }
    let now = clock(15_000, 15_000);
    let query = MachineMetricsQuery {
        from: clock(0, 0).wall,
        to: now.wall,
        max_points: 3,
    };
    let fields: Vec<_> = [
        MachineMetricKind::RamUsed,
        MachineMetricKind::CpuUtilization,
        MachineMetricKind::RamTotal,
    ]
    .into_iter()
    .map(|kind| FieldKey { kind, gpu_id: None })
    .collect();
    let mut view = state.detached_query(now, &mut || false).unwrap();
    let mut calls = 0;
    let result = view
        .query_fields(&query, &fields, &mut || {
            calls += 1;
            false
        })
        .unwrap();
    assert_eq!(result.epoch, 0);
    assert_eq!(
        result.fields.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        fields.iter().collect::<Vec<_>>()
    );
    for ((key, buckets), expected) in result.fields.iter().zip([30.0, 12.0, 100.0]) {
        assert_eq!(buckets.points[0].mean, Some(expected));
        assert!(buckets.points[1..]
            .iter()
            .all(|bucket| bucket.valid_count == 0));
        let single = view
            .query_field(&query, key.kind, None, &mut || false)
            .unwrap();
        assert_eq!(buckets.points, single.buckets.points);
    }
    assert!(matches!(
        view.query_fields_bounded(&query, &fields, &mut || false, 2),
        Err(FieldQueryError::IndexLimit)
    ));
    assert!(view
        .query_fields_bounded(&query, &fields, &mut || false, 3)
        .is_ok());
    assert!(matches!(
        view.query_fields(&query, &[fields[0].clone(), fields[0].clone()], &mut || {
            false
        }),
        Err(FieldQueryError::InvalidFields)
    ));
    assert!(matches!(
        view.query_fields(&query, &[], &mut || false),
        Err(FieldQueryError::InvalidFields)
    ));
    for stop_at in 1..=calls {
        let mut calls = 0;
        assert!(matches!(
            view.query_fields(&query, &fields, &mut || {
                calls += 1;
                calls == stop_at
            }),
            Err(FieldQueryError::Aggregation(
                crate::machine_metrics::aggregation::AggregationError::Cancelled
            ))
        ));
    }
    let mut keys = fields;
    keys.push(FieldKey {
        kind: MachineMetricKind::GpuUtilization,
        gpu_id: Some("a".repeat(64)),
    });
    let missing = view.query_fields(&query, &keys, &mut || false).unwrap();
    assert!(missing.fields[3]
        .1
        .points
        .iter()
        .all(|bucket| bucket.mean.is_none() && bucket.partial));
}

#[test]
fn detached_query_releases_collector_borrow_and_bounds_retained_views() {
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    let sample = frame(&mut state, clock(0, 0));
    state.append_frame(clock(0, 0), &sample).unwrap();
    let now = clock(5_000, 5_000);
    let query = MachineMetricsQuery {
        from: clock(0, 0).wall,
        to: now.wall,
        max_points: 1,
    };
    let mut view = state.detached_query(now, &mut || false).unwrap();
    assert!(matches!(
        state.detached_query(now, &mut || false),
        Err(StateError::History(HistoryError::SnapshotBusy))
    ));
    // A detached reader owns no borrow or lock on the source. Actual collector
    // progress and even an epoch reset can occur while old immutable bytes live.
    record(&mut state, now, 99.0);
    let sample = frame(&mut state, now);
    state.append_frame(now, &sample).unwrap();
    state.snapshot(clock(10_000, 1)).unwrap();
    assert_eq!(state.reset_diagnostics().count, 1);
    let old = view
        .query_field(&query, MachineMetricKind::CpuUtilization, None, &mut || {
            false
        })
        .unwrap();
    assert_eq!(old.epoch, 0);
    assert_eq!(old.buckets.points[0].mean, Some(12.0));
    assert!(matches!(
        state.detached_query(clock(10_000, 1), &mut || false),
        Err(StateError::History(HistoryError::SnapshotBusy))
    ));
    // Cancellation does not accidentally release admission while a view lives.
    assert!(view
        .query_field(&query, MachineMetricKind::CpuUtilization, None, &mut || {
            true
        })
        .is_err());
    drop(view);
    let mut next = state
        .detached_query(clock(10_000, 1), &mut || false)
        .unwrap();
    let fresh = next
        .query_field(&query, MachineMetricKind::CpuUtilization, None, &mut || {
            false
        })
        .unwrap();
    assert_eq!(fresh.epoch, 1);
    assert_eq!(fresh.buckets.points[0].mean, None);
    drop(next);
    // Unwinding a worker also releases its reservation.
    let view = state
        .detached_query(clock(10_000, 1), &mut || false)
        .unwrap();
    let _ = std::panic::catch_unwind(move || {
        let _held = view;
        panic!("worker failure");
    });
    assert!(state
        .detached_query(clock(10_000, 1), &mut || false)
        .is_ok());
}

#[test]
fn cancelled_query_never_returns_partial_buckets_or_consumes_acquisitions() {
    use crate::machine_metrics::aggregation::AggregationError;
    let mut state = state();
    for ms in [0, 5_000, 10_000] {
        record(&mut state, clock(ms, ms as u64), 12.0);
        let sample = frame(&mut state, clock(ms, ms as u64));
        state.append_frame(clock(ms, ms as u64), &sample).unwrap();
    }
    let now = clock(15_000, 15_000);
    let query = MachineMetricsQuery {
        from: clock(0, 0).wall,
        to: now.wall,
        max_points: 3,
    };
    let mut checks = 0;
    let complete = state
        .query_field_cancellable(
            now,
            &query,
            MachineMetricKind::CpuUtilization,
            None,
            &mut || {
                checks += 1;
                false
            },
        )
        .unwrap();
    assert_eq!(complete.buckets.points.len(), 3);
    assert!(checks > 10); // Includes scan, observation and output phases.
    let tick = state.snapshot(now).unwrap().fields[0].observation.tick;
    // Interrupt at every checkpoint, including after some buckets were built.
    for stop_at in 1..=checks {
        let mut visits = 0;
        let result = state.query_field_cancellable(
            now,
            &query,
            MachineMetricKind::CpuUtilization,
            None,
            &mut || {
                visits += 1;
                visits == stop_at
            },
        );
        assert!(matches!(
            result,
            Err(StateError::Query(FieldQueryError::Aggregation(
                AggregationError::Cancelled
            )))
        ));
        assert_eq!(visits, stop_at);
        assert_eq!(
            state.snapshot(now).unwrap().fields[0].observation.tick,
            tick
        );
    }
    // Already cancelled requests do not trigger clock-reset maintenance.
    assert!(state
        .query_field_cancellable(
            clock(15_000, 1),
            &query,
            MachineMetricKind::CpuUtilization,
            None,
            &mut || true,
        )
        .is_err());
    assert_eq!(state.reset_diagnostics().count, 0);
    let retry = state
        .query_field(now, &query, MachineMetricKind::CpuUtilization, None)
        .unwrap();
    assert_eq!(retry.buckets.points, complete.buckets.points);
}

#[test]
fn field_query_counts_real_acquisitions_once_and_keeps_cached_slots_empty() {
    use treer_protocol::MachineMetricsQuery;
    let mut state = state();
    record(&mut state, clock(0, 0), 12.0);
    for ms in [0, 5_000, 10_000] {
        let sample = frame(&mut state, clock(ms, ms as u64));
        state.append_frame(clock(ms, ms as u64), &sample).unwrap();
    }
    state
        .record(
            clock(15_000, 15_000),
            MachineMetricKind::CpuUtilization,
            None,
            MachineMetricStatus::TimedOut,
            None,
        )
        .unwrap();
    let sample = frame(&mut state, clock(15_000, 15_000));
    state.append_frame(clock(15_000, 15_000), &sample).unwrap();
    let query = MachineMetricsQuery {
        from: clock(0, 0).wall,
        to: clock(20_000, 20_000).wall,
        max_points: 4,
    };
    let result = state
        .query_field(
            clock(20_000, 20_000),
            &query,
            MachineMetricKind::CpuUtilization,
            None,
        )
        .unwrap();
    assert_eq!(result.epoch, 0);
    assert!(!result.reset);
    let points = result.buckets.points;
    assert_eq!(points.len(), 4);
    assert_eq!(points[0].valid_count, 1);
    assert_eq!(points[0].mean, Some(12.0));
    assert!(points[1..3].iter().all(|p| p.valid_count == 0
        && p.status_counts.is_empty()
        && p.mean.is_none()
        && p.partial));
    assert_eq!(points[3].valid_count, 0);
    assert_eq!(
        points[3].status_counts[0].status,
        MachineMetricStatus::TimedOut
    );
    assert_eq!(points[3].status_counts[0].count, 1);
    let again = state
        .query_field(
            clock(20_000, 20_000),
            &query,
            MachineMetricKind::CpuUtilization,
            None,
        )
        .unwrap();
    assert_eq!(again.buckets.points, points);
    let missing = state
        .query_field(
            clock(20_000, 20_000),
            &query,
            MachineMetricKind::RamTotal,
            None,
        )
        .unwrap();
    assert!(missing
        .buckets
        .points
        .iter()
        .all(|p| p.mean.is_none() && p.status_counts.is_empty()));
    let capture = state.snapshot(clock(20_000, 20_000)).unwrap();
    assert_eq!(capture.fields[0].observation.tick, 1);
    let invalid = MachineMetricsQuery {
        max_points: 0,
        ..query
    };
    assert!(state
        .query_field(
            clock(20_000, 20_000),
            &invalid,
            MachineMetricKind::CpuUtilization,
            None
        )
        .is_err());
    assert!(state
        .query_field(
            clock(20_000, 20_000),
            &query,
            MachineMetricKind::GpuUtilization,
            None
        )
        .is_err());
    let after_reset = state
        .query_field(
            clock(25_000, 1),
            &query,
            MachineMetricKind::CpuUtilization,
            None,
        )
        .unwrap();
    assert!(after_reset.reset);
    assert_eq!(after_reset.epoch, 1);
    assert!(after_reset
        .buckets
        .points
        .iter()
        .all(|p| p.mean.is_none() && p.status_counts.is_empty()));
}

#[test]
fn ram_batch_keeps_capacity_arithmetic_time_and_failure_status_consistent() {
    let mut state = state();
    state
        .record_ram(clock(0, 0), MachineMetricStatus::Ok, Some((100, 40)))
        .unwrap();
    let snapshot = state.snapshot(clock(0, 0)).unwrap();
    assert_eq!(snapshot.fields.len(), 3);
    assert!(snapshot
        .fields
        .iter()
        .all(|f| f.current.sampled_at == Some(clock(0, 0).wall)));
    let values: std::collections::HashMap<_, _> = snapshot
        .fields
        .iter()
        .map(|f| (f.kind, f.current.value))
        .collect();
    assert_eq!(values[&MachineMetricKind::RamUsed], Some(60.0));
    assert_eq!(values[&MachineMetricKind::RamAvailable], Some(40.0));
    assert_eq!(values[&MachineMetricKind::RamTotal], Some(100.0));
    let valid = frame(&mut state, clock(0, 0));
    state.append_frame(clock(0, 0), &valid).unwrap();
    for reading in [Some((100, 101)), Some((u64::MAX, 0)), None] {
        assert_eq!(
            state.record_ram(clock(5000, 5000), MachineMetricStatus::Ok, reading),
            Err(StateError::Cache(CacheError::InvalidObservation))
        );
    }
    let unchanged = state.snapshot(clock(5000, 5000)).unwrap();
    assert_eq!(
        unchanged
            .fields
            .iter()
            .map(|f| f.observation)
            .collect::<Vec<_>>(),
        snapshot
            .fields
            .iter()
            .map(|f| f.observation)
            .collect::<Vec<_>>()
    );
    state
        .record_ram(
            clock(5000, 5000),
            MachineMetricStatus::CollectionFailed,
            None,
        )
        .unwrap();
    let failed = state.snapshot(clock(5000, 5000)).unwrap();
    assert!(failed.fields.iter().all(|f| f.current.status
        == MachineMetricStatus::CollectionFailed
        && f.current.value.is_none()
        && f.current.sampled_at == Some(clock(5000, 5000).wall)
        && f.observation.tick >= 3));
    let valid = frame(&mut state, clock(5000, 5000));
    state.append_frame(clock(5000, 5000), &valid).unwrap();
    assert!(state
        .record_ram(clock(5000, 5000), MachineMetricStatus::Ok, Some((200, 80)))
        .unwrap());
    let retried = state.snapshot(clock(5000, 5000)).unwrap();
    assert_eq!(retried.epoch, 1);
    assert_eq!(retried.fields.len(), 3);
    assert!(retried.fields.iter().all(|f| f.observation.epoch == 1
        && (6..=8).contains(&f.observation.tick)
        && f.current.status == MachineMetricStatus::Ok));
    assert_eq!(state.reset_diagnostics().count, 1);
    assert_eq!(state.history.record_count(), 0);
}

#[test]
fn vram_batch_is_atomic_and_independent_of_utilization_failure() {
    let mut state = state();
    let gpu = "a".repeat(64);
    state
        .record(
            clock(0, 0),
            MachineMetricKind::GpuUtilization,
            Some(&gpu),
            MachineMetricStatus::TimedOut,
            None,
        )
        .unwrap();
    state
        .record_vram(clock(0, 0), &gpu, MachineMetricStatus::Ok, Some((100, 60)))
        .unwrap();
    let fields = state.snapshot(clock(0, 0)).unwrap().fields;
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].current.status, MachineMetricStatus::TimedOut);
    assert_eq!(fields[1].current.value, Some(60.0));
    assert_eq!(fields[2].current.value, Some(100.0));
    assert!(fields
        .iter()
        .all(|f| f.current.sampled_at == Some(clock(0, 0).wall)));
    let valid = gpu_frame(&mut state, clock(0, 0), &gpu);
    state.append_frame(clock(0, 0), &valid).unwrap();
    assert_eq!(
        state.record_vram(
            clock(5000, 5000),
            &gpu,
            MachineMetricStatus::Ok,
            Some((100, 101))
        ),
        Err(StateError::Cache(CacheError::InvalidObservation))
    );
    state
        .record_vram(
            clock(5000, 5000),
            &gpu,
            MachineMetricStatus::UnsupportedMetric,
            None,
        )
        .unwrap();
    let fields = state.snapshot(clock(5000, 5000)).unwrap().fields;
    assert_eq!(fields[0].observation.tick, 0);
    assert!(fields[1..].iter().all(
        |f| f.current.status == MachineMetricStatus::UnsupportedMetric
            && f.current.value.is_none()
            && f.current.sampled_at == Some(clock(5000, 5000).wall)
    ));
    assert_eq!(
        (fields[1].observation.tick, fields[2].observation.tick),
        (4, 3)
    );
    let valid = gpu_frame(&mut state, clock(5000, 5000), &gpu);
    state.append_frame(clock(5000, 5000), &valid).unwrap();
}

fn gpu_frame(state: &mut CollectorState, clock: Clock, gpu: &str) -> MachineMetricsResponse {
    use treer_protocol::*;
    let mut sample = frame(state, clock);
    sample.gpus.push(MachineMetricsGpu {
        id: gpu.into(),
        name: "Test GPU".into(),
        present: true,
    });
    sample.gpu_status = MachineMetricStatus::Ok;
    for field in state.snapshot(clock).unwrap().fields {
        sample.series.push(MachineMetricSeries {
            kind: field.kind,
            gpu_id: field.gpu_id,
            unit: if field.kind == MachineMetricKind::GpuUtilization {
                MachineMetricUnit::Percent
            } else {
                MachineMetricUnit::Bytes
            },
            current: field.current,
            points: vec![],
        });
    }
    sample
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
