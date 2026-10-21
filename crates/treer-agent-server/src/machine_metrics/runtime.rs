//! Machine-local collection and bounded, streaming disk queries. Metric bodies
//! never enter the Controller command replay cache or application logs.
mod native;
mod query;
mod store;
pub use native::gpu_probe;

use super::memory_budget::{Assembly, Charged, MemoryBudget};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use treer_protocol::*;

const STEADY_CREDIT: usize = 2 * 1024 * 1024;
const QUERY_CREDIT: usize = 14 * 1024 * 1024;
const RECORD_BYTES: usize = 16 * 1024;
const DISK_BYTES: u64 = 64 * 1024 * 1024;
const RETENTION_SECONDS: i64 = 86400;
const SEGMENT_SECONDS: i64 = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Gpu {
    id: String,
    name: String,
    utilization: Option<f64>,
    used: Option<f64>,
    total: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    at: DateTime<Utc>,
    cpu: Option<f64>,
    cpus: Option<u32>,
    used: Option<f64>,
    available: Option<f64>,
    total: Option<f64>,
    memory_method: MachineMemoryMethod,
    gpu_status: MachineMetricStatus,
    gpus: Vec<Gpu>,
}

fn failure(code: &'static str) -> ProtocolError {
    ProtocolError::new(code, "Machine metrics are unavailable")
}

/// Local owner operation only; no remote mutation or Policy bypass is exposed.
pub fn configure_collection(path: PathBuf, enabled: Option<bool>) -> anyhow::Result<bool> {
    let Some(enabled) = enabled else {
        return Ok(!path.join("disabled").exists());
    };
    let store = store::Store::open(path)?;
    store.set_enabled(enabled)?;
    Ok(enabled)
}

/// Retains the assembly slot and its full reservation through serialization and
/// socket publication, not merely until the blocking worker returns.
pub struct Reply {
    bytes: Charged<Vec<u8>>,
    assembly: Assembly,
    _query_lease: std::fs::File,
    store: Arc<store::Store>,
    generation: Vec<u8>,
}
impl Reply {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn revoked(&self) -> bool {
        self.assembly.is_revoked()
            || self.store.disabled()
            || self.store.generation().ok().as_ref() != Some(&self.generation)
    }
}

#[derive(Clone)]
pub struct Metrics {
    store: Arc<store::Store>,
}

impl Metrics {
    /// Every same-account enrollment uses the same machine-owned store. The
    /// source writer lease and query lease are OS locks, shared across processes.
    pub fn open(path: PathBuf) -> Result<Self, ProtocolError> {
        Ok(Self {
            store: Arc::new(
                store::Store::open(path).map_err(|_| failure("machine_metrics_disabled"))?,
            ),
        })
    }

    pub fn start(&self) {
        let weak = Arc::downgrade(&self.store);
        // Capture before a replacement can be accepted by the running service.
        let mut gpu_sampler = native::Sampler::new();
        std::thread::spawn(move || {
            let budget = MemoryBudget::machine();
            let mut credit = None;
            let mut system = sysinfo::System::new();
            let mut warmed = false;
            let mut owner = None;
            let mut last_clock: Option<(DateTime<Utc>, Instant)> = None;
            let mut next = Instant::now();
            loop {
                let Some(store) = weak.upgrade() else { break };
                if owner.is_none() {
                    owner = store.writer_lease().ok();
                    if owner.is_some() {
                        credit = budget
                            .try_build(STEADY_CREDIT, || Ok::<_, ()>(()))
                            .ok()
                            .and_then(Result::ok);
                        if credit.is_none() {
                            owner = None;
                        }
                        warmed = false;
                        last_clock = None;
                    }
                }
                if owner.is_some() {
                    if store.disabled() {
                        let _ = store.clear();
                        system = sysinfo::System::new();
                        warmed = false;
                        last_clock = None;
                        drop(store);
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                    let now = Utc::now();
                    let monotonic = Instant::now();
                    let discontinuity = last_clock.is_some_and(|(wall, tick)| {
                        let elapsed = monotonic.duration_since(tick).as_millis() as i64;
                        let wall_elapsed = (now - wall).num_milliseconds();
                        wall_elapsed <= 0 || (wall_elapsed - elapsed).abs() > 2000
                    });
                    if discontinuity {
                        let _ = store.clear();
                        warmed = false;
                    }
                    last_clock = Some((now, monotonic));
                    system.refresh_cpu_usage();
                    system
                        .refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
                    let total = system.total_memory();
                    let available = system.available_memory().min(total);
                    let sample = Sample {
                        at: now,
                        cpu: warmed.then(|| f64::from(system.global_cpu_usage())),
                        cpus: u32::try_from(system.cpus().len())
                            .ok()
                            .filter(|n| (1..=65_535).contains(n)),
                        used: (total > 0).then_some((total - available) as f64),
                        available: (total > 0).then_some(available as f64),
                        total: (total > 0).then_some(total as f64),
                        memory_method: if cfg!(target_os = "linux") {
                            MachineMemoryMethod::LinuxMemAvailable
                        } else if cfg!(target_os = "macos") {
                            MachineMemoryMethod::MacosAvailableEstimate
                        } else if cfg!(windows) {
                            MachineMemoryMethod::WindowsPhysicalAvailable
                        } else {
                            MachineMemoryMethod::Unavailable
                        },
                        gpu_status: if cfg!(target_os = "macos") {
                            MachineMetricStatus::UnsupportedPlatform
                        } else {
                            MachineMetricStatus::DriverUnavailable
                        },
                        gpus: vec![],
                    };
                    // NVML runs only in the isolated helper, never on this or
                    // the async heartbeat thread; unavailable samples are null.
                    let sample = gpu_sampler.collect(sample, &store);
                    if store.append(&sample).is_err() {
                        owner = None;
                        credit = None;
                    }
                    warmed = true;
                }
                drop(store);
                next += Duration::from_secs(5);
                if next <= Instant::now() {
                    next = Instant::now() + Duration::from_secs(5);
                }
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            drop(credit);
        });
    }

    pub async fn query(
        &self,
        server_id: String,
        query: MachineMetricsQuery,
    ) -> Result<Reply, ProtocolError> {
        if !query.valid_at(Utc::now()) {
            return Err(failure("machine_metrics_invalid_range"));
        }
        let assembly = MemoryBudget::machine()
            .try_assembly()
            .map_err(|_| failure("machine_metrics_busy"))?;
        let cancelled = Arc::new(AtomicBool::new(false));
        struct Cancel(Arc<AtomicBool>);
        impl Drop for Cancel {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _cancel = Cancel(cancelled.clone());
        let store = self.store.clone();
        let worker_assembly = assembly.clone();
        let deadline = Instant::now() + Duration::from_secs(8);
        let task = tokio::task::spawn_blocking(move || {
            let check = || {
                cancelled.load(Ordering::Acquire)
                    || worker_assembly.is_revoked()
                    || Instant::now() >= deadline
            };
            let query_lease = store
                .query_lease()
                .map_err(|_| failure("machine_metrics_busy"))?;
            let generation = store
                .generation()
                .map_err(|_| failure("machine_metrics_invalid_response"))?;
            let bytes = worker_assembly
                .try_build(QUERY_CREDIT, || {
                    query::assemble(&store, &server_id, &query, check)
                })
                .map_err(|_| failure("machine_metrics_busy"))??;
            if store.generation().ok().as_ref() != Some(&generation) {
                return Err(failure("machine_metrics_invalid_response"));
            }
            Ok(Reply {
                bytes,
                assembly: worker_assembly,
                _query_lease: query_lease,
                store,
                generation,
            })
        });
        match tokio::time::timeout(Duration::from_secs(9), task).await {
            Ok(Ok(result)) => result,
            _ => {
                assembly.revoke();
                Err(failure("machine_metrics_timeout"))
            }
        }
    }
}

#[cfg(test)]
mod tests;
