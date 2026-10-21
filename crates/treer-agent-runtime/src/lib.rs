use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use chrono::Utc;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};
use tracing::warn;
use treer_host_protocol::{
    HostOutputChunk, HostOutputReplay, HostProcessInfo, HostSpawnRequest, HostWrite, OutputCursor,
};
use uuid::Uuid;

const OUTPUT_LIMIT_BYTES: usize = 512 * 1024;
const COMPLETED_PROCESS_LIMIT: usize = 256;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const INPUT_LIMIT_BYTES: usize = 2 * 1024 * 1024;
const INPUT_DEADLINE: Duration = Duration::from_secs(5);
// A platform write may remain in its kernel call until the owned PTY closes.
// Its permit stays with that writer thread, including after a caller times out.
const INPUT_WRITER_LIMIT: usize = 128;
static INPUT_WRITERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("process {0} already exists")]
    ProcessExists(String),
    #[error("process {0} was not found")]
    ProcessNotFound(String),
    #[error("process {0} is no longer running")]
    ProcessNotRunning(String),
    #[error("invalid working directory: {0}")]
    InvalidCwd(String),
    #[error("invalid process request: {0}")]
    InvalidRequest(String),
    #[error("failed to start process: {0}")]
    Spawn(String),
    #[error("terminal operation failed: {0}")]
    Terminal(String),
    #[error("terminal input outcome is unknown; do not retry; inspect or stop the owned process")]
    InputUnknown,
    #[error("input refused: prior input outcome unknown; this request was not admitted; reconcile the owned process")]
    InputRefused,
    #[error("terminal input or spawn capacity is busy; request was not admitted")]
    Busy,
    #[error("process exit outcome is unknown; reconcile the owned process")]
    StopUnknown,
}

impl RuntimeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ProcessExists(_) => "process_exists",
            Self::ProcessNotFound(_) => "process_not_found",
            Self::ProcessNotRunning(_) => "process_not_running",
            Self::InvalidCwd(_) => "invalid_cwd",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Spawn(_) => "spawn_failed",
            Self::Terminal(_) => "terminal_error",
            Self::InputUnknown => "input_unknown",
            Self::InputRefused => "input_refused",
            Self::Busy => "runtime_busy",
            Self::StopUnknown => "operation_unknown",
        }
    }
}

#[derive(Clone)]
pub struct HostRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    host_epoch: String,
    root: PathBuf,
    processes: RwLock<HashMap<String, Arc<HostProcess>>>,
    spawning: Mutex<HashSet<String>>,
    completed_processes: Mutex<VecDeque<String>>,
    process_events: broadcast::Sender<HostProcessInfo>,
    output_events: broadcast::Sender<HostOutputChunk>,
}

struct HostProcess {
    #[cfg(windows)]
    process_tree: Mutex<Option<treer_platform::ProcessTreeGuard>>,
    info: Mutex<HostProcessInfo>,
    input: Mutex<Option<std_mpsc::SyncSender<InputWrite>>>,
    input_busy: AtomicBool,
    input_uncertain: Arc<AtomicBool>,
    #[cfg(test)]
    input_writer_finished: Arc<AtomicBool>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    output: Mutex<OutputBuffer>,
    stopping: AtomicBool,
}

struct InputWrite {
    writes: Vec<HostWrite>,
    deadline: Instant,
    uncertain: Arc<AtomicBool>,
    result: std_mpsc::SyncSender<Result<(), String>>,
    async_result: tokio::sync::oneshot::Sender<Result<(), String>>,
}

struct InputCompletion {
    process: Arc<HostProcess>,
    result: std_mpsc::Receiver<Result<(), String>>,
    async_result: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    accepted: bool,
}

impl InputCompletion {
    fn finish(mut self, success: bool) -> Result<HostProcessInfo, RuntimeError> {
        if !success {
            return Err(RuntimeError::InputUnknown);
        }
        self.accepted = false;
        self.process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("process state lock poisoned".into()))
            .map(|info| info.clone())
    }
}

impl Drop for InputCompletion {
    fn drop(&mut self) {
        if self.accepted {
            self.process.input_uncertain.store(true, Ordering::Release);
        }
        self.process.input_busy.store(false, Ordering::Release);
    }
}

struct OutputBuffer {
    stream_epoch: String,
    chunks: VecDeque<HostOutputChunk>,
    byte_len: usize,
    next_revision: u64,
    mode_tail: Vec<u8>,
    bracketed_paste: bool,
}

impl OutputBuffer {
    fn new() -> Self {
        Self {
            stream_epoch: format!("stream_{}", Uuid::new_v4().simple()),
            chunks: VecDeque::new(),
            byte_len: 0,
            next_revision: 1,
            mode_tail: Vec::new(),
            bracketed_paste: false,
        }
    }

    fn push(&mut self, process_id: &str, bytes: &[u8]) -> HostOutputChunk {
        self.update_terminal_modes(bytes);
        let chunk = HostOutputChunk {
            process_id: process_id.to_string(),
            stream_epoch: self.stream_epoch.clone(),
            revision: self.next_revision,
            data: bytes.to_vec(),
            bracketed_paste: self.bracketed_paste,
            emitted_at: Utc::now(),
        };
        self.next_revision = self.next_revision.saturating_add(1);
        self.byte_len = self.byte_len.saturating_add(bytes.len());
        self.chunks.push_back(chunk.clone());
        while self.byte_len > OUTPUT_LIMIT_BYTES {
            let Some(removed) = self.chunks.pop_front() else {
                break;
            };
            self.byte_len = self.byte_len.saturating_sub(removed.data.len());
        }
        chunk
    }

    fn first_revision(&self) -> u64 {
        self.chunks
            .front()
            .map_or(self.next_revision, |chunk| chunk.revision)
    }

    fn replay(&self, process_id: &str, cursor: Option<&OutputCursor>) -> HostOutputReplay {
        let first = self.first_revision();
        let matching_revision = cursor
            .filter(|cursor| cursor.stream_epoch == self.stream_epoch)
            .map_or(0, |cursor| cursor.revision);
        let gap = cursor.is_some_and(|cursor| {
            cursor.stream_epoch != self.stream_epoch || cursor.revision.saturating_add(1) < first
        });
        let chunks = self
            .chunks
            .iter()
            .filter(|chunk| chunk.revision > matching_revision)
            .cloned()
            .collect();
        HostOutputReplay {
            process_id: process_id.to_string(),
            stream_epoch: self.stream_epoch.clone(),
            first_available_revision: first,
            next_revision: self.next_revision,
            gap,
            chunks,
        }
    }

    fn update_terminal_modes(&mut self, bytes: &[u8]) {
        let mut scan = Vec::with_capacity(self.mode_tail.len() + bytes.len());
        scan.extend_from_slice(&self.mode_tail);
        scan.extend_from_slice(bytes);
        for index in 0..scan.len() {
            let remaining = &scan[index..];
            if remaining.starts_with(BRACKETED_PASTE_ENABLE) {
                self.bracketed_paste = true;
            } else if remaining.starts_with(BRACKETED_PASTE_DISABLE) {
                self.bracketed_paste = false;
            }
        }
        let tail_len = BRACKETED_PASTE_ENABLE
            .len()
            .max(BRACKETED_PASTE_DISABLE.len())
            .saturating_sub(1)
            .min(scan.len());
        self.mode_tail.clear();
        self.mode_tail
            .extend_from_slice(&scan[scan.len() - tail_len..]);
    }
}

impl HostRuntime {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let root = std::fs::canonicalize(root.as_ref())
            .map_err(|error| RuntimeError::InvalidCwd(error.to_string()))?;
        if !root.is_dir() {
            return Err(RuntimeError::InvalidCwd(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let (process_events, _) = broadcast::channel(512);
        let (output_events, _) = broadcast::channel(2048);
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                host_epoch: format!("host_{}", Uuid::new_v4().simple()),
                root,
                processes: RwLock::new(HashMap::new()),
                spawning: Mutex::new(HashSet::new()),
                completed_processes: Mutex::new(VecDeque::new()),
                process_events,
                output_events,
            }),
        })
    }

    pub fn host_epoch(&self) -> &str {
        &self.inner.host_epoch
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn subscribe_processes(&self) -> broadcast::Receiver<HostProcessInfo> {
        self.inner.process_events.subscribe()
    }

    pub fn subscribe_output(&self) -> broadcast::Receiver<HostOutputChunk> {
        self.inner.output_events.subscribe()
    }

    pub fn list(&self) -> Vec<HostProcessInfo> {
        let Ok(processes) = self.inner.processes.read() else {
            return Vec::new();
        };
        let mut result: Vec<_> = processes
            .values()
            .filter_map(|process| process.info.lock().ok().map(|info| info.clone()))
            .collect();
        result.sort_by(|left, right| left.process_id.cmp(&right.process_id));
        result
    }

    pub fn spawn(&self, request: HostSpawnRequest) -> Result<HostProcessInfo, RuntimeError> {
        if request.process_id.trim().is_empty() || request.command.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest(
                "process_id and command must not be empty".to_string(),
            ));
        }
        let request_bytes = request
            .command
            .len()
            .saturating_add(request.cwd.len())
            .saturating_add(request.metadata.len())
            .saturating_add(request.args.iter().map(String::len).sum::<usize>())
            .saturating_add(
                request
                    .env
                    .iter()
                    .map(|(key, value)| key.len().saturating_add(value.len()))
                    .sum::<usize>(),
            );
        if request.process_id.len() > 128 || request_bytes > 64 * 1024 {
            return Err(RuntimeError::InvalidRequest(
                "spawn exceeds bounded identity or request limit".into(),
            ));
        }
        // Reserve only this id. Path lookup, PTY allocation and process launch
        // must never hold the process registry used by list/read/stop.
        let reservation = SpawnReservation::new(&self.inner, &request.process_id)?;
        let processes =
            self.inner.processes.read().map_err(|_| {
                RuntimeError::Terminal("process registry lock poisoned".to_string())
            })?;
        if let Some(process) = processes.get(&request.process_id) {
            if process
                .info
                .lock()
                .map_err(|_| RuntimeError::Terminal("process state lock poisoned".into()))?
                .running
            {
                return Err(RuntimeError::ProcessExists(request.process_id));
            }
            self.inner
                .completed_processes
                .lock()
                .map_err(|_| RuntimeError::Terminal("completed registry lock poisoned".into()))?
                .retain(|id| id != &request.process_id);
        }
        drop(processes);
        let writer_permit =
            Arc::clone(INPUT_WRITERS.get_or_init(|| Arc::new(Semaphore::new(INPUT_WRITER_LIMIT))))
                .try_acquire_owned()
                .map_err(|_| RuntimeError::Busy)?;
        let cwd = self.resolve_cwd(&request.cwd)?;
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: request.rows.max(1),
                cols: request.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        let mut builder = CommandBuilder::new(request.command);
        builder.args(request.args);
        builder.cwd(&cwd);
        // portable-pty otherwise captures the Host's entire environment.
        builder.env_clear();
        for (key, value) in treer_platform::inherited_environment() {
            builder.env(key, value);
        }
        for (key, value) in request.env {
            builder.env(key, value);
        }
        #[allow(unused_mut)]
        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| RuntimeError::Spawn(error.to_string()))?;
        drop(pair.slave);
        let pid = child.process_id();
        #[cfg(windows)]
        let process_tree = match pid
            .map(treer_platform::ProcessTreeGuard::for_process)
            .transpose()
        {
            Ok(job) => job.flatten(),
            Err(error) => {
                let _ = child.kill();
                return Err(RuntimeError::Spawn(format!(
                    "failed to own workload process tree: {error}"
                )));
            }
        };
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        let (input, input_rx) = std_mpsc::sync_channel(1);
        let input_writer_finished = Arc::new(AtomicBool::new(false));
        spawn_input_writer(
            writer,
            input_rx,
            writer_permit,
            Arc::clone(&input_writer_finished),
            request.process_id.clone(),
        );
        let output = OutputBuffer::new();
        let now = Utc::now();
        let info = HostProcessInfo {
            process_id: request.process_id.clone(),
            pid,
            cwd: relative_display(&self.inner.root, &cwd),
            running: true,
            started_at: now,
            last_output_at: now,
            exited_at: None,
            exit_code: None,
            stream_epoch: output.stream_epoch.clone(),
            first_revision: output.first_revision(),
            next_revision: output.next_revision,
            bracketed_paste: false,
            metadata: request.metadata,
        };
        let process = Arc::new(HostProcess {
            #[cfg(windows)]
            process_tree: Mutex::new(process_tree),
            info: Mutex::new(info.clone()),
            input: Mutex::new(Some(input)),
            input_busy: AtomicBool::new(false),
            input_uncertain: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            input_writer_finished,
            child: Mutex::new(Some(child)),
            master: Mutex::new(Some(pair.master)),
            output: Mutex::new(output),
            stopping: AtomicBool::new(false),
        });
        self.inner
            .processes
            .write()
            .map_err(|_| RuntimeError::Terminal("process registry lock poisoned".into()))?
            .insert(request.process_id, Arc::clone(&process));
        drop(reservation);
        let _ = self.inner.process_events.send(info.clone());
        spawn_output_reader(Arc::downgrade(&self.inner), Arc::clone(&process), reader);
        spawn_process_monitor(Arc::downgrade(&self.inner), process);
        Ok(info)
    }

    pub fn read(
        &self,
        process_id: &str,
        cursor: Option<&OutputCursor>,
    ) -> Result<HostOutputReplay, RuntimeError> {
        let process = self.get(process_id)?;
        process
            .output
            .lock()
            .map_err(|_| RuntimeError::Terminal("output lock poisoned".to_string()))
            .map(|output| output.replay(process_id, cursor))
    }

    pub fn write(
        &self,
        process_id: &str,
        writes: &[HostWrite],
    ) -> Result<HostProcessInfo, RuntimeError> {
        let completion = self.begin_write(process_id, writes)?;
        let success = matches!(completion.result.recv_timeout(INPUT_DEADLINE), Ok(Ok(())));
        completion.finish(success)
    }

    /// Host callers wait asynchronously; no shared blocking worker is occupied
    /// by input backpressure or inter-part delay. Dropping this future poisons
    /// the input generation rather than silently permitting a retry.
    pub async fn write_async(
        &self,
        process_id: &str,
        writes: &[HostWrite],
    ) -> Result<HostProcessInfo, RuntimeError> {
        let mut completion = self.begin_write(process_id, writes)?;
        let receiver = completion.async_result.take().expect("completion receiver");
        let success = matches!(
            tokio::time::timeout(INPUT_DEADLINE, receiver).await,
            Ok(Ok(Ok(())))
        );
        completion.finish(success)
    }

    fn begin_write(
        &self,
        process_id: &str,
        writes: &[HostWrite],
    ) -> Result<InputCompletion, RuntimeError> {
        if writes.is_empty() {
            return Err(RuntimeError::InvalidRequest(
                "writes must not be empty".to_string(),
            ));
        }
        let bytes = writes
            .iter()
            .try_fold(0usize, |sum, write| sum.checked_add(write.data.len()));
        let delay = writes
            .iter()
            .try_fold(0u64, |sum, write| sum.checked_add(write.delay_ms));
        if writes.len() > 256
            || bytes.is_none_or(|n| n > INPUT_LIMIT_BYTES)
            || delay.is_none_or(|n| n >= INPUT_DEADLINE.as_millis() as u64)
        {
            return Err(RuntimeError::InvalidRequest(
                "input exceeds bounded batch or delay limit".into(),
            ));
        }
        let process = self.get_running(process_id)?;
        if process.input_uncertain.load(Ordering::Acquire) {
            return Err(RuntimeError::InputRefused);
        }
        process
            .input_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RuntimeError::Busy)?;
        let (result, result_rx) = std_mpsc::sync_channel(1);
        let (async_result, async_rx) = tokio::sync::oneshot::channel();
        let mut completion = InputCompletion {
            process: Arc::clone(&process),
            result: result_rx,
            async_result: Some(async_rx),
            accepted: false,
        };
        {
            let input = process
                .input
                .lock()
                .map_err(|_| RuntimeError::Terminal("terminal input lock poisoned".to_string()))?
                .as_ref()
                .cloned()
                .ok_or_else(|| RuntimeError::ProcessNotRunning(process_id.to_string()))?;
            input
                .try_send(InputWrite {
                    writes: writes.to_vec(),
                    deadline: Instant::now() + INPUT_DEADLINE,
                    uncertain: Arc::clone(&process.input_uncertain),
                    result,
                    async_result,
                })
                .map_err(|_| RuntimeError::Busy)?;
        }
        completion.accepted = true;
        Ok(completion)
    }

    pub fn resize(
        &self,
        process_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<HostProcessInfo, RuntimeError> {
        let process = self.get_running(process_id)?;
        let master = process.master.try_lock().map_err(|_| RuntimeError::Busy)?;
        master
            .as_ref()
            .ok_or_else(|| RuntimeError::ProcessNotRunning(process_id.to_string()))?
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("process state lock poisoned".to_string()))
            .map(|info| info.clone())
    }

    pub fn stop(&self, process_id: &str) -> Result<HostProcessInfo, RuntimeError> {
        let process = self.get_running(process_id)?;
        let mut child = process.child.try_lock().map_err(|_| RuntimeError::Busy)?;
        // One stop submission per generation. Further requests cannot create
        // more possibly-effectful kills / UNKNOWN ledger entries while this
        // target is stuck. A failed try_lock has not consumed this admission.
        process
            .stopping
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RuntimeError::Busy)?;
        process.input_uncertain.store(true, Ordering::Release);
        child
            .as_mut()
            .ok_or_else(|| RuntimeError::ProcessNotRunning(process_id.to_string()))?
            .kill()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        drop(child);
        #[cfg(windows)]
        process
            .process_tree
            .lock()
            .map_err(|_| RuntimeError::Terminal("process tree lock poisoned".into()))?
            .take();
        // A successful kill request is not an observed exit. The monitor owns
        // reaping and the terminal state; do not release a generation early.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let info = process
                .info
                .lock()
                .map_err(|_| RuntimeError::Terminal("process state lock poisoned".into()))?
                .clone();
            if !info.running {
                return Ok(info);
            }
            if Instant::now() >= deadline {
                return Err(RuntimeError::StopUnknown);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn get(&self, process_id: &str) -> Result<Arc<HostProcess>, RuntimeError> {
        self.inner
            .processes
            .read()
            .ok()
            .and_then(|processes| processes.get(process_id).cloned())
            .ok_or_else(|| RuntimeError::ProcessNotFound(process_id.to_string()))
    }

    fn get_running(&self, process_id: &str) -> Result<Arc<HostProcess>, RuntimeError> {
        let process = self.get(process_id)?;
        let running = process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("process state lock poisoned".to_string()))?
            .running;
        if running {
            Ok(process)
        } else {
            Err(RuntimeError::ProcessNotRunning(process_id.to_string()))
        }
    }

    fn resolve_cwd(&self, requested: &str) -> Result<PathBuf, RuntimeError> {
        let requested = if requested.trim().is_empty() {
            Path::new(".")
        } else {
            Path::new(requested)
        };
        if requested.is_absolute() {
            return Err(RuntimeError::InvalidCwd(
                "cwd must be relative to the host root".to_string(),
            ));
        }
        let cwd = std::fs::canonicalize(self.inner.root.join(requested))
            .map_err(|error| RuntimeError::InvalidCwd(error.to_string()))?;
        if !cwd.starts_with(&self.inner.root) {
            return Err(RuntimeError::InvalidCwd(
                "cwd resolves outside the host root".to_string(),
            ));
        }
        Ok(cwd)
    }
}

impl RuntimeInner {
    fn update_process(
        &self,
        process: &HostProcess,
        mutation: impl FnOnce(&mut HostProcessInfo),
    ) -> Option<HostProcessInfo> {
        let mut info = process.info.lock().ok()?;
        mutation(&mut info);
        let snapshot = info.clone();
        drop(info);
        let _ = self.process_events.send(snapshot.clone());
        Some(snapshot)
    }

    fn finish_process(&self, process: &HostProcess) {
        #[cfg(windows)]
        if let Ok(mut job) = process.process_tree.lock() {
            job.take();
        }
        if let Ok(mut input) = process.input.lock() {
            input.take();
        }
        if let Ok(mut master) = process.master.lock() {
            master.take();
        }
        if let Ok(mut child) = process.child.lock() {
            child.take();
        }

        let process_id = process.info.lock().ok().map(|info| info.process_id.clone());
        let Some(process_id) = process_id else {
            return;
        };
        let Ok(mut processes) = self.processes.write() else {
            return;
        };
        if !processes
            .get(&process_id)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), process))
        {
            return;
        }
        let evicted = self
            .completed_processes
            .lock()
            .ok()
            .and_then(|mut completed| {
                push_bounded(&mut completed, process_id, COMPLETED_PROCESS_LIMIT)
            });
        if let Some(evicted) = evicted {
            processes.remove(&evicted);
        }
    }
}

fn push_bounded(
    completed: &mut VecDeque<String>,
    process_id: String,
    limit: usize,
) -> Option<String> {
    completed.push_back(process_id);
    (completed.len() > limit)
        .then(|| completed.pop_front())
        .flatten()
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let Ok(processes) = self.processes.read() else {
            return;
        };
        for process in processes.values() {
            process.stopping.store(true, Ordering::Release);
            if let Ok(mut child) = process.child.lock() {
                if let Some(child) = child.as_mut() {
                    if let Err(error) = child.kill() {
                        warn!(%error, "failed to stop child while dropping host runtime");
                    }
                }
            }
        }
    }
}

struct SpawnReservation<'a> {
    runtime: &'a RuntimeInner,
    id: String,
}

impl<'a> SpawnReservation<'a> {
    fn new(runtime: &'a RuntimeInner, id: &str) -> Result<Self, RuntimeError> {
        let mut spawning = runtime.spawning.lock().map_err(|_| RuntimeError::Busy)?;
        if !spawning.insert(id.to_owned()) {
            return Err(RuntimeError::Busy);
        }
        Ok(Self {
            runtime,
            id: id.to_owned(),
        })
    }
}

impl Drop for SpawnReservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut spawning) = self.runtime.spawning.lock() {
            spawning.remove(&self.id);
        }
    }
}

fn spawn_input_writer(
    mut writer: Box<dyn Write + Send>,
    input: std_mpsc::Receiver<InputWrite>,
    _permit: OwnedSemaphorePermit,
    finished: Arc<AtomicBool>,
    process_id: String,
) {
    std::thread::spawn(move || {
        struct WriterLease {
            permit: Option<OwnedSemaphorePermit>,
            finished: Arc<AtomicBool>,
        }
        impl Drop for WriterLease {
            fn drop(&mut self) {
                drop(self.permit.take());
                self.finished.store(true, Ordering::Release);
            }
        }
        let _lease = WriterLease {
            permit: Some(_permit),
            finished,
        };
        while let Ok(write) = input.recv() {
            let started = Instant::now();
            let result = (|| {
                for part in &write.writes {
                    let delay_end = Instant::now() + Duration::from_millis(part.delay_ms);
                    while Instant::now() < delay_end {
                        if write.uncertain.load(Ordering::Acquire)
                            || Instant::now() >= write.deadline
                        {
                            return Err("input_unknown".into());
                        }
                        std::thread::sleep(
                            Duration::from_millis(10)
                                .min(delay_end.saturating_duration_since(Instant::now())),
                        );
                    }
                    for chunk in part.data.chunks(4096) {
                        if write.uncertain.load(Ordering::Acquire)
                            || Instant::now() >= write.deadline
                        {
                            return Err("input_unknown".into());
                        }
                        writer.write_all(chunk).map_err(|error| error.to_string())?;
                    }
                    writer.flush().map_err(|error| error.to_string())?;
                }
                Ok(())
            })();
            let failed = result.is_err();
            if failed || write.uncertain.load(Ordering::Acquire) {
                warn!(%process_id, phase = "input_writer_settled_unknown", elapsed_ms = started.elapsed().as_millis() as u64,
                    bytes = write.writes.iter().map(|part| part.data.len()).sum::<usize>(), "owned PTY input writer settled; no retry");
            }
            let _ = write.async_result.send(result.clone());
            let _ = write.result.send(result);
            if failed {
                break;
            }
        }
    });
}

fn spawn_output_reader(
    runtime: Weak<RuntimeInner>,
    process: Arc<HostProcess>,
    mut reader: Box<dyn Read + Send>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        #[cfg(windows)]
        let mut cursor_query = ConPtyCursorQuery::default();
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) => {
                    if !process.stopping.load(Ordering::Acquire) {
                        warn!(%error, "process PTY reader failed");
                    }
                    break;
                }
            };
            // portable-pty requests PSEUDOCONSOLE_INHERIT_CURSOR. A headless
            // Host must answer the initial query itself, before a browser is
            // attached, or ConPTY blocks the child process's output.
            #[cfg(windows)]
            if cursor_query.observe(&buffer[..count]) {
                let (result, _) = std_mpsc::sync_channel(1);
                let (async_result, _) = tokio::sync::oneshot::channel();
                if let Ok(input) = process.input.lock() {
                    if let Some(input) = input.as_ref() {
                        let _ = input.try_send(InputWrite {
                            writes: vec![HostWrite {
                                data: b"\x1b[1;1R".to_vec(),
                                delay_ms: 0,
                            }],
                            deadline: Instant::now() + INPUT_DEADLINE,
                            uncertain: Arc::clone(&process.input_uncertain),
                            result,
                            async_result,
                        });
                    }
                }
            }
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            let process_id = process.info.lock().ok().map(|info| info.process_id.clone());
            let Some(process_id) = process_id else {
                continue;
            };
            let chunk_and_bounds = process.output.lock().ok().map(|mut output| {
                let chunk = output.push(&process_id, &buffer[..count]);
                (
                    chunk,
                    output.first_revision(),
                    output.next_revision,
                    output.bracketed_paste,
                )
            });
            let Some((chunk, first_revision, next_revision, bracketed_paste)) = chunk_and_bounds
            else {
                continue;
            };
            if let Ok(mut info) = process.info.lock() {
                info.last_output_at = chunk.emitted_at;
                info.first_revision = first_revision;
                info.next_revision = next_revision;
                info.bracketed_paste = bracketed_paste;
            }
            let _ = runtime.output_events.send(chunk);
        }
    });
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct ConPtyCursorQuery {
    matched: usize,
    answered: bool,
}

#[cfg(any(windows, test))]
impl ConPtyCursorQuery {
    fn observe(&mut self, bytes: &[u8]) -> bool {
        const QUERY: &[u8] = b"\x1b[6n";
        if self.answered {
            return false;
        }
        for &byte in bytes {
            if byte == QUERY[self.matched] {
                self.matched += 1;
                if self.matched == QUERY.len() {
                    self.answered = true;
                    return true;
                }
            } else {
                self.matched = usize::from(byte == QUERY[0]);
            }
        }
        false
    }
}

fn spawn_process_monitor(runtime: Weak<RuntimeInner>, process: Arc<HostProcess>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(PROCESS_POLL_INTERVAL);
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let exit = process.child.lock().ok().and_then(|mut child| {
            child
                .as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
        });
        if let Some(exit) = exit {
            let exit_code = i32::try_from(exit.exit_code()).ok();
            runtime.update_process(&process, |info| {
                info.running = false;
                info.exit_code = exit_code;
                info.exited_at = Some(Utc::now());
            });
            runtime.finish_process(&process);
            break;
        }
    });
}

fn relative_display(root: &Path, cwd: &Path) -> String {
    cwd.strip_prefix(root)
        .ok()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::Instant;

    #[test]
    #[ignore = "private raw-mode nonreader child for input isolation"]
    fn nonreader_fixture() {
        #[cfg(unix)]
        assert!(std::process::Command::new("/bin/stty")
            .args(["raw", "-echo"])
            .status()
            .unwrap()
            .success());
        println!("\nNONREADER_READY");
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(45));
    }

    #[tokio::test]
    async fn cancelled_async_input_reclaims_writer_after_owned_stop() {
        let root = std::env::temp_dir().join(format!("treer224-input-{}", Uuid::new_v4().simple()));
        std::fs::create_dir(&root).unwrap();
        let runtime = HostRuntime::new(&root).unwrap();
        runtime
            .spawn(HostSpawnRequest {
                process_id: "nonreader".into(),
                command: std::env::current_exe().unwrap().display().to_string(),
                args: vec![
                    "--exact".into(),
                    "tests::nonreader_fixture".into(),
                    "--ignored".into(),
                    "--nocapture".into(),
                ],
                cwd: ".".into(),
                env: BTreeMap::new(),
                cols: 80,
                rows: 24,
                metadata: String::new(),
            })
            .unwrap();
        wait_for_output_until(
            &runtime,
            "nonreader",
            "NONREADER_READY",
            Instant::now() + Duration::from_secs(5),
        );
        let process = runtime.get("nonreader").unwrap();
        let writer = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .write_async(
                        "nonreader",
                        &[HostWrite {
                            data: vec![b'x'; INPUT_LIMIT_BYTES],
                            delay_ms: 0,
                        }],
                    )
                    .await
            })
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        while !process.input_busy.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!writer.is_finished(), "real backpressure required");
        assert!(matches!(
            runtime.begin_write(
                "nonreader",
                &[HostWrite {
                    data: vec![b'y'],
                    delay_ms: 0
                }]
            ),
            Err(RuntimeError::Busy)
        ));
        writer.abort();
        assert!(writer.await.unwrap_err().is_cancelled());
        assert!(process.input_uncertain.load(Ordering::Acquire));
        assert!(!process.input_busy.load(Ordering::Acquire));
        assert!(matches!(
            runtime.begin_write(
                "nonreader",
                &[HostWrite {
                    data: vec![b'y'],
                    delay_ms: 0
                }]
            ),
            Err(RuntimeError::InputRefused)
        ));
        let info = runtime.stop("nonreader").unwrap();
        assert!(!info.running);
        assert!(info.exit_code.is_some());
        wait_for_resources_released(&process); // includes actual writer exit and its permit
        assert!(process.input_writer_finished.load(Ordering::Acquire));
        drop(process);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conpty_startup_cursor_query_handles_chunk_boundaries_once() {
        for split in 0..=4 {
            let mut query = ConPtyCursorQuery::default();
            let bytes = b"\x1b[6n";
            let first = query.observe(&bytes[..split]);
            let second = query.observe(&bytes[split..]);
            assert!(first ^ second);
            assert!(!query.observe(bytes));
        }
        let mut query = ConPtyCursorQuery::default();
        assert!(!query.observe(b"\x1b[31mhello\x1b["));
        assert!(!query.observe(b"0m\x1b"));
        assert!(query.observe(b"\x1b[6n"));
    }

    fn decoded(replay: &HostOutputReplay) -> String {
        String::from_utf8_lossy(
            &replay
                .chunks
                .iter()
                .flat_map(|chunk| chunk.data.iter().copied())
                .collect::<Vec<_>>(),
        )
        .into_owned()
    }

    #[cfg(unix)]
    fn wait_for_output(runtime: &HostRuntime, process_id: &str, needle: &str) -> HostOutputReplay {
        let deadline = Instant::now() + Duration::from_secs(5);
        wait_for_output_until(runtime, process_id, needle, deadline)
    }

    fn wait_for_output_until(
        runtime: &HostRuntime,
        process_id: &str,
        needle: &str,
        deadline: Instant,
    ) -> HostOutputReplay {
        loop {
            let replay = runtime.read(process_id, None).expect("read output");
            if decoded(&replay).contains(needle) {
                return replay;
            }
            assert!(
                Instant::now() < deadline,
                "process output did not contain {needle:?}; received {:?}; processes {:?}",
                decoded(&replay),
                runtime.list(),
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_resources_released(process: &HostProcess) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let exited = process
                .info
                .lock()
                .expect("process info")
                .exited_at
                .is_some();
            let input_released = process.input.lock().expect("process input").is_none();
            let child_released = process.child.lock().expect("process child").is_none();
            let master_released = process.master.lock().expect("process master").is_none();
            if exited
                && input_released
                && child_released
                && master_released
                && process.input_writer_finished.load(Ordering::Acquire)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "exited process resources were not released"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_conpty_argv_input_resize_and_stop() {
        native_pty_argv_input_resize_and_stop();
    }

    #[cfg(unix)]
    #[test]
    fn unix_native_fixture_argv_input_resize_and_stop() {
        native_pty_argv_input_resize_and_stop();
    }

    #[test]
    fn polluted_host_environment_does_not_leak_into_workloads() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::polluted_host_environment_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("AGENT_UI_INSTANCE_ID", "old-ui-instance")
            .env("CODEX_UI_THREAD_ID", "old-thread")
            .env("TREER_WORKLOAD_CREDENTIAL", "old-credential")
            .env("TREER_AGENT_ID", "old-agent")
            .env("TREER_MACHINE_TOKEN", "old-machine-token")
            .env("TREER_OPERATOR_CREDENTIAL", "old-operator-credential")
            .env("TREER_ENROLLMENT_KEY", "old-enrollment-key")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "subprocess helper, intentionally polluted Host runtime"]
    fn polluted_host_environment_fixture() {
        let root = std::env::temp_dir().join(format!("trr-env-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let runtime = HostRuntime::new(&root).unwrap();
        runtime
            .spawn(HostSpawnRequest {
                process_id: "environment-boundary".into(),
                command: std::env::current_exe().unwrap().display().to_string(),
                args: vec![
                    "--exact".into(),
                    "tests::clean_workload_environment_fixture".into(),
                    "--ignored".into(),
                    "--nocapture".into(),
                ],
                cwd: ".".into(),
                env: std::collections::BTreeMap::from([
                    ("TREER_AGENT_ID".into(), "new-agent".into()),
                    ("TREER_WORKLOAD_CREDENTIAL".into(), "new-credential".into()),
                    ("HTTP_PROXY".into(), "http://127.0.0.1:12345".into()),
                ]),
                cols: 80,
                rows: 24,
                metadata: "{}".into(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let info = runtime
                .list()
                .into_iter()
                .find(|p| p.process_id == "environment-boundary")
                .unwrap();
            if !info.running {
                assert_eq!(info.exit_code, Some(0));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "workload environment fixture timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "subprocess helper, validates the fresh workload identity"]
    fn clean_workload_environment_fixture() {
        for name in [
            "AGENT_UI_INSTANCE_ID",
            "CODEX_UI_THREAD_ID",
            "TREER_MACHINE_TOKEN",
            "TREER_OPERATOR_CREDENTIAL",
            "TREER_ENROLLMENT_KEY",
            "ALL_PROXY",
        ] {
            assert!(std::env::var_os(name).is_none(), "inherited {name}");
        }
        #[cfg(windows)]
        for name in [
            "SystemRoot",
            "windir",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            assert!(
                std::env::var_os(name).is_some(),
                "missing Windows baseline {name}"
            );
        }
        assert_eq!(std::env::var("TREER_AGENT_ID").unwrap(), "new-agent");
        assert_eq!(
            std::env::var("TREER_WORKLOAD_CREDENTIAL").unwrap(),
            "new-credential"
        );
        assert_eq!(
            std::env::var("HTTP_PROXY").unwrap(),
            "http://127.0.0.1:12345"
        );
    }

    // Run this Rust test executable as the workload rather than measuring
    // PowerShell/.NET's first-use startup in a low-level ConPTY test. Keep the
    // child alive until stop, and announce readiness before accepting input.
    #[test]
    #[ignore = "child fixture, invoked only by the native PTY test"]
    fn native_pty_child_fixture() {
        assert_eq!(std::env::var("TREER_TEST_VALUE").unwrap(), "environment-ok");
        let args: Vec<_> = std::env::args().collect();
        assert_eq!(&args[args.len() - 2..], ["hello world $ &", "空间 参数"]);
        println!("\nargv:{}", args[args.len() - 2]);
        println!("unicode:{}", args[args.len() - 1]);
        println!("environment:{}", std::env::var("TREER_TEST_VALUE").unwrap());
        println!("ready-for-input");
        std::io::stdout().flush().unwrap();
        let mut value = String::new();
        assert!(std::io::stdin().read_line(&mut value).unwrap() > 0);
        println!("echo:{}", value.trim_end_matches(['\r', '\n']));
        std::io::stdout().flush().unwrap();
        loop {
            value.clear();
            assert!(std::io::stdin().read_line(&mut value).unwrap() > 0);
        }
    }

    fn native_pty_argv_input_resize_and_stop() {
        let root = std::env::temp_dir().join(format!("treer 空格 {}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("agent test.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let runtime = HostRuntime::new(&root).unwrap();
        let request = HostSpawnRequest {
            process_id: "windows-agent".into(),
            command: executable.to_string_lossy().into_owned(),
            args: vec![
                "--ignored".into(),
                "--exact".into(),
                "--nocapture".into(),
                "tests::native_pty_child_fixture".into(),
                "--".into(),
                "hello world $ &".into(),
                "空间 参数".into(),
            ],
            cwd: ".".into(),
            env: BTreeMap::from([("TREER_TEST_VALUE".into(), "environment-ok".into())]),
            cols: 80,
            rows: 24,
            metadata: "{}".into(),
        };
        runtime.spawn(request.clone()).unwrap();
        assert!(matches!(
            runtime.spawn(request),
            Err(RuntimeError::ProcessExists(_))
        ));
        // One overall deadline, not a fixed startup sleep or a fresh timeout
        // for each assertion. Loaded runners may need time to schedule the
        // child; the ready marker, not elapsed time, gates input.
        let deadline = Instant::now() + Duration::from_secs(15);
        for needle in [
            "argv:hello world $ &",
            "unicode:空间 参数",
            "environment:environment-ok",
            "ready-for-input",
        ] {
            wait_for_output_until(&runtime, "windows-agent", needle, deadline);
        }
        runtime.resize("windows-agent", 100, 35).unwrap();
        let process = runtime.get("windows-agent").unwrap();
        let size = process
            .master
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .get_size()
            .unwrap();
        assert_eq!((size.cols, size.rows), (100, 35));
        runtime
            .write(
                "windows-agent",
                &[HostWrite {
                    data: b"input-ok\r\n".to_vec(),
                    delay_ms: 0,
                }],
            )
            .unwrap();
        wait_for_output_until(&runtime, "windows-agent", "echo:input-ok", deadline);
        assert!(process.info.lock().unwrap().running);
        let stopped = runtime.stop("windows-agent").unwrap();
        assert!(!stopped.running);
        assert!(stopped.exited_at.is_some());
        wait_for_resources_released(&process);
        assert!(!runtime.list()[0].running);
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn output_replay_is_incremental_without_a_gap() {
        let mut output = OutputBuffer::new();
        let first = output.push("p1", b"one");
        let second = output.push("p1", b"two");
        let replay = output.replay(
            "p1",
            Some(&OutputCursor {
                stream_epoch: first.stream_epoch.clone(),
                revision: first.revision,
            }),
        );
        assert!(!replay.gap);
        assert_eq!(replay.chunks, vec![second]);
        assert_eq!(replay.stream_epoch, first.stream_epoch);
    }

    #[test]
    fn output_replay_from_the_tip_is_empty_without_a_gap() {
        let mut output = OutputBuffer::new();
        let last = output.push("p1", b"one");
        let replay = output.replay(
            "p1",
            Some(&OutputCursor {
                stream_epoch: last.stream_epoch.clone(),
                revision: last.revision,
            }),
        );
        assert!(!replay.gap);
        assert!(replay.chunks.is_empty());
    }

    #[test]
    fn output_replay_reports_a_gap_across_epochs() {
        let mut output = OutputBuffer::new();
        output.push("p1", b"one");
        let replay = output.replay(
            "p1",
            Some(&OutputCursor {
                stream_epoch: "other_stream".to_string(),
                revision: 1,
            }),
        );
        assert!(replay.gap);
        assert_eq!(decoded(&replay), "one");
    }

    #[test]
    fn output_replay_reports_gaps() {
        let mut output = OutputBuffer::new();
        output.push("p1", &vec![b'x'; OUTPUT_LIMIT_BYTES]);
        let first = output.push("p1", b"new");
        let replay = output.replay(
            "p1",
            Some(&OutputCursor {
                stream_epoch: output.stream_epoch.clone(),
                revision: 0,
            }),
        );
        assert!(replay.gap);
        assert_eq!(replay.chunks, vec![first]);
    }

    #[test]
    fn tracks_bracketed_paste_across_chunks() {
        let mut output = OutputBuffer::new();
        output.push("p1", b"before\x1b[?20");
        assert!(!output.bracketed_paste);
        output.push("p1", b"04hafter");
        assert!(output.bracketed_paste);
        output.push("p1", b"\x1b[?2004l");
        assert!(!output.bracketed_paste);
    }

    #[test]
    fn completed_process_history_is_bounded() {
        let mut completed = VecDeque::new();
        assert_eq!(push_bounded(&mut completed, "p1".to_string(), 2), None);
        assert_eq!(push_bounded(&mut completed, "p2".to_string(), 2), None);
        assert_eq!(
            push_bounded(&mut completed, "p3".to_string(), 2),
            Some("p1".to_string())
        );
        assert_eq!(
            completed,
            VecDeque::from(["p2".to_string(), "p3".to_string()])
        );
    }

    #[test]
    #[cfg(unix)]
    fn exited_process_releases_live_resources_and_keeps_replay() {
        let temporary = std::env::temp_dir().join(format!(
            "treer-host-runtime-cleanup-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temporary).expect("create temporary directory");
        let runtime = HostRuntime::new(&temporary).expect("create runtime");
        runtime
            .spawn(HostSpawnRequest {
                process_id: "p1".to_string(),
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "printf cleanup-ok".to_string()],
                cwd: ".".to_string(),
                env: BTreeMap::new(),
                cols: 80,
                rows: 24,
                metadata: json!({"opaque": true}).to_string(),
            })
            .expect("spawn process");
        wait_for_output(&runtime, "p1", "cleanup-ok");

        let process = runtime.get("p1").expect("process record");
        wait_for_resources_released(&process);

        let replay = runtime.read("p1", None).expect("read retained output");
        assert!(decoded(&replay).contains("cleanup-ok"));
        std::fs::remove_dir_all(temporary).expect("remove temporary directory");
    }

    #[test]
    #[cfg(unix)]
    fn stopped_process_releases_live_resources() {
        let temporary = std::env::temp_dir().join(format!(
            "treer-host-runtime-stop-cleanup-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temporary).expect("create temporary directory");
        let runtime = HostRuntime::new(&temporary).expect("create runtime");
        runtime
            .spawn(HostSpawnRequest {
                process_id: "p1".to_string(),
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "printf ready; exec cat".to_string()],
                cwd: ".".to_string(),
                env: BTreeMap::new(),
                cols: 80,
                rows: 24,
                metadata: json!({"opaque": true}).to_string(),
            })
            .expect("spawn process");
        wait_for_output(&runtime, "p1", "ready");

        let process = runtime.get("p1").expect("process record");
        runtime.stop("p1").expect("stop process");
        wait_for_resources_released(&process);
        std::fs::remove_dir_all(temporary).expect("remove temporary directory");
    }

    #[test]
    #[cfg(unix)]
    fn spawns_and_reads_a_real_pty() {
        let temporary = std::env::temp_dir().join(format!(
            "treer-host-runtime-test-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temporary).expect("create temporary directory");
        let runtime = HostRuntime::new(&temporary).expect("create runtime");
        runtime
            .spawn(HostSpawnRequest {
                process_id: "p1".to_string(),
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "printf host-runtime-ok".to_string()],
                cwd: ".".to_string(),
                env: BTreeMap::new(),
                cols: 80,
                rows: 24,
                metadata: json!({"opaque": true}).to_string(),
            })
            .expect("spawn process");
        wait_for_output(&runtime, "p1", "host-runtime-ok");
    }

    #[test]
    #[cfg(unix)]
    fn recovery_replaces_terminal_process_but_rejects_live_duplicate() {
        let temporary = std::env::temp_dir().join(format!(
            "treer-recovery-runtime-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temporary).unwrap();
        let runtime = HostRuntime::new(&temporary).unwrap();
        let mut request = HostSpawnRequest {
            process_id: "stable-agent".into(),
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 1".into()],
            cwd: ".".into(),
            env: BTreeMap::new(),
            cols: 80,
            rows: 24,
            metadata: "{}".into(),
        };
        runtime.spawn(request.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.list()[0].running {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        request.args = vec!["-c".into(), "sleep 30".into()];
        let restored = runtime.spawn(request.clone()).unwrap();
        assert_eq!(restored.process_id, "stable-agent");
        assert!(restored.running);
        assert!(matches!(
            runtime.spawn(request),
            Err(RuntimeError::ProcessExists(_))
        ));
        runtime.stop("stable-agent").unwrap();
        drop(runtime);
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn real_pty_replay_is_incremental_from_a_cursor() {
        let temporary = std::env::temp_dir().join(format!(
            "treer-host-runtime-replay-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temporary).expect("create temporary directory");
        let runtime = HostRuntime::new(&temporary).expect("create runtime");
        runtime
            .spawn(HostSpawnRequest {
                process_id: "p1".to_string(),
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "printf first-chunk; exec cat".to_string()],
                cwd: ".".to_string(),
                env: BTreeMap::new(),
                cols: 80,
                rows: 24,
                metadata: json!({"opaque": true}).to_string(),
            })
            .expect("spawn process");
        let first = wait_for_output(&runtime, "p1", "first-chunk");
        let cursor = OutputCursor {
            stream_epoch: first.stream_epoch.clone(),
            revision: first.next_revision.saturating_sub(1),
        };
        runtime
            .write(
                "p1",
                &[HostWrite {
                    data: b"second-chunk\n".to_vec(),
                    delay_ms: 0,
                }],
            )
            .expect("write later output");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let replay = runtime
                .read("p1", Some(&cursor))
                .expect("read incremental output");
            let text = decoded(&replay);
            if text.contains("second-chunk") {
                assert!(!replay.gap);
                assert!(
                    !text.contains("first-chunk"),
                    "incremental replay must not repeat bytes already held by the cursor: {text:?}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "incremental process output did not arrive"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
