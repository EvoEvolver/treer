//! Executable synthetic contract examples. No machine access or persistence.
//! cargo run -p treer-protocol --example machine_metrics_fixtures
use chrono::{DateTime, Duration};
use treer_protocol::*;

fn base() -> (MachineMetricsQuery, MachineMetricsResponse) {
    let now = DateTime::from_timestamp(1_792_500_000, 0).unwrap();
    let query = MachineMetricsQuery {
        from: now - Duration::minutes(1),
        to: now,
        max_points: 360,
    };
    let gpu_id = "a".repeat(64); // Opaque mock, not a hardware identifier.
    let series = [
        (MachineMetricKind::CpuUtilization, 12.5),
        (MachineMetricKind::RamUsed, 6_000_000_000.0),
        (MachineMetricKind::RamAvailable, 10_000_000_000.0),
        (MachineMetricKind::RamTotal, 16_000_000_000.0),
        (MachineMetricKind::GpuUtilization, 25.0),
        (MachineMetricKind::GpuMemoryUsed, 2_000_000_000.0),
        (MachineMetricKind::GpuMemoryTotal, 8_000_000_000.0),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (kind, value))| MachineMetricSeries {
        kind,
        gpu_id: (index >= 4).then(|| gpu_id.clone()),
        unit: if index == 0 || index == 4 {
            MachineMetricUnit::Percent
        } else {
            MachineMetricUnit::Bytes
        },
        current: MachineMetricCurrent {
            status: MachineMetricStatus::Ok,
            value: Some(value),
            sampled_at: Some(now - Duration::seconds(2)),
            age_ms: Some(2000),
        },
        points: vec![MachineMetricBucket {
            from: query.from,
            to: query.to,
            expected_count: 12,
            valid_count: 12,
            status_counts: vec![MachineMetricStatusCount {
                status: MachineMetricStatus::Ok,
                count: 12,
            }],
            first_sample_at: Some(query.from),
            last_sample_at: Some(query.to - Duration::seconds(5)),
            mean: Some(value),
            min: Some(value),
            max: Some(value),
            partial: false,
        }],
    })
    .collect();
    let response = MachineMetricsResponse {
        schema: CAPABILITY_MACHINE_METRICS.into(),
        server_id: "fixture-machine".into(),
        generated_at: now,
        from: query.from,
        to: query.to,
        retained_from: Some(query.from),
        truncated: false,
        sample_interval_ms: 5000,
        bucket_width_ms: 60_000,
        stale_after_ms: 15_000,
        logical_cpus: Some(8),
        memory_method: MachineMemoryMethod::LinuxMemAvailable,
        scope: "os_visible_machine".into(),
        gpus: vec![MachineMetricsGpu {
            id: gpu_id,
            name: "Synthetic NVIDIA device".into(),
            present: true,
        }],
        inventory_truncated: false,
        gpu_status: MachineMetricStatus::Ok,
        series,
    };
    (query, response)
}

fn examples() -> Vec<(&'static str, MachineMetricsResponse)> {
    let (_, healthy) = base();
    let mut gap = healthy.clone();
    gap.series[0].points[0].valid_count = 6;
    gap.series[0].points[0].status_counts[0].count = 6;
    gap.series[0].points[0].partial = true;
    let mut failure = healthy.clone();
    failure.series[4].current.status = MachineMetricStatus::CollectionFailed;
    failure.series[4].current.value = None;
    let bucket = &mut failure.series[4].points[0];
    bucket.valid_count = 0;
    bucket.status_counts[0].status = MachineMetricStatus::CollectionFailed;
    bucket.first_sample_at = None;
    bucket.last_sample_at = None;
    bucket.mean = None;
    bucket.min = None;
    bucket.max = None;
    bucket.partial = true;
    let mut removed = healthy.clone();
    removed.gpus[0].present = false;
    removed.gpu_status = MachineMetricStatus::NoDevice;
    for series in &mut removed.series[4..] {
        series.current.status = MachineMetricStatus::DeviceAbsent;
        series.current.value = None;
    }
    let mut empty = healthy.clone();
    empty.retained_from = None;
    empty.gpus.clear();
    empty.series.truncate(4);
    empty.gpu_status = MachineMetricStatus::WarmingUp;
    for series in &mut empty.series {
        series.current = MachineMetricCurrent {
            status: MachineMetricStatus::WarmingUp,
            value: None,
            sampled_at: None,
            age_ms: None,
        };
        series.points.clear();
    }
    let mut stale = healthy.clone();
    stale.series[4].current = MachineMetricCurrent {
        status: MachineMetricStatus::Stale,
        value: None,
        sampled_at: Some(stale.generated_at - Duration::seconds(20)),
        age_ms: Some(20_000),
    };
    vec![
        ("healthy", healthy),
        ("partial_gap", gap),
        ("gpu_field_failure", failure),
        ("removed_gpu", removed),
        ("empty_new_collector", empty),
        ("stale_gpu_field", stale),
    ]
}

fn main() {
    let (query, _) = base();
    let fixtures: Vec<_> = examples()
        .into_iter()
        .map(|(name, response)| {
            response
                .validate(&query, "fixture-machine", response.generated_at)
                .unwrap();
            serde_json::json!({"case": name, "query": query, "response": response})
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&fixtures).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_synthetic_states_pass_bounded_validation() {
        let (query, _) = base();
        for (_, response) in examples() {
            let bytes = serde_json::to_vec(&response).unwrap();
            let parsed = MachineMetricsResponse::parse_bounded(
                &bytes,
                &query,
                "fixture-machine",
                response.generated_at,
            )
            .unwrap();
            assert_eq!(parsed, response);
        }
    }

    #[test]
    fn untrusted_responses_refuse_binding_counts_time_values_and_identity() {
        let (query, valid) = base();
        type Mutation = Box<dyn Fn(&mut MachineMetricsResponse)>;
        let rejects: Vec<Mutation> = vec![
            Box::new(|r| r.server_id = "another-machine".into()),
            Box::new(|r| r.from -= Duration::seconds(1)),
            Box::new(|r| r.generated_at += Duration::seconds(16)),
            Box::new(|r| r.retained_from = Some(r.generated_at - Duration::hours(25))),
            Box::new(|r| r.series[0].current.value = Some(f64::NAN)),
            Box::new(|r| r.series[0].current.value = Some(101.0)),
            Box::new(|r| r.series[0].current.age_ms = Some(20_000)),
            Box::new(|r| {
                r.series[0].current.sampled_at = Some(r.generated_at + Duration::seconds(1))
            }),
            Box::new(|r| r.series[0].points[0].valid_count = 13),
            Box::new(|r| r.series[0].points[0].status_counts[0].count = u32::MAX),
            Box::new(|r| {
                let point = &mut r.series[0].points[0];
                point.valid_count = 6;
                point.status_counts[0].count = 6;
                point.partial = false;
            }),
            Box::new(|r| r.series[0].points[0].to += Duration::seconds(1)),
            Box::new(|r| r.series[0].points[0].mean = Some(f64::INFINITY)),
            Box::new(|r| r.series[0].points[0].last_sample_at = Some(r.to)),
            Box::new(|r| r.series[1].current.value = Some(17_000_000_000.0)),
            Box::new(|r| r.series[1].unit = MachineMetricUnit::Percent),
            Box::new(|r| r.series[1].kind = MachineMetricKind::RamAvailable),
            Box::new(|r| r.series[4].gpu_id = Some("foreign".repeat(10))),
            Box::new(|r| r.gpus[0].id = "GPU-raw-uuid".into()),
            Box::new(|r| r.gpus[0].name = "x".repeat(129)),
            Box::new(|r| r.gpus[0].name = "line\nbreak".into()),
            Box::new(|r| r.gpus[0].present = false),
            Box::new(|r| r.gpus = vec![r.gpus[0].clone(); 33]),
            Box::new(|r| r.series[0].points = vec![r.series[0].points[0].clone(); 721]),
        ];
        for (index, mutate) in rejects.iter().enumerate() {
            let mut response = valid.clone();
            mutate(&mut response);
            assert!(
                response
                    .validate(&query, "fixture-machine", valid.generated_at)
                    .is_err(),
                "rejected control {index}"
            );
        }
        // Bound raw bytes, including ignored extension fields, before parsing.
        let oversized = vec![b' '; MACHINE_METRICS_MAX_RESPONSE_BYTES + 1];
        assert!(MachineMetricsResponse::parse_bounded(
            &oversized,
            &query,
            "fixture-machine",
            valid.generated_at
        )
        .is_err());
        let wire = serde_json::to_vec(&valid).unwrap();
        assert!(MachineMetricsResponse::parse_bounded(
            &wire[..wire.len() - 1],
            &query,
            "fixture-machine",
            valid.generated_at
        )
        .is_err());
    }
}
