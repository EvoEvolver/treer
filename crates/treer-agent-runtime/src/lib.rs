use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use chrono::Utc;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::warn;
use treer_protocol::{
    AgentInfo, AgentStatus, CreateAgentRequest, ProtocolError, ReadAgentOutputResponse,
};

const OUTPUT_LIMIT_BYTES: usize = 512 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const QUIET_IDLE_AFTER: Duration = Duration::from_millis(900);
const OUTPUT_METADATA_INTERVAL: Duration = Duration::from_millis(150);
const PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);
const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("agent {0} already exists")]
    AgentExists(String),
    #[error("agent {0} was not found")]
    AgentNotFound(String),
    #[error("agent {0} is no longer running")]
    AgentNotRunning(String),
    #[error("invalid working directory: {0}")]
    InvalidCwd(String),
    #[error("invalid agent request: {0}")]
    InvalidRequest(String),
    #[error("failed to start agent: {0}")]
    Spawn(String),
    #[error("terminal operation failed: {0}")]
    Terminal(String),
}

impl RuntimeError {
    pub fn protocol_error(&self) -> ProtocolError {
        let code = match self {
            Self::AgentExists(_) => "agent_exists",
            Self::AgentNotFound(_) => "agent_not_found",
            Self::AgentNotRunning(_) => "agent_not_running",
            Self::InvalidCwd(_) => "invalid_cwd",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Spawn(_) => "spawn_failed",
            Self::Terminal(_) => "terminal_error",
        };
        ProtocolError::new(code, self.to_string())
    }
}

#[derive(Clone)]
pub struct AgentRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    workspace_id: String,
    server_id: String,
    agent_server_url: String,
    treer_binary: Option<PathBuf>,
    root: PathBuf,
    agents: RwLock<HashMap<String, Arc<AgentProcess>>>,
    events: broadcast::Sender<AgentInfo>,
    terminal_events: broadcast::Sender<TerminalOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalOutput {
    pub agent_id: String,
    pub data: Vec<u8>,
}

struct AgentProcess {
    info: Mutex<AgentInfo>,
    input: std_mpsc::Sender<InputWrite>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    output: Mutex<OutputBuffer>,
    last_output: Mutex<Instant>,
    last_metadata_event: Mutex<Instant>,
    bracketed_paste: AtomicBool,
    stopping: AtomicBool,
}

struct InputWrite {
    data: Vec<u8>,
    delay: Duration,
    result: Option<std_mpsc::SyncSender<Result<(), String>>>,
}

struct OutputBuffer {
    text: String,
    raw: Vec<u8>,
    mode_tail: Vec<u8>,
    bracketed_paste: bool,
    truncated: bool,
}

impl OutputBuffer {
    fn new() -> Self {
        Self {
            text: String::new(),
            raw: Vec::new(),
            mode_tail: Vec::new(),
            bracketed_paste: false,
            truncated: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.update_terminal_modes(bytes);
        self.raw.extend_from_slice(bytes);
        if self.raw.len() > OUTPUT_LIMIT_BYTES {
            let split = self.raw.len().saturating_sub(OUTPUT_LIMIT_BYTES);
            self.raw.drain(..split);
            self.truncated = true;
        }
        let plain = strip_ansi_escapes::strip(bytes);
        self.text.push_str(&String::from_utf8_lossy(&plain));
        if self.text.len() <= OUTPUT_LIMIT_BYTES {
            return;
        }
        let mut split = self.text.len().saturating_sub(OUTPUT_LIMIT_BYTES);
        while split < self.text.len() && !self.text.is_char_boundary(split) {
            split += 1;
        }
        self.text.drain(..split);
        self.truncated = true;
    }

    fn read(&self, lines: Option<usize>) -> (String, bool) {
        let Some(lines) = lines else {
            return (self.text.clone(), self.truncated);
        };
        if lines == 0 {
            return (String::new(), self.truncated || !self.text.is_empty());
        }
        let line_count = self.text.lines().count();
        let text = if line_count <= lines {
            self.text.clone()
        } else {
            self.text
                .lines()
                .skip(line_count - lines)
                .collect::<Vec<_>>()
                .join("\n")
        };
        (text, self.truncated || line_count > lines)
    }

    fn raw_snapshot(&self) -> Vec<u8> {
        self.raw.clone()
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

impl AgentRuntime {
    pub fn new(
        workspace_id: impl Into<String>,
        server_id: impl Into<String>,
        agent_server_url: impl Into<String>,
        treer_binary: Option<PathBuf>,
        root: impl AsRef<Path>,
    ) -> Result<Self, RuntimeError> {
        let root = std::fs::canonicalize(root.as_ref())
            .map_err(|err| RuntimeError::InvalidCwd(err.to_string()))?;
        if !root.is_dir() {
            return Err(RuntimeError::InvalidCwd(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let (events, _) = broadcast::channel(512);
        let (terminal_events, _) = broadcast::channel(2048);
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                workspace_id: workspace_id.into(),
                server_id: server_id.into(),
                agent_server_url: agent_server_url.into(),
                treer_binary,
                root,
                agents: RwLock::new(HashMap::new()),
                events,
                terminal_events,
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentInfo> {
        self.inner.events.subscribe()
    }

    pub fn subscribe_terminal(&self) -> broadcast::Receiver<TerminalOutput> {
        self.inner.terminal_events.subscribe()
    }

    pub fn list(&self) -> Vec<AgentInfo> {
        let Ok(agents) = self.inner.agents.read() else {
            return Vec::new();
        };
        let mut result: Vec<_> = agents
            .values()
            .filter_map(|agent| agent.info.lock().ok().map(|info| info.clone()))
            .collect();
        result.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        result
    }

    pub fn create(
        &self,
        agent_id: String,
        request: CreateAgentRequest,
    ) -> Result<AgentInfo, RuntimeError> {
        if request.name.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest(
                "agent name cannot be empty".to_string(),
            ));
        }
        if self
            .inner
            .agents
            .read()
            .ok()
            .is_some_and(|agents| agents.contains_key(&agent_id))
        {
            return Err(RuntimeError::AgentExists(agent_id));
        }
        let cwd = self.resolve_cwd(&request.cwd)?;
        let (command, args) = resolve_command(&request)?;
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: request.rows.max(1),
                cols: request.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| RuntimeError::Terminal(err.to_string()))?;
        let mut builder = CommandBuilder::new(command);
        builder.args(args);
        builder.cwd(&cwd);
        builder.env("TREER_WORKSPACE_ID", &self.inner.workspace_id);
        builder.env("TREER_SERVER_ID", &self.inner.server_id);
        builder.env("TREER_AGENT_ID", &agent_id);
        builder.env("TREER_AGENT_SERVER_URL", &self.inner.agent_server_url);
        if let Some(treer_binary) = &self.inner.treer_binary {
            builder.env("TREER_BIN", treer_binary);
            if let Some(bin_dir) = treer_binary.parent() {
                let mut paths = vec![bin_dir.to_path_buf()];
                if let Some(current_path) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&current_path));
                }
                if let Ok(path) = std::env::join_paths(paths) {
                    builder.env("PATH", path);
                }
            }
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
        drop(pair.slave);
        let pid = child.process_id();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| RuntimeError::Terminal(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| RuntimeError::Terminal(err.to_string()))?;
        let (input, input_rx) = std_mpsc::channel();
        spawn_input_writer(writer, input_rx);
        let now = Utc::now();
        let info = AgentInfo {
            agent_id: agent_id.clone(),
            workspace_id: self.inner.workspace_id.clone(),
            server_id: self.inner.server_id.clone(),
            kind: request.kind,
            name: request.name,
            cwd: relative_display(&self.inner.root, &cwd),
            status: AgentStatus::Starting,
            pid,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
        };
        let process = Arc::new(AgentProcess {
            info: Mutex::new(info.clone()),
            input,
            child: Mutex::new(child),
            master: Mutex::new(pair.master),
            output: Mutex::new(OutputBuffer::new()),
            last_output: Mutex::new(Instant::now()),
            last_metadata_event: Mutex::new(Instant::now()),
            bracketed_paste: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        });
        self.inner
            .agents
            .write()
            .map_err(|_| RuntimeError::Terminal("agent registry lock poisoned".to_string()))?
            .insert(agent_id, Arc::clone(&process));
        let _ = self.inner.events.send(info.clone());
        spawn_output_reader(Arc::downgrade(&self.inner), Arc::clone(&process), reader);
        spawn_process_monitor(Arc::downgrade(&self.inner), process);
        Ok(info)
    }

    pub fn prompt(&self, agent_id: &str, text: &str) -> Result<AgentInfo, RuntimeError> {
        if text.is_empty() {
            return Err(RuntimeError::InvalidRequest(
                "agent prompt cannot be empty".to_string(),
            ));
        }
        let process = self.get_running(agent_id)?;
        let bracketed = process.bracketed_paste.load(Ordering::Acquire);
        self.queue_input(
            &process,
            encode_prompt_text(text, bracketed),
            Duration::ZERO,
            true,
        )?;
        self.queue_input(&process, vec![b'\r'], PROMPT_SUBMIT_DELAY, false)?;
        self.mark_working(&process)
    }

    pub fn write_raw(&self, agent_id: &str, data: &[u8]) -> Result<AgentInfo, RuntimeError> {
        let process = self.get_running(agent_id)?;
        self.queue_input(&process, data.to_vec(), Duration::ZERO, true)?;
        self.mark_working(&process)
    }

    fn get_running(&self, agent_id: &str) -> Result<Arc<AgentProcess>, RuntimeError> {
        let process = self.get(agent_id)?;
        let is_terminal = process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("agent state lock poisoned".to_string()))?
            .status
            .is_terminal();
        if is_terminal {
            return Err(RuntimeError::AgentNotRunning(agent_id.to_string()));
        }
        Ok(process)
    }

    fn queue_input(
        &self,
        process: &AgentProcess,
        data: Vec<u8>,
        delay: Duration,
        wait_for_result: bool,
    ) -> Result<(), RuntimeError> {
        let (result, result_rx) = if wait_for_result {
            let (sender, receiver) = std_mpsc::sync_channel(1);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        process
            .input
            .send(InputWrite {
                data,
                delay,
                result,
            })
            .map_err(|_| RuntimeError::Terminal("terminal input queue closed".to_string()))?;
        if let Some(result_rx) = result_rx {
            result_rx
                .recv()
                .map_err(|_| RuntimeError::Terminal("terminal input writer stopped".to_string()))?
                .map_err(RuntimeError::Terminal)?;
        }
        Ok(())
    }

    fn mark_working(&self, process: &AgentProcess) -> Result<AgentInfo, RuntimeError> {
        self.inner
            .update_agent(process, |info| info.status = AgentStatus::Working)
            .ok_or_else(|| RuntimeError::Terminal("agent state lock poisoned".to_string()))
    }

    pub fn read(
        &self,
        agent_id: &str,
        lines: Option<usize>,
    ) -> Result<ReadAgentOutputResponse, RuntimeError> {
        let process = self.get(agent_id)?;
        let revision = process
            .info
            .lock()
            .map_err(|_| RuntimeError::Terminal("agent state lock poisoned".to_string()))?
            .output_revision;
        let (text, truncated) = process
            .output
            .lock()
            .map_err(|_| RuntimeError::Terminal("output lock poisoned".to_string()))?
            .read(lines);
        Ok(ReadAgentOutputResponse {
            agent_id: agent_id.to_string(),
            revision,
            text,
            truncated,
        })
    }

    pub fn terminal_snapshot(&self, agent_id: &str) -> Result<Vec<u8>, RuntimeError> {
        let process = self.get(agent_id)?;
        let snapshot = process
            .output
            .lock()
            .map_err(|_| RuntimeError::Terminal("output lock poisoned".to_string()))?
            .raw_snapshot();
        Ok(snapshot)
    }

    pub fn stop(&self, agent_id: &str) -> Result<AgentInfo, RuntimeError> {
        let process = self.get(agent_id)?;
        process.stopping.store(true, Ordering::Release);
        process
            .child
            .lock()
            .map_err(|_| RuntimeError::Terminal("child lock poisoned".to_string()))?
            .kill()
            .map_err(|err| RuntimeError::Terminal(err.to_string()))?;
        self.inner
            .update_agent(&process, |info| info.status = AgentStatus::Exited)
            .ok_or_else(|| RuntimeError::Terminal("agent state lock poisoned".to_string()))
    }

    pub fn resize(&self, agent_id: &str, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        let process = self.get(agent_id)?;
        let result = process
            .master
            .lock()
            .map_err(|_| RuntimeError::Terminal("terminal lock poisoned".to_string()))?
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| RuntimeError::Terminal(err.to_string()));
        result
    }

    fn get(&self, agent_id: &str) -> Result<Arc<AgentProcess>, RuntimeError> {
        self.inner
            .agents
            .read()
            .ok()
            .and_then(|agents| agents.get(agent_id).cloned())
            .ok_or_else(|| RuntimeError::AgentNotFound(agent_id.to_string()))
    }

    fn resolve_cwd(&self, requested: &str) -> Result<PathBuf, RuntimeError> {
        let requested = if requested.trim().is_empty() {
            Path::new(".")
        } else {
            Path::new(requested)
        };
        if requested.is_absolute() {
            return Err(RuntimeError::InvalidCwd(
                "cwd must be relative to the workspace root".to_string(),
            ));
        }
        let cwd = std::fs::canonicalize(self.inner.root.join(requested))
            .map_err(|err| RuntimeError::InvalidCwd(err.to_string()))?;
        if !cwd.starts_with(&self.inner.root) {
            return Err(RuntimeError::InvalidCwd(
                "cwd resolves outside the workspace root".to_string(),
            ));
        }
        Ok(cwd)
    }
}

fn encode_prompt_text(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return text.as_bytes().to_vec();
    }
    let mut encoded = Vec::with_capacity(text.len() + 12);
    encoded.extend_from_slice(b"\x1b[200~");
    encoded.extend_from_slice(text.as_bytes());
    encoded.extend_from_slice(b"\x1b[201~");
    encoded
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
            if let Some(sender) = write.result {
                let _ = sender.send(result);
            } else if let Err(error) = result {
                warn!(%error, "delayed terminal input failed");
            }
            if failed {
                break;
            }
        }
    });
}

impl RuntimeInner {
    fn update_agent(
        &self,
        process: &AgentProcess,
        mutation: impl FnOnce(&mut AgentInfo),
    ) -> Option<AgentInfo> {
        let mut info = process.info.lock().ok()?;
        mutation(&mut info);
        info.updated_at = Utc::now();
        let snapshot = info.clone();
        drop(info);
        if let Ok(mut last_event) = process.last_metadata_event.lock() {
            *last_event = Instant::now();
        }
        let _ = self.events.send(snapshot.clone());
        Some(snapshot)
    }

    fn update_from_output(&self, process: &AgentProcess, detected: AgentStatus) {
        let Some((snapshot, status_changed)) = process.info.lock().ok().map(|mut info| {
            let previous_status = info.status;
            info.output_revision = info.output_revision.saturating_add(1);
            if !info.status.is_terminal() {
                info.status = detected;
            }
            info.updated_at = Utc::now();
            (info.clone(), info.status != previous_status)
        }) else {
            return;
        };
        let should_emit = process
            .last_metadata_event
            .lock()
            .ok()
            .is_some_and(|mut last_event| {
                if status_changed || last_event.elapsed() >= OUTPUT_METADATA_INTERVAL {
                    *last_event = Instant::now();
                    true
                } else {
                    false
                }
            });
        if should_emit {
            let _ = self.events.send(snapshot);
        }
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let Ok(agents) = self.agents.read() else {
            return;
        };
        for process in agents.values() {
            process.stopping.store(true, Ordering::Release);
            if let Ok(mut child) = process.child.lock() {
                if let Err(err) = child.kill() {
                    warn!(%err, "failed to stop child while dropping runtime");
                }
            }
        }
    }
}

fn spawn_output_reader(
    runtime: Weak<RuntimeInner>,
    process: Arc<AgentProcess>,
    mut reader: Box<dyn Read + Send>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(err) => {
                    if !process.stopping.load(Ordering::Acquire) {
                        warn!(%err, "agent PTY reader failed");
                    }
                    break;
                }
            };
            let chunk = &buffer[..count];
            if let Ok(mut output) = process.output.lock() {
                output.push(chunk);
                process
                    .bracketed_paste
                    .store(output.bracketed_paste, Ordering::Release);
            }
            if let Ok(mut last_output) = process.last_output.lock() {
                *last_output = Instant::now();
            }
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            let agent_id = process.info.lock().ok().map(|info| info.agent_id.clone());
            if let Some(agent_id) = agent_id {
                let _ = runtime.terminal_events.send(TerminalOutput {
                    agent_id,
                    data: chunk.to_vec(),
                });
            }
            let recent = process
                .output
                .lock()
                .ok()
                .map(|output| output.read(Some(20)).0)
                .unwrap_or_default();
            let detected = detect_status(&recent).unwrap_or(AgentStatus::Working);
            runtime.update_from_output(&process, detected);
        }
    });
}

fn spawn_process_monitor(runtime: Weak<RuntimeInner>, process: Arc<AgentProcess>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(PROCESS_POLL_INTERVAL);
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        let exit = process
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten());
        if let Some(exit) = exit {
            let exit_code = i32::try_from(exit.exit_code()).ok();
            runtime.update_agent(&process, |info| {
                info.status = AgentStatus::Exited;
                info.exit_code = exit_code;
                info.exited_at = Some(Utc::now());
            });
            break;
        }

        let quiet = process
            .last_output
            .lock()
            .ok()
            .is_some_and(|last| last.elapsed() >= QUIET_IDLE_AFTER);
        if quiet {
            let should_idle = process.info.lock().ok().is_some_and(|info| {
                matches!(info.status, AgentStatus::Starting | AgentStatus::Working)
            });
            if should_idle {
                runtime.update_agent(&process, |info| info.status = AgentStatus::Idle);
            }
        }
    });
}

fn detect_status(text: &str) -> Option<AgentStatus> {
    let lower = text.to_lowercase();
    let blocked = [
        "allow command?",
        "do you want to proceed",
        "press enter to confirm",
        "enter to submit answer",
        "[y/n]",
        "action required",
    ];
    if blocked.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Blocked);
    }
    let working = [
        "esc to interrupt",
        "working (",
        "thinking (",
        "running tool",
    ];
    if working.iter().any(|pattern| lower.contains(pattern)) {
        return Some(AgentStatus::Working);
    }
    let last = text.lines().next_back().unwrap_or_default().trim_end();
    if last.ends_with('❯') || last.ends_with('›') || last.ends_with('$') || last.ends_with('#')
    {
        return Some(AgentStatus::Idle);
    }
    None
}

fn resolve_command(request: &CreateAgentRequest) -> Result<(String, Vec<String>), RuntimeError> {
    match request.kind.as_str() {
        "codex" => Ok(("codex".to_string(), request.args.clone())),
        "claude" => Ok(("claude".to_string(), request.args.clone())),
        "command" => {
            if let Some((command, args)) = request.args.split_first() {
                Ok((command.clone(), args.to_vec()))
            } else {
                Ok((
                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
                    Vec::new(),
                ))
            }
        }
        other => Err(RuntimeError::InvalidRequest(format!(
            "unsupported agent kind {other}"
        ))),
    }
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

    #[test]
    fn status_detector_prefers_blockers() {
        assert_eq!(
            detect_status("Working (esc to interrupt)\nAllow command?"),
            Some(AgentStatus::Blocked)
        );
    }

    #[test]
    fn output_buffer_is_bounded() {
        let mut output = OutputBuffer::new();
        output.push(&vec![b'x'; OUTPUT_LIMIT_BYTES + 128]);
        assert!(output.text.len() <= OUTPUT_LIMIT_BYTES);
        assert!(output.raw.len() <= OUTPUT_LIMIT_BYTES);
        assert!(output.truncated);
    }

    #[test]
    fn tracks_bracketed_paste_across_output_chunks() {
        let mut output = OutputBuffer::new();
        output.push(b"before\x1b[?20");
        assert!(!output.bracketed_paste);
        output.push(b"04hafter");
        assert!(output.bracketed_paste);
        output.push(b"\x1b[?2004l");
        assert!(!output.bracketed_paste);
    }

    #[test]
    fn prompt_text_uses_bracketed_paste_when_enabled() {
        assert_eq!(encode_prompt_text("hello", false), b"hello");
        assert_eq!(
            encode_prompt_text("hello", true),
            b"\x1b[200~hello\x1b[201~"
        );
    }

    #[cfg(unix)]
    #[test]
    fn creates_prompts_and_reads_a_real_pty() {
        let runtime = AgentRuntime::new(
            "test-workspace",
            "test-server",
            "http://127.0.0.1:8790",
            None,
            env!("CARGO_MANIFEST_DIR"),
        )
        .expect("runtime should initialize");
        runtime
            .create(
                "test-agent".to_string(),
                CreateAgentRequest {
                    server_id: None,
                    kind: "command".to_string(),
                    name: "test shell".to_string(),
                    cwd: ".".to_string(),
                    args: vec!["/bin/sh".to_string()],
                    cols: 80,
                    rows: 24,
                },
            )
            .expect("shell should start");
        let submitted_at = Instant::now();
        runtime
            .prompt(
                "test-agent",
                "printf 'TREER_E2E:%s:%s\\n' \"$TREER_WORKSPACE_ID\" \"$TREER_AGENT_SERVER_URL\"",
            )
            .expect("prompt should be written");

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let output = runtime
                .read("test-agent", None)
                .expect("output should be readable");
            if output
                .text
                .contains("TREER_E2E:test-workspace:http://127.0.0.1:8790")
            {
                assert!(submitted_at.elapsed() >= PROMPT_SUBMIT_DELAY);
                break;
            }
            assert!(Instant::now() < deadline, "agent output: {}", output.text);
            std::thread::sleep(Duration::from_millis(25));
        }

        runtime.stop("test-agent").expect("shell should stop");
    }
}
