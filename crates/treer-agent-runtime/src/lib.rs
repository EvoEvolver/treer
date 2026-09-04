use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use chrono::Utc;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::warn;
use treer_host_protocol::{
    HostOutputChunk, HostOutputReplay, HostProcessInfo, HostSpawnRequest, HostWrite, OutputCursor,
};
use uuid::Uuid;

const OUTPUT_LIMIT_BYTES: usize = 512 * 1024;
const COMPLETED_PROCESS_LIMIT: usize = 256;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
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
    completed_processes: Mutex<VecDeque<String>>,
    process_events: broadcast::Sender<HostProcessInfo>,
    output_events: broadcast::Sender<HostOutputChunk>,
}

struct HostProcess {
    info: Mutex<HostProcessInfo>,
    input: Mutex<Option<std_mpsc::Sender<InputWrite>>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    output: Mutex<OutputBuffer>,
    stopping: AtomicBool,
}

struct InputWrite {
    data: Vec<u8>,
    delay: Duration,
    result: std_mpsc::SyncSender<Result<(), String>>,
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
        let mut processes =
            self.inner.processes.write().map_err(|_| {
                RuntimeError::Terminal("process registry lock poisoned".to_string())
            })?;
        if processes.contains_key(&request.process_id) {
            return Err(RuntimeError::ProcessExists(request.process_id));
        }
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
        for (key, value) in request.env {
            builder.env(key, value);
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| RuntimeError::Spawn(error.to_string()))?;
        drop(pair.slave);
        let pid = child.process_id();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        let (input, input_rx) = std_mpsc::channel();
        spawn_input_writer(writer, input_rx);
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
            info: Mutex::new(info.clone()),
            input: Mutex::new(Some(input)),
            child: Mutex::new(Some(child)),
            master: Mutex::new(Some(pair.master)),
            output: Mutex::new(output),
            stopping: AtomicBool::new(false),
        });
        processes.insert(request.process_id, Arc::clone(&process));
        drop(processes);
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
        if writes.is_empty() {
            return Err(RuntimeError::InvalidRequest(
                "writes must not be empty".to_string(),
            ));
        }
        let process = self.get_running(process_id)?;
        for write in writes {
            let (result, result_rx) = std_mpsc::sync_channel(1);
            let input = process
                .input
                .lock()
                .map_err(|_| RuntimeError::Terminal("terminal input lock poisoned".to_string()))?
                .as_ref()
                .cloned()
                .ok_or_else(|| RuntimeError::ProcessNotRunning(process_id.to_string()))?;
            input
                .send(InputWrite {
                    data: write.data.clone(),
                    delay: Duration::from_millis(write.delay_ms),
                    result,
                })
                .map_err(|_| RuntimeError::Terminal("terminal input queue closed".to_string()))?;
            result_rx
                .recv()
                .map_err(|_| RuntimeError::Terminal("terminal input writer stopped".to_string()))?
                .map_err(RuntimeError::Terminal)?;
        }
        process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("process state lock poisoned".to_string()))
            .map(|info| info.clone())
    }

    pub fn resize(
        &self,
        process_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<HostProcessInfo, RuntimeError> {
        let process = self.get_running(process_id)?;
        let master = process
            .master
            .lock()
            .map_err(|_| RuntimeError::Terminal("terminal lock poisoned".to_string()))?;
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
        process.stopping.store(true, Ordering::Release);
        process
            .child
            .lock()
            .map_err(|_| RuntimeError::Terminal("child lock poisoned".to_string()))?
            .as_mut()
            .ok_or_else(|| RuntimeError::ProcessNotRunning(process_id.to_string()))?
            .kill()
            .map_err(|error| RuntimeError::Terminal(error.to_string()))?;
        self.inner
            .update_process(&process, |info| {
                info.running = false;
                info.exited_at = Some(Utc::now());
            })
            .ok_or_else(|| RuntimeError::Terminal("process state lock poisoned".to_string()))
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
        let evicted = self
            .completed_processes
            .lock()
            .ok()
            .and_then(|mut completed| {
                push_bounded(&mut completed, process_id, COMPLETED_PROCESS_LIMIT)
            });
        if let Some(evicted) = evicted {
            if let Ok(mut processes) = self.processes.write() {
                processes.remove(&evicted);
            }
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

fn spawn_input_writer(mut writer: Box<dyn Write + Send>, input: std_mpsc::Receiver<InputWrite>) {
    std::thread::spawn(move || {
        while let Ok(write) = input.recv() {
            if !write.delay.is_zero() {
                std::thread::sleep(write.delay);
            }
            let result = writer
                .write_all(&write.data)
                .and_then(|_| writer.flush())
                .map_err(|error| error.to_string());
            let failed = result.is_err();
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
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::Instant;

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

    fn wait_for_output(runtime: &HostRuntime, process_id: &str, needle: &str) -> HostOutputReplay {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let replay = runtime.read(process_id, None).expect("read output");
            if decoded(&replay).contains(needle) {
                return replay;
            }
            assert!(Instant::now() < deadline, "process output did not arrive");
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
            if exited && input_released && child_released && master_released {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "exited process resources were not released"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
