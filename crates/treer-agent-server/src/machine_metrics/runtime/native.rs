use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, Read, Write},
    process::{Command, Stdio},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    status: MachineMetricStatus,
    gpus: Vec<Gpu>,
}

#[derive(Default)]
pub(super) struct Sampler {
    stalled: Option<std::process::Child>,
    retry_after: Option<Instant>,
    failures: u32,
    helper_credit: Option<Arc<Charged<()>>>,
}
impl Drop for Sampler {
    fn drop(&mut self) {
        if let Some(mut child) = self.stalled.take() {
            let _ = child.kill();
            // Reaping never stalls the sampler/Controller shutdown. The child
            // holds its own inherited salt-file lock until it actually exits.
            let credit = self.helper_credit.take();
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(credit);
            });
        }
    }
}
impl Sampler {
    pub(super) fn collect(&mut self, mut sample: Sample, store: &store::Store) -> Sample {
        if cfg!(target_os = "macos") {
            return sample;
        }
        if let Some(child) = self.stalled.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                sample.gpu_status = MachineMetricStatus::TimedOut;
                return sample;
            }
            self.stalled = None;
            self.helper_credit = None;
        }
        if self.retry_after.is_some_and(|at| Instant::now() < at) {
            sample.gpu_status = MachineMetricStatus::DriverUnavailable;
            return sample;
        }
        let result = (|| -> Option<Probe> {
            self.helper_credit = MemoryBudget::machine()
                .try_build(16 * 1024 * 1024, || Ok::<_, ()>(()))
                .ok()?
                .ok()
                .map(Arc::new);
            self.helper_credit.as_ref()?;
            let executable = std::env::current_exe().ok()?;
            let mut command = Command::new(executable);
            command
                .arg("metrics-gpu-probe")
                .env_clear()
                .stdin(Stdio::from(store.gpu_input().ok()?))
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let mut child = command.spawn().ok()?;
            let stdout = child.stdout.take()?;
            let (output_tx, output_rx) = std::sync::mpsc::sync_channel(1);
            let reader_credit = self.helper_credit.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::with_capacity(RECORD_BYTES);
                let result = std::io::BufReader::new(stdout)
                    .take(RECORD_BYTES as u64 + 1)
                    .read_until(b'\n', &mut bytes);
                let valid =
                    result.is_ok() && bytes.len() <= RECORD_BYTES && bytes.last() == Some(&b'\n');
                let _ = output_tx.send(valid.then_some(bytes));
                drop(reader_credit);
            });
            #[cfg(windows)]
            let _tree = match treer_platform::ProcessTreeGuard::for_process(child.id()) {
                Ok(Some(tree)) => tree,
                _ => {
                    let _ = child.kill();
                    if child.try_wait().ok().flatten().is_none() {
                        self.stalled = Some(child);
                    }
                    return None;
                }
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => break,
                    Ok(Some(_)) | Err(_) => {
                        let _ = child.kill();
                        if child.try_wait().ok().flatten().is_none() {
                            self.stalled = Some(child);
                        }
                        return None;
                    }
                    Ok(None) if Instant::now() >= deadline => {
                        #[cfg(unix)]
                        {
                            let _ = treer_platform::kill_process_group(child.id());
                        }
                        let _ = child.kill();
                        if child.try_wait().ok().flatten().is_none() {
                            self.stalled = Some(child);
                        }
                        return None;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            // A descendant retaining stdout cannot block collection: the reader
            // has a byte cap and the parent has a separate receive deadline.
            let bytes = output_rx.recv_timeout(Duration::from_millis(100)).ok()??;
            let probe: Probe = serde_json::from_slice(&bytes).ok()?;
            if probe.gpus.len() > MACHINE_METRICS_MAX_GPUS {
                return None;
            }
            for gpu in &probe.gpus {
                MachineMetricsGpu {
                    id: gpu.id.clone(),
                    name: gpu.name.clone(),
                    present: true,
                }
                .validate_descriptor()
                .ok()?;
                if gpu
                    .utilization
                    .is_some_and(|v| !v.is_finite() || !(0.0..=100.0).contains(&v))
                    || gpu.used.zip(gpu.total).is_some_and(|(used, total)| {
                        used > total || total > 9_007_199_254_740_991.0
                    })
                {
                    return None;
                }
            }
            Some(probe)
        })();
        if self.stalled.is_none() {
            self.helper_credit = None;
        }
        if let Some(probe) = result {
            if matches!(
                probe.status,
                MachineMetricStatus::Ok | MachineMetricStatus::NoDevice
            ) {
                self.failures = 0;
                self.retry_after = None;
            } else {
                self.backoff();
            }
            sample.gpu_status = probe.status;
            sample.gpus = probe.gpus;
        } else {
            self.backoff();
            sample.gpu_status = MachineMetricStatus::CollectionFailed;
        }
        sample
    }
    fn backoff(&mut self) {
        self.failures = self.failures.saturating_add(1).min(5);
        self.retry_after =
            Some(Instant::now() + Duration::from_secs((30_u64 << (self.failures - 1)).min(300)));
    }
}

/// Hidden fixed-argv child. No arbitrary path input; only vetted root-owned
/// system driver files, never PATH/LD_LIBRARY_PATH/DYLD lookup.
pub fn gpu_probe() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    rustix::process::setrlimit(
        rustix::process::Resource::Data,
        rustix::process::Rlimit {
            current: Some(12 * 1024 * 1024),
            maximum: Some(12 * 1024 * 1024),
        },
    )?;
    #[cfg(windows)]
    treer_platform::limit_helper_memory(16 * 1024 * 1024)?;
    #[cfg(unix)]
    let mut input: std::fs::File = {
        use std::os::fd::AsFd;
        std::io::stdin().as_fd().try_clone_to_owned()?.into()
    };
    #[cfg(windows)]
    let mut input: std::fs::File = {
        use std::os::windows::io::AsHandle;
        std::io::stdin().as_handle().try_clone_to_owned()?.into()
    };
    // Held by the helper itself, including an uninterruptible driver call or
    // parent death. A successor cannot enter NVML while an old helper lives.
    input.try_lock()?;
    let mut salt = [0_u8; 32];
    input.read_exact(&mut salt)?;
    let mut result = Probe {
        status: MachineMetricStatus::DriverUnavailable,
        gpus: vec![],
    };
    #[cfg(target_os = "linux")]
    let candidates: Vec<PathBuf> = [
        "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1",
        "/usr/lib/aarch64-linux-gnu/libnvidia-ml.so.1",
        "/usr/lib64/libnvidia-ml.so.1",
        "/usr/lib/libnvidia-ml.so.1",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    #[cfg(windows)]
    let candidates: Vec<PathBuf> =
        vec![treer_platform::system_windows_directory()?.join("System32/nvml.dll")];
    #[cfg(not(any(target_os = "linux", windows)))]
    let candidates: Vec<PathBuf> = vec![];
    for candidate in candidates {
        let Ok(path) = std::fs::canonicalize(candidate) else {
            continue;
        };
        #[cfg(windows)]
        let _guards = {
            // Pin the DLL's parent ancestry against reparse/rename attacks.
            // Use the OS-derived non-verbatim path for the ACL comparison.
            let original = treer_platform::system_windows_directory()?.join("System32/nvml.dll");
            if !treer_platform::trusted_system_library(&original).unwrap_or(false) {
                continue;
            }
            let Ok(root) =
                treer_platform::machine_file::Root::open_local(original.parent().unwrap())
            else {
                continue;
            };
            root
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if path.ancestors().any(|p| {
                std::fs::symlink_metadata(p).map_or(true, |m| {
                    m.uid() != 0 || m.mode() & 0o022 != 0 || m.file_type().is_symlink()
                })
            }) {
                continue;
            }
        }
        let Ok(nvml) = nvml_wrapper::Nvml::builder()
            .lib_path(path.as_os_str())
            .init()
        else {
            continue;
        };
        let Ok(count) = nvml.device_count() else {
            break;
        };
        if count as usize > MACHINE_METRICS_MAX_GPUS {
            result.status = MachineMetricStatus::CollectionFailed;
            break;
        }
        result.status = if count == 0 {
            MachineMetricStatus::NoDevice
        } else {
            MachineMetricStatus::Ok
        };
        for index in 0..count {
            let gpu = (|| -> Option<Gpu> {
                let device = nvml.device_by_index(index).ok()?;
                let identity = device.uuid().ok()?;
                let mut hash = Sha256::new();
                hash.update(salt);
                hash.update(identity.as_bytes());
                let id = format!("{:x}", hash.finalize());
                let name = device.name().ok()?;
                let memory = device.memory_info().ok();
                Some(Gpu {
                    id,
                    name,
                    utilization: device.utilization_rates().ok().map(|v| f64::from(v.gpu)),
                    used: memory.as_ref().map(|v| v.used as f64),
                    total: memory.map(|v| v.total as f64),
                })
            })();
            match gpu {
                Some(gpu) => result.gpus.push(gpu),
                None => {
                    result.gpus.clear();
                    result.status = MachineMetricStatus::CollectionFailed;
                    break;
                }
            }
        }
        break;
    }
    result.gpus.sort_by(|a, b| a.id.cmp(&b.id));
    serde_json::to_writer(std::io::stdout().lock(), &result)?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}
