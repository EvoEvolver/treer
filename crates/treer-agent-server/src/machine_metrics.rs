//! Machine hardware sampler (`machine.metrics.v1`). One thread reads whole-
//! machine CPU and RAM every five seconds into a bounded in-memory history:
//! raw samples for the last hour and per-minute means for the last day.
//! NVIDIA GPUs are read from a long-running `nvidia-smi --loop` child; other
//! GPUs are not reported. Nothing is written to disk; a restart starts empty.
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use treer_protocol::{
    GpuDevice, GpuSample, GpuStatus, MachineMetrics, MachineMetricsSample,
    MACHINE_METRICS_BUCKET_SECONDS, MACHINE_METRICS_MAX_WINDOW_SECONDS,
    MACHINE_METRICS_RAW_SECONDS, MACHINE_METRICS_SAMPLE_SECONDS,
};

const RAW_CAPACITY: usize = (MACHINE_METRICS_RAW_SECONDS / MACHINE_METRICS_SAMPLE_SECONDS) as usize;
const BUCKET_CAPACITY: usize =
    (MACHINE_METRICS_MAX_WINDOW_SECONDS / MACHINE_METRICS_BUCKET_SECONDS) as usize;
/// GPU lines older than this are treated as gone (the child stalled or died).
const GPU_STALE: Duration = Duration::from_secs(15);
const MAX_GPUS: usize = 64;

static STORE: Mutex<Option<Store>> = Mutex::new(None);
static GPUS: Mutex<GpuReadings> = Mutex::new(GpuReadings {
    status: GpuStatus::NotDetected,
    devices: BTreeMap::new(),
});
static START: Once = Once::new();

struct GpuReadings {
    status: GpuStatus,
    devices: BTreeMap<u32, (Instant, GpuDevice, GpuSample)>,
}

/// Starts the sampler threads once per process.
pub fn start() {
    START.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("treer-machine-metrics".into())
            .spawn(sample_loop);
        if let Err(error) = spawned {
            tracing::warn!(%error, "machine metrics sampler did not start");
        }
        if cfg!(target_os = "macos") {
            lock(&GPUS).status = GpuStatus::Unsupported;
        } else if let Err(error) = std::thread::Builder::new()
            .name("treer-gpu-metrics".into())
            .spawn(gpu_loop)
        {
            tracing::warn!(%error, "GPU metrics reader did not start");
        }
    });
}

/// The newest `window_seconds` of history.
pub fn query(window_seconds: u32) -> MachineMetrics {
    let (status, devices) = {
        let gpus = lock(&GPUS);
        (
            gpus.status,
            gpus.devices
                .values()
                .map(|(_, device, _)| device.clone())
                .collect(),
        )
    };
    let store = lock(&STORE);
    let empty = Store::new(0);
    store
        .as_ref()
        .unwrap_or(&empty)
        .query(window_seconds, Utc::now(), status, devices)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// CPU usage and RAM only: no process, disk or network inventory.
fn system() -> System {
    System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    )
}

fn read(system: &mut System, gpus: Vec<GpuSample>) -> MachineMetricsSample {
    system.refresh_cpu_usage();
    system.refresh_memory();
    let total = system.total_memory();
    MachineMetricsSample {
        at: Utc::now(),
        cpu_percent: system.global_cpu_usage().clamp(0.0, 100.0),
        memory_used_bytes: total.saturating_sub(system.available_memory()),
        memory_total_bytes: total,
        gpus,
    }
}

fn sample_loop() {
    let mut system = system();
    let cpu_count = system.cpus().len() as u32;
    *lock(&STORE) = Some(Store::new(cpu_count));
    let period = Duration::from_secs(MACHINE_METRICS_SAMPLE_SECONDS.into());
    let mut next = Instant::now() + period;
    loop {
        // Usage is measured between two refreshes, so the first reading
        // covers a full period rather than reporting a warm-up zero.
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        next += period;
        let now = Instant::now();
        if next < now {
            // The machine slept or the thread stalled: do not burst.
            next = now + period;
        }
        let sample = read(&mut system, current_gpus(now));
        if let Some(store) = lock(&STORE).as_mut() {
            store.push(sample);
        }
    }
}

fn current_gpus(now: Instant) -> Vec<GpuSample> {
    let mut gpus = lock(&GPUS);
    gpus.devices
        .retain(|_, (seen, ..)| now.saturating_duration_since(*seen) < GPU_STALE);
    if gpus.devices.is_empty() && gpus.status == GpuStatus::Available {
        gpus.status = GpuStatus::NotDetected;
    }
    gpus.devices
        .values()
        .map(|(_, _, sample)| sample.clone())
        .collect()
}

const NVIDIA_QUERY: &str = "--query-gpu=index,name,utilization.gpu,memory.used,memory.total";

fn gpu_loop() {
    let mut backoff = Duration::from_secs(30);
    loop {
        let started = Instant::now();
        match run_nvidia_smi() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // No NVIDIA tooling: look again occasionally (driver installs).
                backoff = Duration::from_secs(300);
            }
            Err(error) => tracing::debug!(%error, "nvidia-smi stopped"),
        }
        if started.elapsed() > Duration::from_secs(600) {
            backoff = Duration::from_secs(30);
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
}

fn run_nvidia_smi() -> std::io::Result<()> {
    let mut command = Command::new("nvidia-smi");
    command
        .args([
            NVIDIA_QUERY,
            "--format=csv,noheader,nounits",
            &format!("--loop={MACHINE_METRICS_SAMPLE_SECONDS}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Some((device, sample)) = parse_nvidia_line(&line) else {
            continue;
        };
        let mut gpus = lock(&GPUS);
        if gpus.devices.len() < MAX_GPUS || gpus.devices.contains_key(&device.index) {
            gpus.status = GpuStatus::Available;
            gpus.devices
                .insert(device.index, (Instant::now(), device, sample));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// `index, name, utilization.gpu [%], memory.used [MiB], memory.total [MiB]`;
/// unsupported fields print as `[N/A]` or `[Not Supported]`.
fn parse_nvidia_line(line: &str) -> Option<(GpuDevice, GpuSample)> {
    let fields: Vec<&str> = line.trim().split(',').map(str::trim).collect();
    let [index, name, utilization, used, total] = fields.as_slice() else {
        return None;
    };
    let index = index.parse().ok()?;
    let mib = |value: &str| value.parse::<u64>().ok().map(|mib| mib * 1024 * 1024);
    Some((
        GpuDevice {
            index,
            name: name.chars().take(128).collect(),
        },
        GpuSample {
            index,
            utilization_percent: utilization
                .parse::<f32>()
                .ok()
                .map(|value| value.clamp(0.0, 100.0)),
            memory_used_bytes: mib(used),
            memory_total_bytes: mib(total),
        },
    ))
}

/// Raw samples for the last hour plus per-minute means for the last day.
struct Store {
    cpu_count: u32,
    raw: VecDeque<MachineMetricsSample>,
    buckets: VecDeque<MachineMetricsSample>,
    /// Samples of the minute currently being filled.
    minute: Vec<MachineMetricsSample>,
}

impl Store {
    fn new(cpu_count: u32) -> Self {
        Self {
            cpu_count,
            raw: VecDeque::with_capacity(RAW_CAPACITY),
            buckets: VecDeque::new(),
            minute: Vec::new(),
        }
    }

    fn push(&mut self, sample: MachineMetricsSample) {
        if self.raw.back().is_some_and(|last| sample.at <= last.at) {
            // The wall clock went backwards: the history no longer lines up.
            self.raw.clear();
            self.buckets.clear();
            self.minute.clear();
        }
        if self
            .minute
            .first()
            .is_some_and(|first| minute_of(first.at) != minute_of(sample.at))
        {
            self.flush_minute();
        }
        self.minute.push(sample.clone());
        if self.raw.len() == RAW_CAPACITY {
            self.raw.pop_front();
        }
        self.raw.push_back(sample);
    }

    fn flush_minute(&mut self) {
        if let Some(bucket) = mean(&self.minute) {
            if self.buckets.len() == BUCKET_CAPACITY {
                self.buckets.pop_front();
            }
            self.buckets.push_back(bucket);
        }
        self.minute.clear();
    }

    fn query(
        &self,
        window_seconds: u32,
        now: DateTime<Utc>,
        gpu_status: GpuStatus,
        gpus: Vec<GpuDevice>,
    ) -> MachineMetrics {
        let window_seconds = window_seconds.clamp(1, MACHINE_METRICS_MAX_WINDOW_SECONDS);
        let since = now - chrono::Duration::seconds(window_seconds.into());
        let (resolution_seconds, samples) = if window_seconds <= MACHINE_METRICS_RAW_SECONDS {
            (
                MACHINE_METRICS_SAMPLE_SECONDS,
                self.raw.iter().filter(|s| s.at >= since).cloned().collect(),
            )
        } else {
            (
                MACHINE_METRICS_BUCKET_SECONDS,
                self.buckets
                    .iter()
                    .cloned()
                    .chain(mean(&self.minute))
                    .filter(|s| s.at >= since)
                    .collect(),
            )
        };
        MachineMetrics {
            cpu_count: self.cpu_count,
            gpu_status,
            gpus,
            sample_seconds: MACHINE_METRICS_SAMPLE_SECONDS,
            resolution_seconds,
            window_seconds,
            samples,
            latest: self.raw.back().cloned(),
        }
    }
}

fn minute_of(at: DateTime<Utc>) -> i64 {
    at.timestamp()
        .div_euclid(MACHINE_METRICS_BUCKET_SECONDS.into())
}

/// Mean of one minute, stamped at the minute's start. A GPU's fields average
/// only the samples that reported them.
fn mean(samples: &[MachineMetricsSample]) -> Option<MachineMetricsSample> {
    let first = samples.first()?;
    let at = Utc
        .timestamp_opt(
            minute_of(first.at) * i64::from(MACHINE_METRICS_BUCKET_SECONDS),
            0,
        )
        .single()?;
    let mut gpus: BTreeMap<u32, Vec<&GpuSample>> = BTreeMap::new();
    for gpu in samples.iter().flat_map(|sample| &sample.gpus) {
        gpus.entry(gpu.index).or_default().push(gpu);
    }
    let average = |values: Vec<f64>| {
        (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
    };
    Some(MachineMetricsSample {
        at,
        cpu_percent: average(samples.iter().map(|s| s.cpu_percent.into()).collect())? as f32,
        memory_used_bytes: average(samples.iter().map(|s| s.memory_used_bytes as f64).collect())?
            as u64,
        memory_total_bytes: samples.iter().map(|s| s.memory_total_bytes).max()?,
        gpus: gpus
            .into_iter()
            .map(|(index, readings)| GpuSample {
                index,
                utilization_percent: average(
                    readings
                        .iter()
                        .filter_map(|gpu| gpu.utilization_percent.map(f64::from))
                        .collect(),
                )
                .map(|value| value as f32),
                memory_used_bytes: average(
                    readings
                        .iter()
                        .filter_map(|gpu| gpu.memory_used_bytes.map(|value| value as f64))
                        .collect(),
                )
                .map(|value| value as u64),
                memory_total_bytes: readings
                    .iter()
                    .filter_map(|gpu| gpu.memory_total_bytes)
                    .max(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seconds: i64, cpu: f32, gpus: Vec<GpuSample>) -> MachineMetricsSample {
        MachineMetricsSample {
            at: Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap(),
            cpu_percent: cpu,
            memory_used_bytes: 1000 + seconds as u64,
            memory_total_bytes: 4000,
            gpus,
        }
    }

    fn gpu(index: u32, utilization: Option<f32>) -> GpuSample {
        GpuSample {
            index,
            utilization_percent: utilization,
            memory_used_bytes: Some(10),
            memory_total_bytes: Some(20),
        }
    }

    #[test]
    fn raw_ring_is_bounded_and_windowed() {
        let mut store = Store::new(8);
        for step in 0..(RAW_CAPACITY as i64 + 10) {
            store.push(sample(step * 5, 50.0, Vec::new()));
        }
        assert_eq!(store.raw.len(), RAW_CAPACITY);
        let now = store.raw.back().unwrap().at;
        let result = store.query(60, now, GpuStatus::NotDetected, Vec::new());
        assert_eq!(result.resolution_seconds, MACHINE_METRICS_SAMPLE_SECONDS);
        assert_eq!(result.samples.len(), 13);
        assert_eq!(result.latest.as_ref().map(|s| s.at), Some(now));
        assert_eq!(result.cpu_count, 8);
    }

    #[test]
    fn long_windows_use_minute_means() {
        let mut store = Store::new(4);
        // Minute-aligned start: 1_800_000_000 is divisible by 60.
        for step in 0..36 {
            let cpu = if step < 12 { 10.0 } else { 70.0 };
            let gpus = vec![gpu(0, (step % 2 == 0).then_some(40.0))];
            store.push(sample(step * 5, cpu, gpus));
        }
        let now = store.raw.back().unwrap().at;
        let result = store.query(6 * 3600, now, GpuStatus::Available, Vec::new());
        assert_eq!(result.resolution_seconds, MACHINE_METRICS_BUCKET_SECONDS);
        // Two flushed minutes plus the minute in progress.
        assert_eq!(result.samples.len(), 3);
        assert_eq!(result.samples[0].cpu_percent, 10.0);
        assert_eq!(result.samples[1].cpu_percent, 70.0);
        assert_eq!(result.samples[0].at.timestamp() % 60, 0);
        // Missing GPU readings are skipped, not averaged as zero.
        assert_eq!(result.samples[0].gpus[0].utilization_percent, Some(40.0));
    }

    #[test]
    fn clock_going_backwards_restarts_history() {
        let mut store = Store::new(1);
        store.push(sample(100, 1.0, Vec::new()));
        store.push(sample(50, 2.0, Vec::new()));
        assert_eq!(store.raw.len(), 1);
        assert_eq!(store.raw[0].cpu_percent, 2.0);
    }

    #[test]
    fn parses_nvidia_smi_csv() {
        let (device, sample) =
            parse_nvidia_line("0, NVIDIA GeForce RTX 4090, 37, 1024, 24564").unwrap();
        assert_eq!(device.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(sample.utilization_percent, Some(37.0));
        assert_eq!(sample.memory_used_bytes, Some(1024 * 1024 * 1024));
        let (_, sample) = parse_nvidia_line("1, Tesla T4, [N/A], [Not Supported], 15360").unwrap();
        assert_eq!(sample.index, 1);
        assert_eq!(sample.utilization_percent, None);
        assert_eq!(sample.memory_used_bytes, None);
        assert!(parse_nvidia_line("garbage").is_none());
    }

    #[test]
    fn reads_this_machine() {
        let mut system = system();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let sample = read(&mut system, Vec::new());
        assert!(!system.cpus().is_empty());
        assert!(sample.memory_total_bytes > 0);
        assert!(sample.memory_used_bytes <= sample.memory_total_bytes);
        assert!((0.0..=100.0).contains(&sample.cpu_percent));
    }

    #[test]
    fn query_without_sampler_is_empty() {
        let result = Store::new(0).query(0, Utc::now(), GpuStatus::Unsupported, Vec::new());
        assert_eq!(result.window_seconds, 1);
        assert!(result.samples.is_empty() && result.latest.is_none());
    }
}
