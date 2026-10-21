use super::*;
use std::{fs, io::Write};

pub(super) struct Fixture(pub(super) PathBuf);
impl Fixture {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("treer-metrics-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        treer_platform::secure_directory(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sample(at: DateTime<Utc>, gpus: usize) -> Sample {
    Sample {
        at,
        cpu: Some(25.0),
        cpus: Some(8),
        used: Some(512.0),
        available: Some(512.0),
        total: Some(1024.0),
        memory_method: MachineMemoryMethod::MacosAvailableEstimate,
        gpu_status: if gpus == 0 {
            MachineMetricStatus::NoDevice
        } else {
            MachineMetricStatus::Ok
        },
        gpus: (0..gpus)
            .map(|index| Gpu {
                id: format!("{index:064x}"),
                name: format!("NVIDIA {index}"),
                utilization: Some(50.0),
                used: Some(1024.0),
                total: Some(2048.0),
            })
            .collect(),
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serialize fixtures using the production ledger.
async fn disk_reopen_query_binds_machine_and_keeps_credit_through_slow_reader() {
    let _test_guard = super::super::memory_budget::test_machine_lock();
    let fixture = Fixture::new();
    let store = store::Store::open(fixture.0.clone()).unwrap();
    let now = Utc::now();
    for offset in [20, 15, 5] {
        store
            .append(&sample(now - chrono::Duration::seconds(offset), 2))
            .unwrap();
    }
    drop(store);
    let metrics = Metrics::open(fixture.0.clone()).unwrap();
    let query = MachineMetricsQuery {
        from: now - chrono::Duration::seconds(25),
        to: now,
        max_points: 5,
    };
    let baseline = MemoryBudget::machine().charged_bytes();
    let reply = metrics
        .query("source-machine".into(), query.clone())
        .await
        .unwrap();
    let response =
        MachineMetricsResponse::parse_bounded(reply.bytes(), &query, "source-machine", Utc::now())
            .unwrap();
    assert_eq!(response.series.len(), 10);
    assert_eq!(
        response.series[0]
            .points
            .iter()
            .map(|b| b.valid_count)
            .sum::<u32>(),
        3
    );
    assert!(response.series[0].points.iter().any(|b| b.partial));
    assert_eq!(
        MemoryBudget::machine().charged_bytes(),
        baseline + QUERY_CREDIT
    );
    assert!(metrics
        .query("source-machine".into(), query.clone())
        .await
        .is_err());
    let other = Metrics::open(fixture.0.clone()).unwrap();
    assert!(other.store.query_lease().is_err());
    metrics.store.clear().unwrap();
    assert!(reply.revoked());
    drop(reply);
    assert_eq!(MemoryBudget::machine().charged_bytes(), baseline);
    assert!(metrics.store.query_lease().is_ok());
}

#[test]
fn disk_expiry_capacity_corruption_disable_and_cancel_fail_whole() {
    let fixture = Fixture::new();
    let store = store::Store::open(fixture.0.clone()).unwrap();
    let now = Utc::now();
    store
        .append(&sample(now - chrono::Duration::hours(25), 0))
        .unwrap();
    store
        .append(&sample(now - chrono::Duration::seconds(5), 0))
        .unwrap();
    let query = MachineMetricsQuery {
        from: now - chrono::Duration::minutes(15),
        to: now,
        max_points: 20,
    };
    let mut seen = 0;
    store
        .scan(
            now,
            |_| {
                seen += 1;
                Ok(())
            },
            &|| false,
        )
        .unwrap();
    assert_eq!(seen, 1);
    assert_eq!(
        fs::read_dir(&fixture.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|v| v == "samples"))
            .count(),
        1
    );
    assert!(query::assemble(&store, "source", &query, || true).is_err());
    let sample_file = fs::read_dir(&fixture.0)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|v| v == "samples"))
        .unwrap()
        .path();
    fs::OpenOptions::new()
        .append(true)
        .open(sample_file)
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    assert!(query::assemble(&store, "source", &query, || false).is_err());
    fs::write(fixture.0.join("disabled"), b"").unwrap();
    store.append(&sample(now, 0)).unwrap();
    assert!(query::assemble(&store, "source", &query, || false).is_err());
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
#[tokio::test]
#[allow(clippy::await_holding_lock)] // The sampler owns real process-wide credit.
async fn actual_os_sampler_to_disk_and_wire_query() {
    let _test_guard = super::super::memory_budget::test_machine_lock();
    let fixture = Fixture::new();
    let metrics = Metrics::open(fixture.0.clone()).unwrap();
    metrics.start();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut seen = 0;
        metrics
            .store
            .scan(
                Utc::now(),
                |sample| {
                    assert!(sample.total.is_some_and(|value| value > 0.0));
                    assert!(sample.cpus.is_some_and(|value| value > 0));
                    if cfg!(target_os = "macos") {
                        assert_eq!(sample.gpu_status, MachineMetricStatus::UnsupportedPlatform);
                    }
                    seen += 1;
                    Ok(())
                },
                &|| false,
            )
            .unwrap();
        if seen >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "no real OS samples");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let now = Utc::now();
    let query = MachineMetricsQuery {
        from: now - chrono::Duration::minutes(15),
        to: now,
        max_points: 360,
    };
    let reply = metrics
        .query("real-machine".into(), query.clone())
        .await
        .unwrap();
    let response =
        MachineMetricsResponse::parse_bounded(reply.bytes(), &query, "real-machine", Utc::now())
            .unwrap();
    assert!(response.series.len() >= 4);
    assert_eq!(
        response.memory_method,
        if cfg!(target_os = "linux") {
            MachineMemoryMethod::LinuxMemAvailable
        } else if cfg!(target_os = "windows") {
            MachineMemoryMethod::WindowsPhysicalAvailable
        } else {
            MachineMemoryMethod::MacosAvailableEstimate
        }
    );
    assert_eq!(response.series[0].current.status, MachineMetricStatus::Ok);
    assert!(response.series[0].current.value.is_some());
    println!("actual_metrics_entry os={} samples>=2 memory={:?} cpu_ok=true disk_reopen=pending gpu={:?}",std::env::consts::OS,response.memory_method,response.gpu_status);
    drop(reply);
    drop(metrics);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let reopened = Metrics::open(fixture.0.clone()).unwrap();
    assert!(reopened.query("real-machine".into(), query).await.is_ok());
    println!(
        "actual_metrics_entry os={} disk_reopen=ok bounded_wire=ok; native_NVML_not_proven",
        std::env::consts::OS
    );
}

#[cfg(unix)]
#[test]
fn store_refuses_symlink_and_nonprivate_root() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new();
    let alias = fixture.0.join("alias");
    symlink(&fixture.0, &alias).unwrap();
    assert!(store::Store::open(alias).is_err());
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(store::Store::open(fixture.0.clone()).is_err());
}

#[cfg(feature = "metrics-allocation-measurement")]
#[test]
#[ignore = "isolated complete disk-query allocator/RSS measurement"]
fn real_disk_24h_eight_gpu_complete_query_peak() {
    for gpus in [0, 1, 8, 32] {
        let fixture = Fixture::new();
        let store = store::Store::open(fixture.0.clone()).unwrap();
        let now = Utc::now();
        for index in 0..17280 {
            store
                .append(&sample(
                    now - chrono::Duration::seconds((17280 - index) * 5),
                    gpus,
                ))
                .unwrap();
        }
        let query = MachineMetricsQuery {
            from: now - chrono::Duration::hours(24),
            to: now,
            max_points: 720,
        };
        let mut wire_bytes = 0;
        let rss = || {
            std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
        };
        let baseline_rss = rss();
        let info = allocation_counter::measure(|| {
            // Actual selective system refresh plus source assembly and overlapping
            // Controller/Proxy/HTTP representations. GPU records are deterministic
            // fixtures; native driver allocations require separate native evidence.
            let mut system = sysinfo::System::new();
            system.refresh_cpu_usage();
            system.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
            let bytes = query::assemble(&store, "source", &query, || false).unwrap();
            wire_bytes = bytes.len();
            let frame = format!("{{\"type\":\"command_result\",\"result\":{{\"command_id\":\"measurement\",\"data\":{}}}}}", std::str::from_utf8(&bytes).unwrap());
            let envelope: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let response: MachineMetricsResponse =
                serde_json::from_value(envelope["result"]["data"].clone()).unwrap();
            response.validate(&query, "source", Utc::now()).unwrap();
            let http = serde_json::to_vec(&response).unwrap();
            assert!(http.len() <= MACHINE_METRICS_MAX_RESPONSE_BYTES);
            assert_eq!(response.series.len(), 4 + gpus * 3);
            let held = http.clone();
            assert_eq!(held, http);
        });
        eprintln!("metrics_composed gpus={gpus} input_samples=17280 wire_bytes={wire_bytes} peak={} total={} final={} baseline_rss_kib={baseline_rss:?} final_rss_kib={:?}",info.bytes_max,info.bytes_total,info.bytes_current,rss());
        assert!(info.bytes_max < QUERY_CREDIT as u64);
        assert_eq!(info.bytes_current, 0);
    }
}
