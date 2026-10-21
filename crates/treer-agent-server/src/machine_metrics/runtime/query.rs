use super::*;
use chrono::Duration as Span;
use std::collections::BTreeMap;

fn fields(
    sample: &Sample,
    key: (MachineMetricKind, Option<&str>),
) -> (MachineMetricStatus, Option<f64>) {
    use MachineMetricKind::*;
    let value = match key {
        (CpuUtilization, None) => sample.cpu,
        (RamUsed, None) => sample.used,
        (RamAvailable, None) => sample.available,
        (RamTotal, None) => sample.total,
        (kind, Some(id)) => {
            let Some(gpu) = sample.gpus.iter().find(|gpu| gpu.id == id) else {
                return (
                    if matches!(
                        sample.gpu_status,
                        MachineMetricStatus::Ok | MachineMetricStatus::NoDevice
                    ) {
                        MachineMetricStatus::DeviceAbsent
                    } else {
                        sample.gpu_status
                    },
                    None,
                );
            };
            match kind {
                GpuUtilization => gpu.utilization,
                GpuMemoryUsed => gpu.used,
                GpuMemoryTotal => gpu.total,
                _ => None,
            }
        }
        _ => None,
    };
    (
        if value.is_some() {
            MachineMetricStatus::Ok
        } else if key.0 == CpuUtilization {
            MachineMetricStatus::WarmingUp
        } else {
            MachineMetricStatus::CollectionFailed
        },
        value,
    )
}

fn validate(sample: &Sample) -> Result<(), ProtocolError> {
    let bad = || failure("machine_metrics_invalid_response");
    if sample.gpus.len() > MACHINE_METRICS_MAX_GPUS
        || sample.cpus.is_some_and(|n| n == 0 || n > 65_535)
    {
        return Err(bad());
    }
    if sample
        .cpu
        .is_some_and(|n| !n.is_finite() || !(0.0..=100.0).contains(&n))
    {
        return Err(bad());
    }
    for number in [sample.used, sample.available, sample.total]
        .into_iter()
        .chain(sample.gpus.iter().flat_map(|gpu| [gpu.used, gpu.total]))
    {
        if number.is_some_and(|n| {
            !n.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&n) || n.fract() != 0.0
        }) {
            return Err(bad());
        }
    }
    if sample.used.zip(sample.total).is_some_and(|(u, t)| u > t)
        || sample
            .available
            .zip(sample.total)
            .is_some_and(|(a, t)| a > t)
        || sample
            .used
            .zip(sample.available)
            .zip(sample.total)
            .is_some_and(|((u, a), t)| u != t - a)
    {
        return Err(bad());
    }
    let mut previous: Option<&str> = None;
    for gpu in &sample.gpus {
        MachineMetricsGpu {
            id: gpu.id.clone(),
            name: gpu.name.clone(),
            present: true,
        }
        .validate_descriptor()
        .map_err(|_| bad())?;
        if previous.is_some_and(|id| id >= gpu.id.as_str())
            || gpu
                .utilization
                .is_some_and(|v| !v.is_finite() || !(0.0..=100.0).contains(&v))
            || gpu.used.zip(gpu.total).is_some_and(|(u, t)| u > t)
        {
            return Err(bad());
        }
        previous = Some(&gpu.id);
    }
    Ok(())
}

pub(super) fn assemble(
    store: &store::Store,
    server_id: &str,
    query: &MachineMetricsQuery,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<u8>, ProtocolError> {
    let now = Utc::now();
    let snapshot = store.snapshot()?;
    let truncated = store
        .capacity_floor()
        .map_err(|_| failure("machine_metrics_invalid_response"))?
        > query.from.timestamp();
    let mut latest = None;
    let mut retained_from = None;
    let mut devices = BTreeMap::<String, String>::new();
    store.scan_snapshot(
        &snapshot,
        now,
        |sample| {
            validate(&sample)?;
            retained_from.get_or_insert(sample.at);
            if sample.at >= query.from && sample.at < query.to {
                for gpu in &sample.gpus {
                    if !devices.contains_key(&gpu.id) && devices.len() >= MACHINE_METRICS_MAX_GPUS {
                        return Err(failure("machine_metrics_busy"));
                    }
                    devices.insert(gpu.id.clone(), gpu.name.clone());
                }
            }
            latest = Some(sample);
            Ok(())
        },
        &cancelled,
    )?;
    if let Some(sample) = &latest {
        for gpu in &sample.gpus {
            if !devices.contains_key(&gpu.id) && devices.len() >= MACHINE_METRICS_MAX_GPUS {
                return Err(failure("machine_metrics_busy"));
            }
            devices.insert(gpu.id.clone(), gpu.name.clone());
        }
    }
    let count = 4 + devices.len() * 3;
    // Wire-cap-aware downsampling, not series truncation: all device/field keys
    // remain. The final capped encoder still refuses the whole response if the
    // status-rich result cannot fit, before any byte is published.
    let max_points = usize::from(query.max_points)
        .min(MACHINE_METRICS_MAX_TOTAL_BUCKETS / count)
        .min(2000 / count)
        .max(1);
    let slots = machine_metrics_expected_samples(query.to - query.from)
        .ok_or_else(|| failure("machine_metrics_invalid_range"))?;
    let width_ms = u64::from(slots).div_ceil(max_points as u64) * 5000;
    let mut keys: Vec<_> = [
        MachineMetricKind::CpuUtilization,
        MachineMetricKind::RamUsed,
        MachineMetricKind::RamAvailable,
        MachineMetricKind::RamTotal,
    ]
    .into_iter()
    .map(|kind| (kind, None))
    .collect();
    for id in devices.keys() {
        for kind in [
            MachineMetricKind::GpuUtilization,
            MachineMetricKind::GpuMemoryUsed,
            MachineMetricKind::GpuMemoryTotal,
        ] {
            keys.push((kind, Some(id.clone())));
        }
    }
    let mut series = Vec::with_capacity(count);
    for (kind, id) in keys {
        if cancelled() {
            return Err(failure("machine_metrics_timeout"));
        }
        let unit = if matches!(
            kind,
            MachineMetricKind::CpuUtilization | MachineMetricKind::GpuUtilization
        ) {
            MachineMetricUnit::Percent
        } else {
            MachineMetricUnit::Bytes
        };
        let (status, value) = latest
            .as_ref()
            .map(|sample| fields(sample, (kind, id.as_deref())))
            .unwrap_or((MachineMetricStatus::WarmingUp, None));
        let age_ms = latest
            .as_ref()
            .map(|sample| (now - sample.at).num_milliseconds() as u64);
        let stale = status == MachineMetricStatus::Ok
            && age_ms.is_some_and(|age| age > MACHINE_METRICS_STALE_AFTER_MS);
        let mut points = Vec::with_capacity(max_points);
        let mut from = query.from;
        while from < query.to {
            let to = (from + Span::milliseconds(width_ms as i64)).min(query.to);
            points.push(MachineMetricBucket {
                from,
                to,
                expected_count: machine_metrics_expected_samples(to - from).unwrap(),
                valid_count: 0,
                status_counts: Vec::new(),
                first_sample_at: None,
                last_sample_at: None,
                mean: None,
                min: None,
                max: None,
                partial: true,
            });
            from = to;
        }
        series.push(MachineMetricSeries {
            kind,
            gpu_id: id,
            unit,
            current: MachineMetricCurrent {
                status: if stale {
                    MachineMetricStatus::Stale
                } else {
                    status
                },
                value: if stale { None } else { value },
                sampled_at: latest.as_ref().map(|sample| sample.at),
                age_ms,
            },
            points,
        });
    }
    let mut apply = |sample: Sample| -> Result<(), ProtocolError> {
        let index = (sample.at - query.from).num_milliseconds() as u64 / width_ms;
        for series in &mut series {
            let (status, value) = fields(&sample, (series.kind, series.gpu_id.as_deref()));
            let bucket = &mut series.points[index as usize];
            if let Some(entry) = bucket
                .status_counts
                .iter_mut()
                .find(|entry| entry.status == status)
            {
                entry.count += 1;
            } else {
                bucket
                    .status_counts
                    .push(MachineMetricStatusCount { status, count: 1 });
            }
            if let Some(value) = value {
                bucket.valid_count += 1;
                bucket.mean = Some(bucket.mean.map_or(value, |mean| {
                    mean + (value - mean) / f64::from(bucket.valid_count)
                }));
                bucket.min = Some(bucket.min.map_or(value, |min| min.min(value)));
                bucket.max = Some(bucket.max.map_or(value, |max| max.max(value)));
                bucket.first_sample_at.get_or_insert(sample.at);
                bucket.last_sample_at = Some(sample.at);
            }
        }
        Ok(())
    };
    let mut pending: Option<(u64, Sample)> = None;
    store.scan_snapshot(
        &snapshot,
        now,
        |sample| {
            validate(&sample)?;
            if sample.at < query.from || sample.at >= query.to {
                return Ok(());
            }
            let slot = (sample.at - query.from).num_milliseconds() as u64 / 5000;
            if pending.as_ref().is_some_and(|(old, _)| *old != slot) {
                apply(pending.take().unwrap().1)?;
            }
            pending = Some((slot, sample));
            Ok(())
        },
        &cancelled,
    )?;
    if let Some((_, sample)) = pending {
        apply(sample)?;
    }
    for field in &mut series {
        for bucket in &mut field.points {
            bucket.partial = truncated || bucket.valid_count != bucket.expected_count;
            bucket.status_counts.sort_by_key(|entry| entry.status as u8);
        }
    }
    let current = latest.as_ref();
    let gpus: Vec<_> = devices
        .into_iter()
        .map(|(id, name)| {
            let present = current.is_some_and(|sample| sample.gpus.iter().any(|gpu| gpu.id == id));
            MachineMetricsGpu { id, name, present }
        })
        .collect();
    // Unknown current inventory must not be reported as known absence.
    if gpus.iter().any(|gpu| !gpu.present)
        && current.is_none_or(|s| {
            !matches!(
                s.gpu_status,
                MachineMetricStatus::Ok | MachineMetricStatus::NoDevice
            )
        })
    {
        return Err(failure("machine_metrics_invalid_response"));
    }
    let response = MachineMetricsResponse {
        schema: CAPABILITY_MACHINE_METRICS.into(),
        server_id: server_id.into(),
        generated_at: now,
        from: query.from,
        to: query.to,
        retained_from,
        truncated,
        sample_interval_ms: 5000,
        bucket_width_ms: width_ms,
        stale_after_ms: MACHINE_METRICS_STALE_AFTER_MS,
        logical_cpus: current.and_then(|s| s.cpus),
        memory_method: current.map_or(MachineMemoryMethod::Unavailable, |s| s.memory_method),
        scope: "os_visible_machine".into(),
        gpus,
        inventory_truncated: false,
        gpu_status: current.map_or(MachineMetricStatus::WarmingUp, |s| s.gpu_status),
        series,
    };
    super::super::response_encoding::encode_bounded(&response, query, server_id, now, &mut || {
        cancelled()
    })
    .map_err(|_| failure("machine_metrics_invalid_response"))
}
