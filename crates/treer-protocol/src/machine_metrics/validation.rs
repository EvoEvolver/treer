use super::*;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineMetricsValidationError;

type Result<T> = std::result::Result<T, MachineMetricsValidationError>;

fn require(value: bool) -> Result<()> {
    value.then_some(()).ok_or(MachineMetricsValidationError)
}

fn number(value: f64, unit: MachineMetricUnit) -> bool {
    value.is_finite()
        && value >= 0.0
        && match unit {
            MachineMetricUnit::Percent => value <= 100.0,
            MachineMetricUnit::Bytes => value <= 9_007_199_254_740_991.0,
        }
}

impl MachineMetricKind {
    fn gpu(self) -> bool {
        matches!(
            self,
            Self::GpuUtilization | Self::GpuMemoryUsed | Self::GpuMemoryTotal
        )
    }

    fn unit(self) -> MachineMetricUnit {
        match self {
            Self::CpuUtilization | Self::GpuUtilization => MachineMetricUnit::Percent,
            _ => MachineMetricUnit::Bytes,
        }
    }
}

impl MachineMetricsGpu {
    /// Descriptor shape only; does not establish collector provenance/presence.
    pub fn validate_descriptor(&self) -> Result<()> {
        require(
            self.id.len() == 64
                && self
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && !self.name.is_empty()
                && self.name.len() <= 128
                && !self.name.chars().any(|c| {
                    c.is_control()
                        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}' | '\u{feff}')
                }),
        )
    }
}

impl MachineMetricsResponse {
    /// Validate untrusted transport bytes before exposing a response. The raw
    /// frame is bounded before parsing, including ignored extension fields.
    pub fn parse_bounded(
        bytes: &[u8],
        query: &MachineMetricsQuery,
        server_id: &str,
        received_at: DateTime<Utc>,
    ) -> Result<Self> {
        require(bytes.len() <= MACHINE_METRICS_MAX_RESPONSE_BYTES)?;
        let response: Self =
            serde_json::from_slice(bytes).map_err(|_| MachineMetricsValidationError)?;
        response.validate(query, server_id, received_at)?;
        Ok(response)
    }

    /// Source-side check before serialization. This is a wire invariant check,
    /// not proof that a machine's measurements or clock are trustworthy.
    pub fn validate(
        &self,
        query: &MachineMetricsQuery,
        server_id: &str,
        received_at: DateTime<Utc>,
    ) -> Result<()> {
        self.validate_model(query, server_id, received_at)?;
        // Count with a hard stop instead of allocating the complete encoding.
        serde_json::to_writer(SizeCounter(0), self).map_err(|_| MachineMetricsValidationError)
    }

    /// Logical wire invariants only, excluding serialized size. Source encoders
    /// must additionally cap writes before publishing any bytes. Transport
    /// consumers should use validate/parse_bounded for the complete contract.
    pub fn validate_model(
        &self,
        query: &MachineMetricsQuery,
        server_id: &str,
        received_at: DateTime<Utc>,
    ) -> Result<()> {
        require(query.valid_at(received_at))?;
        require(
            self.schema == CAPABILITY_MACHINE_METRICS
                && self.server_id == server_id
                && self.scope == "os_visible_machine"
                && self.from == query.from
                && self.to == query.to
                && self.sample_interval_ms == MACHINE_METRICS_SAMPLE_INTERVAL_MS
                && self.stale_after_ms == MACHINE_METRICS_STALE_AFTER_MS
                && self.bucket_width_ms >= self.sample_interval_ms
                && self.bucket_width_ms <= MACHINE_METRICS_MAX_RANGE_MS as u64
                && self.generated_at
                    <= received_at
                        .checked_add_signed(chrono::Duration::seconds(15))
                        .unwrap_or(received_at)
                && self.generated_at >= query.to
                && self
                    .logical_cpus
                    .is_none_or(|count| (1..=MACHINE_METRICS_MAX_LOGICAL_CPUS).contains(&count))
                && self.gpus.len() <= MACHINE_METRICS_MAX_GPUS
                && self.series.len() == 4 + self.gpus.len() * 3
                && self.series.len() <= MACHINE_METRICS_MAX_SERIES
                && self.retained_from.is_none_or(|at| {
                    at <= self.generated_at
                        && self.generated_at.signed_duration_since(at)
                            <= chrono::Duration::hours(24)
                }),
        )?;
        let mut ids = HashSet::new();
        for gpu in &self.gpus {
            gpu.validate_descriptor()?;
            require(ids.insert(gpu.id.as_str()))?;
        }
        let mut keys = HashSet::new();
        let mut total = 0usize;
        let reference = &self.series[0].points;
        for series in &self.series {
            require(keys.insert((series.kind, series.gpu_id.as_deref())))?;
            require(series.unit == series.kind.unit())?;
            require(if series.kind.gpu() {
                series.gpu_id.as_deref().is_some_and(|id| ids.contains(id))
            } else {
                series.gpu_id.is_none()
            })?;
            series.current.validate(self.generated_at, series.unit)?;
            if let Some(id) = series.gpu_id.as_deref() {
                let present = self.gpus.iter().find(|gpu| gpu.id == id).unwrap().present;
                require(present || series.current.status == MachineMetricStatus::DeviceAbsent)?;
            }
            require(series.points.len() <= usize::from(query.max_points))?;
            total += series.points.len();
            require(total <= MACHINE_METRICS_MAX_TOTAL_BUCKETS)?;
            require(series.points.len() == reference.len())?;
            let mut next = query.from;
            for (bucket, other) in series.points.iter().zip(reference) {
                let end = next
                    .checked_add_signed(chrono::Duration::milliseconds(self.bucket_width_ms as i64))
                    .unwrap_or(query.to)
                    .min(query.to);
                require(
                    next < query.to
                        && bucket.from == next
                        && bucket.to == end
                        && bucket.from == other.from
                        && bucket.to == other.to,
                )?;
                bucket.validate(series.unit, self.sample_interval_ms)?;
                if let Some(first) = bucket.first_sample_at {
                    require(
                        self.generated_at.signed_duration_since(first)
                            <= chrono::Duration::hours(24),
                    )?;
                    require(self.retained_from.is_some_and(|start| start <= first))?;
                }
                next = end;
            }
            require(series.points.is_empty() || next == query.to)?;
        }
        for kind in [
            MachineMetricKind::CpuUtilization,
            MachineMetricKind::RamUsed,
            MachineMetricKind::RamAvailable,
            MachineMetricKind::RamTotal,
        ] {
            require(
                self.series
                    .iter()
                    .any(|s| s.kind == kind && s.gpu_id.is_none()),
            )?;
        }
        for gpu in &self.gpus {
            for kind in [
                MachineMetricKind::GpuUtilization,
                MachineMetricKind::GpuMemoryUsed,
                MachineMetricKind::GpuMemoryTotal,
            ] {
                require(
                    self.series
                        .iter()
                        .any(|s| s.kind == kind && s.gpu_id.as_deref() == Some(gpu.id.as_str())),
                )?;
            }
        }
        // Compare simultaneous current memory observations only, never unlike
        // acquisitions or historical means across a capacity change.
        let paired = |used_kind, available_kind, total_kind, gpu: Option<&str>| -> Result<()> {
            let find = |kind| {
                &self
                    .series
                    .iter()
                    .find(|s| s.kind == kind && s.gpu_id.as_deref() == gpu)
                    .unwrap()
                    .current
            };
            let used = find(used_kind);
            let total = find(total_kind);
            if let Some(total_value) = total.value {
                if let Some(used_value) = used.value {
                    require(used.sampled_at == total.sampled_at && used_value <= total_value)?;
                }
                if let Some(kind) = available_kind {
                    let available = find(kind);
                    if let Some(value) = available.value {
                        require(
                            available.sampled_at == total.sampled_at
                                && value <= total_value
                                && used.value.is_none_or(|v| v == total_value - value),
                        )?;
                    }
                }
            }
            Ok(())
        };
        paired(
            MachineMetricKind::RamUsed,
            Some(MachineMetricKind::RamAvailable),
            MachineMetricKind::RamTotal,
            None,
        )?;
        for gpu in &self.gpus {
            paired(
                MachineMetricKind::GpuMemoryUsed,
                None,
                MachineMetricKind::GpuMemoryTotal,
                Some(&gpu.id),
            )?;
        }
        Ok(())
    }
}

struct SizeCounter(usize);

impl std::io::Write for SizeCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MACHINE_METRICS_MAX_RESPONSE_BYTES - self.0 {
            return Err(std::io::Error::other("machine_metrics_response_too_large"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl MachineMetricCurrent {
    fn validate(&self, generated: DateTime<Utc>, unit: MachineMetricUnit) -> Result<()> {
        match (self.sampled_at, self.age_ms) {
            (Some(at), Some(age)) => {
                let delta = generated.signed_duration_since(at).num_milliseconds();
                require(at <= generated && delta >= 0 && age == delta as u64)?;
                if self.status == MachineMetricStatus::Ok {
                    require(age <= MACHINE_METRICS_STALE_AFTER_MS)?;
                }
                if self.status == MachineMetricStatus::Stale {
                    require(age > MACHINE_METRICS_STALE_AFTER_MS)?;
                }
            }
            (None, None) => require(
                self.status != MachineMetricStatus::Ok && self.status != MachineMetricStatus::Stale,
            )?,
            _ => return Err(MachineMetricsValidationError),
        }
        require(match self.value {
            Some(value) => {
                self.status == MachineMetricStatus::Ok
                    && number(value, unit)
                    && (unit != MachineMetricUnit::Bytes || value.fract() == 0.0)
            }
            None => self.status != MachineMetricStatus::Ok,
        })
    }
}

impl MachineMetricBucket {
    fn validate(&self, unit: MachineMetricUnit, interval: u64) -> Result<()> {
        let span = self.to.signed_duration_since(self.from).num_milliseconds();
        require(span > 0)?;
        let expected = (span as u64).div_ceil(interval);
        require(u64::from(self.expected_count) == expected)?;
        let mut observed = 0u64;
        let mut valid = 0;
        let mut statuses = HashSet::new();
        for entry in &self.status_counts {
            require(entry.count > 0 && statuses.insert(entry.status))?;
            observed += u64::from(entry.count);
            if entry.status == MachineMetricStatus::Ok {
                valid = entry.count;
            }
        }
        require(
            observed <= expected
                && valid == self.valid_count
                && (u64::from(valid) == expected || self.partial),
        )?;
        match (
            self.mean,
            self.min,
            self.max,
            self.first_sample_at,
            self.last_sample_at,
        ) {
            (None, None, None, None, None) => require(valid == 0),
            (Some(mean), Some(min), Some(max), Some(first), Some(last)) => require(
                valid > 0
                    && [mean, min, max].into_iter().all(|n| number(n, unit))
                    && min <= mean
                    && mean <= max
                    && self.from <= first
                    && first <= last
                    && last < self.to,
            ),
            _ => Err(MachineMetricsValidationError),
        }
    }
}
