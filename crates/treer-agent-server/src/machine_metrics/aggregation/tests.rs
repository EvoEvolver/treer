use super::*;

fn query(seconds: i64, max_points: u16) -> MachineMetricsQuery {
    let from = DateTime::from_timestamp(1_792_500_000, 0).unwrap();
    MachineMetricsQuery {
        from,
        to: from + Duration::seconds(seconds),
        max_points,
    }
}

fn reading(query: &MachineMetricsQuery, seconds: i64, value: Option<f64>) -> Observation {
    Observation {
        epoch: 0,
        tick: seconds.div_euclid(5) as u64,
        at: query.from + Duration::seconds(seconds),
        status: if value.is_some() {
            MachineMetricStatus::Ok
        } else {
            MachineMetricStatus::TimedOut
        },
        value,
    }
}

#[test]
fn statistics_keep_gaps_failures_and_valid_zero_distinct() {
    let q = query(20, 1);
    let result = aggregate(
        &q,
        q.to,
        MachineMetricUnit::Percent,
        [
            reading(&q, 10, Some(100.0)),
            reading(&q, 0, Some(0.0)),
            reading(&q, 15, None),
        ],
    )
    .unwrap();
    assert_eq!(result.width_ms, 20_000);
    let bucket = &result.points[0];
    assert_eq!((bucket.expected_count, bucket.valid_count), (4, 2));
    assert_eq!(
        (bucket.mean, bucket.min, bucket.max),
        (Some(50.0), Some(0.0), Some(100.0))
    );
    assert_eq!(bucket.first_sample_at, Some(q.from));
    assert_eq!(bucket.last_sample_at, Some(q.from + Duration::seconds(10)));
    assert!(bucket.partial);
    assert_eq!(bucket.status_counts.iter().map(|v| v.count).sum::<u32>(), 3);
    assert!(bucket
        .status_counts
        .iter()
        .any(|v| v.status == MachineMetricStatus::TimedOut && v.count == 1));
    let empty = aggregate(&q, q.to, MachineMetricUnit::Percent, []).unwrap();
    assert_eq!(empty.points[0].mean, None);
    assert_eq!(empty.points[0].first_sample_at, None);
    assert!(empty.points[0].partial);
}

#[test]
fn point_budget_and_end_exclusion_cover_full_day_and_partial_tail() {
    let q = query(86400, 720);
    let result = aggregate(&q, q.to, MachineMetricUnit::Percent, []).unwrap();
    assert_eq!(result.points.len(), 720);
    assert_eq!(result.width_ms, 120_000);
    assert_eq!(result.points.last().unwrap().to, q.to);
    let q = query(11, 2);
    let result = aggregate(
        &q,
        q.to,
        MachineMetricUnit::Percent,
        [
            reading(&q, -1, Some(50.0)),
            reading(&q, 0, Some(10.0)),
            reading(&q, 5, Some(20.0)),
            reading(&q, 10, Some(30.0)),
            reading(&q, 11, Some(100.0)),
        ],
    )
    .unwrap();
    assert_eq!(result.width_ms, 10_000);
    assert_eq!(result.points.len(), 2);
    assert!(!result.points[0].partial);
    assert!(!result.points[1].partial);
    assert_eq!(result.points[1].mean, Some(30.0));
    assert_eq!(result.points[1].expected_count, 1);
}

#[test]
fn repeated_slots_and_invalid_measurements_are_refused() {
    let q = query(10, 1);
    assert!(matches!(
        aggregate(
            &q,
            q.to,
            MachineMetricUnit::Percent,
            [reading(&q, 0, Some(10.0)), reading(&q, 1, Some(10.0))]
        ),
        Err(AggregationError::DuplicateAcquisition)
    ));
    for value in [f64::NAN, f64::INFINITY, -1.0, 100.1] {
        assert!(matches!(
            aggregate(
                &q,
                q.to,
                MachineMetricUnit::Percent,
                [reading(&q, 0, Some(value))]
            ),
            Err(AggregationError::InvalidObservation)
        ));
    }
    assert!(matches!(
        aggregate(
            &q,
            q.to,
            MachineMetricUnit::Bytes,
            [reading(&q, 0, Some(0.5))]
        ),
        Err(AggregationError::InvalidObservation)
    ));
    let mut invalid = q.clone();
    invalid.max_points = 0;
    assert!(matches!(
        aggregate(&invalid, q.to, MachineMetricUnit::Percent, []),
        Err(AggregationError::InvalidQuery)
    ));
}

#[test]
fn input_budget_applies_even_to_out_of_range_observations() {
    let q = query(10, 1);
    let outside = reading(&q, -1, Some(1.0));
    assert!(aggregate(
        &q,
        q.to,
        MachineMetricUnit::Percent,
        std::iter::repeat_n(outside, super::super::history::MAX_RECORDS)
    )
    .is_ok());
    assert!(matches!(
        aggregate(
            &q,
            q.to,
            MachineMetricUnit::Percent,
            std::iter::repeat_n(outside, super::super::history::MAX_RECORDS + 1)
        ),
        Err(AggregationError::TooManyObservations)
    ));
}

#[test]
fn generated_boundaries_pass_shared_wire_validation() {
    use treer_protocol::*;
    for (nanos, max_points) in [
        (1, 1),
        (999_999, 1),
        (5_000_000_001, 1),
        (5_000_000_001, 2),
        (11_000_000_001, 2),
        (86_400_000_000_000, 720),
    ] {
        let mut q = query(1, max_points);
        q.from += Duration::nanoseconds(100_001);
        q.to = q.from + Duration::nanoseconds(nanos);
        let mut width = 0;
        let series = [
            MachineMetricKind::CpuUtilization,
            MachineMetricKind::RamUsed,
            MachineMetricKind::RamAvailable,
            MachineMetricKind::RamTotal,
        ]
        .into_iter()
        .map(|kind| {
            let unit = if kind == MachineMetricKind::CpuUtilization {
                MachineMetricUnit::Percent
            } else {
                MachineMetricUnit::Bytes
            };
            let buckets = aggregate(&q, q.to, unit, [reading(&q, 0, Some(1.0))]).unwrap();
            width = buckets.width_ms;
            MachineMetricSeries {
                kind,
                gpu_id: None,
                unit,
                current: MachineMetricCurrent {
                    status: MachineMetricStatus::WarmingUp,
                    value: None,
                    sampled_at: None,
                    age_ms: None,
                },
                points: buckets.points,
            }
        })
        .collect();
        let mut response = MachineMetricsResponse {
            schema: CAPABILITY_MACHINE_METRICS.into(),
            server_id: "fixture-machine".into(),
            generated_at: q.to,
            from: q.from,
            to: q.to,
            retained_from: Some(q.from),
            truncated: false,
            sample_interval_ms: 5000,
            bucket_width_ms: width,
            stale_after_ms: 15000,
            logical_cpus: None,
            memory_method: MachineMemoryMethod::Unavailable,
            scope: "os_visible_machine".into(),
            gpus: vec![],
            inventory_truncated: false,
            gpu_status: MachineMetricStatus::NoDevice,
            series,
        };
        response.validate(&q, "fixture-machine", q.to).unwrap();
        let wire = serde_json::to_vec(&response).unwrap();
        MachineMetricsResponse::parse_bounded(&wire, &q, "fixture-machine", q.to).unwrap();
        let expected = (nanos as u64).div_ceil(5_000_000_000);
        assert_eq!(
            response.series[0]
                .points
                .iter()
                .map(|point| u64::from(point.expected_count))
                .sum::<u64>(),
            expected
        );
        assert_eq!(response.series[0].points.last().unwrap().to, q.to);
        assert!(response.series[0].points.len() <= usize::from(max_points));
        // A consumer must reject the old floor-to-ms count for a fractional
        // tail; accepting the source output alone could hide shared rounding.
        if nanos % 5_000_000_000 == 1 {
            let last = response.series[0].points.last_mut().unwrap();
            last.expected_count -= 1;
            assert!(response.validate(&q, "fixture-machine", q.to).is_err());
        }
    }
}

#[test]
fn jittered_distinct_acquisitions_do_not_fail_when_query_origin_moves() {
    let q = query(10, 1);
    let first = Observation {
        epoch: 4,
        tick: 10,
        at: q.from + Duration::milliseconds(1),
        status: MachineMetricStatus::Ok,
        value: Some(10.0),
    };
    let second = Observation {
        epoch: 4,
        tick: 11,
        at: q.from + Duration::milliseconds(4999),
        status: MachineMetricStatus::Ok,
        value: Some(20.0),
    };
    let third = Observation {
        epoch: 4,
        tick: 12,
        at: q.from + Duration::milliseconds(5001),
        status: MachineMetricStatus::Ok,
        value: Some(30.0),
    };
    for shift in [0, -1, 1] {
        let mut shifted = q.clone();
        shifted.from += Duration::milliseconds(shift);
        shifted.to += Duration::milliseconds(shift);
        let forward = aggregate(
            &shifted,
            shifted.to,
            MachineMetricUnit::Percent,
            [first, second, third],
        )
        .unwrap();
        let reversed = aggregate(
            &shifted,
            shifted.to,
            MachineMetricUnit::Percent,
            [third, second, first],
        )
        .unwrap();
        assert_eq!(forward.points, reversed.points);
        assert_eq!(forward.points[0].valid_count, 2);
        assert_eq!(forward.points[0].expected_count, 2);
    }
    let latest_failure = Observation {
        status: MachineMetricStatus::TimedOut,
        value: None,
        ..second
    };
    let result = aggregate(
        &q,
        q.to,
        MachineMetricUnit::Percent,
        [third, latest_failure, first],
    )
    .unwrap();
    assert_eq!(result.points[0].valid_count, 1);
    assert_eq!(result.points[0].mean, Some(30.0));
    assert!(result.points[0].partial);
    assert!(result.points[0]
        .status_counts
        .iter()
        .any(|v| v.status == MachineMetricStatus::TimedOut));
    let bad_old = Observation {
        value: Some(f64::NAN),
        ..first
    };
    assert!(matches!(
        aggregate(&q, q.to, MachineMetricUnit::Percent, [bad_old, second]),
        Err(AggregationError::InvalidObservation)
    ));
    let same_identity = Observation {
        at: third.at,
        ..first
    };
    assert!(matches!(
        aggregate(&q, q.to, MachineMetricUnit::Percent, [first, same_identity]),
        Err(AggregationError::DuplicateAcquisition)
    ));
}
