use super::*;
use crate::machine_metrics::response_encoding::{
    encode_charged, ChargedEncodingError, EncodingError, ENCODING_RESERVATION_BYTES,
};
use chrono::{Duration, TimeZone, Utc};
use treer_protocol::*;

fn fixture() -> (MachineMetricsResponse, MachineMetricsQuery) {
    let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
    let query = MachineMetricsQuery {
        from: now - Duration::seconds(5),
        to: now,
        max_points: 1,
    };
    let response = MachineMetricsResponse {
        schema: CAPABILITY_MACHINE_METRICS.into(),
        server_id: "machine".into(),
        generated_at: now,
        from: query.from,
        to: query.to,
        retained_from: None,
        truncated: false,
        sample_interval_ms: MACHINE_METRICS_SAMPLE_INTERVAL_MS,
        bucket_width_ms: 5_000,
        stale_after_ms: MACHINE_METRICS_STALE_AFTER_MS,
        logical_cpus: None,
        memory_method: MachineMemoryMethod::Unavailable,
        scope: "os_visible_machine".into(),
        gpus: Vec::new(),
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
            current: MachineMetricCurrent {
                status: MachineMetricStatus::WarmingUp,
                value: None,
                sampled_at: None,
                age_ms: None,
            },
            points: Vec::new(),
        })
        .collect(),
    };
    (response, query)
}

fn dense_fixture(status_rich: bool) -> (MachineMetricsResponse, MachineMetricsQuery) {
    let (mut response, mut query) = fixture();
    query.from = query.to - Duration::hours(24);
    query.max_points = 720;
    response.from = query.from;
    response.bucket_width_ms = 120_000;
    for series in &mut response.series {
        series.points = (0..720)
            .map(|index| {
                let from = query.from + Duration::seconds(index * 120);
                let statuses = if status_rich {
                    &[
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
                    ][..]
                } else {
                    &[MachineMetricStatus::WarmingUp][..]
                };
                MachineMetricBucket {
                    from,
                    to: from + Duration::seconds(120),
                    expected_count: 24,
                    valid_count: 0,
                    status_counts: statuses
                        .iter()
                        .map(|&status| MachineMetricStatusCount {
                            status,
                            count: if status_rich { 1 } else { 24 },
                        })
                        .collect(),
                    first_sample_at: None,
                    last_sample_at: None,
                    mean: None,
                    min: None,
                    max: None,
                    partial: true,
                }
            })
            .collect();
    }
    response
        .validate_model(&query, "machine", query.to)
        .unwrap();
    (response, query)
}

#[test]
fn slow_reader_keeps_encoded_bytes_credit_and_revoked_slot_until_last_drop() {
    let budget = MemoryBudget::isolated();
    let baseline = budget
        .try_build(AGGREGATE_BYTES - ENCODING_RESERVATION_BYTES, || {
            Ok::<_, ()>(())
        })
        .unwrap()
        .unwrap();
    let assembly = budget.try_assembly().unwrap();
    let (response, query) = fixture();
    let output = Arc::new(
        encode_charged(
            assembly.clone(),
            &response,
            &query,
            "machine",
            query.to,
            &mut || {
                assert_eq!(budget.charged_bytes(), AGGREGATE_BYTES);
                false
            },
        )
        .unwrap(),
    );
    assert_eq!(output.as_bytes(), serde_json::to_vec(&response).unwrap());
    assert_eq!(
        MachineMetricsResponse::parse_bounded(output.as_bytes(), &query, "machine", query.to)
            .unwrap(),
        response
    );
    let slow_reader = output.clone();
    assert!(!slow_reader.is_revoked());
    assembly.revoke();
    drop(assembly);
    drop(output);
    // A writer can still observe revocation after the worker's guard is gone.
    assert!(slow_reader.is_revoked());
    // Reading part of an output does not free the rest or permit new assembly.
    assert!(!slow_reader.as_bytes()[..8].is_empty());
    assert_eq!(budget.charged_bytes(), AGGREGATE_BYTES);
    assert!(matches!(
        budget.try_assembly(),
        Err(BudgetError::AssemblyBusy)
    ));
    assert!(matches!(
        budget.try_build(1, || -> Result<(), ()> {
            panic!("slow reader lost credit")
        }),
        Err(BudgetError::Exhausted)
    ));
    drop(slow_reader);
    assert_eq!(
        budget.charged_bytes(),
        AGGREGATE_BYTES - ENCODING_RESERVATION_BYTES
    );
    assert!(budget.try_assembly().is_ok());
    drop(baseline);
    assert_eq!(budget.charged_bytes(), 0);
}

#[test]
fn exhausted_encoding_never_enters_encoder_and_releases_assembly() {
    let budget = MemoryBudget::isolated();
    let baseline = budget
        .try_build(AGGREGATE_BYTES, || Ok::<_, ()>(()))
        .unwrap()
        .unwrap();
    let (response, query) = fixture();
    assert!(matches!(
        encode_charged(
            budget.try_assembly().unwrap(),
            &response,
            &query,
            "machine",
            query.to,
            &mut || panic!("encoder entered without reserved credit")
        ),
        Err(ChargedEncodingError::Budget(BudgetError::Exhausted))
    ));
    assert_eq!(budget.charged_bytes(), AGGREGATE_BYTES);
    assert!(budget.try_assembly().is_ok());
    drop(baseline);
}

#[test]
fn encoding_failures_cancellation_revocation_and_unwind_restore_live_baseline() {
    let budget = MemoryBudget::isolated();
    let baseline = budget.try_build(128, || Ok::<_, ()>(())).unwrap().unwrap();
    let (response, query) = fixture();
    let mut checks = 0;
    let output = encode_charged(
        budget.try_assembly().unwrap(),
        &response,
        &query,
        "machine",
        query.to,
        &mut || {
            checks += 1;
            false
        },
    )
    .unwrap();
    drop(output);
    // The last callback is after compaction, not merely after serialization.
    for stop_at in [1, 2, checks / 2, checks] {
        let mut calls = 0;
        assert!(matches!(
            encode_charged(
                budget.try_assembly().unwrap(),
                &response,
                &query,
                "machine",
                query.to,
                &mut || {
                    calls += 1;
                    calls == stop_at
                }
            ),
            Err(ChargedEncodingError::Encoding(EncodingError::Cancelled))
        ));
        assert_eq!(budget.charged_bytes(), 128);
    }
    assert!(matches!(
        encode_charged(
            budget.try_assembly().unwrap(),
            &response,
            &query,
            "wrong-machine",
            query.to,
            &mut || false
        ),
        Err(ChargedEncodingError::Encoding(
            EncodingError::InvalidResponse
        ))
    ));
    assert_eq!(budget.charged_bytes(), 128);
    let (too_large, large_query) = dense_fixture(true);
    assert!(matches!(
        encode_charged(
            budget.try_assembly().unwrap(),
            &too_large,
            &large_query,
            "machine",
            large_query.to,
            &mut || false
        ),
        Err(ChargedEncodingError::Encoding(EncodingError::TooLarge))
    ));
    assert_eq!(budget.charged_bytes(), 128);
    let assembly = budget.try_assembly().unwrap();
    let observer = assembly.clone();
    assert!(matches!(
        encode_charged(
            assembly,
            &response,
            &query,
            "machine",
            query.to,
            &mut || {
                observer.revoke();
                false
            }
        ),
        Err(ChargedEncodingError::Encoding(EncodingError::Cancelled))
    ));
    assert_eq!(budget.charged_bytes(), 128);
    assert!(matches!(
        budget.try_assembly(),
        Err(BudgetError::AssemblyBusy)
    ));
    drop(observer);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        encode_charged(
            budget.try_assembly().unwrap(),
            &response,
            &query,
            "machine",
            query.to,
            &mut || panic!("injected encoder callback panic"),
        )
    }))
    .is_err());
    assert_eq!(budget.charged_bytes(), 128);
    assert!(budget.try_assembly().is_ok());
    drop(baseline);
    assert_eq!(budget.charged_bytes(), 0);
}

#[cfg(feature = "metrics-allocation-measurement")]
#[test]
fn charged_encoding_measured_live_heap_stays_within_payload_reservation() {
    let budget = MemoryBudget::isolated();
    let assembly = budget.try_assembly().unwrap();
    for status_rich in [false, true] {
        let (response, query) = dense_fixture(status_rich);
        // The typed input is a previous stage's obligation, deliberately built
        // before this encoder-only measurement. Test oracle also stays outside.
        let expected = serde_json::to_vec(&response).unwrap();
        assert!(expected.len() > MACHINE_METRICS_MAX_RESPONSE_BYTES / 2);
        assert_eq!(
            expected.len() > MACHINE_METRICS_MAX_RESPONSE_BYTES,
            status_rich
        );
        let measured = allocation_counter::measure(|| {
            let output = encode_charged(
                assembly.clone(),
                &response,
                &query,
                "machine",
                query.to,
                &mut || false,
            );
            if status_rich {
                assert!(matches!(
                    output,
                    Err(ChargedEncodingError::Encoding(EncodingError::TooLarge))
                ));
            } else {
                let output = output.unwrap();
                assert_eq!(output.as_bytes(), expected);
                std::hint::black_box(output.as_bytes());
                drop(output);
            }
        });
        println!(
            "charged_encoding status_rich={status_rich} peak={} total={} final={}",
            measured.bytes_max, measured.bytes_total, measured.bytes_current
        );
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(measured.count_current, 0);
        assert!(measured.bytes_max >= MACHINE_METRICS_MAX_RESPONSE_BYTES as u64);
        assert!(measured.bytes_max <= ENCODING_RESERVATION_BYTES as u64);
        assert_eq!(budget.charged_bytes(), 0);
    }
}
