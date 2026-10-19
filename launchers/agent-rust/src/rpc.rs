use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
    time::{timeout, Duration},
};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

// Long-lived Codex threads arrive as one thread/resume JSON-RPC line.
// This is a frame bound, separate from the 16 MiB retained-history budget.
const PROVIDER_LINE_BYTES: usize = 64 * 1024 * 1024;

pub struct ProviderChild {
    child: Child,
    group: Option<rustix::process::Pid>,
    readers: Vec<tokio::task::AbortHandle>,
    diagnostics: Arc<std::sync::Mutex<Value>>,
    stderr_tail: Arc<std::sync::Mutex<VecDeque<u8>>>,
}
impl ProviderChild {
    fn kill_group(&mut self) {
        if let Some(group) = self.group.take() {
            // This is the group created for this child, never the launcher's
            // inherited process group. Deliberately detached children are not
            // an OS containment boundary and remain outside this experiment.
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
    pub async fn kill(&mut self) -> std::io::Result<()> {
        use std::os::unix::process::ExitStatusExt;
        let observed = match self.child.try_wait() {
            Ok(Some(status)) => json!({"state":"exited","code":status.code(),"signal":status.signal()}),
            Ok(None) => json!({"state":"running","code":null,"signal":null}),
            Err(_) => json!({"state":"unverified","code":null,"signal":null}),
        };
        {
            let mut diagnostic = self.diagnostics.lock().unwrap();
            if diagnostic["beforeCleanup"].is_null() { diagnostic["beforeCleanup"] = observed; }
        }
        self.kill_group();
        for task in &self.readers { task.abort(); }
        self.child.kill().await
    }
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_tail.lock().unwrap().iter().copied().collect()
    }
}
impl Drop for ProviderChild {
    fn drop(&mut self) {
        self.kill_group();
        for task in &self.readers { task.abort(); }
    }
}

type Reply = oneshot::Sender<Result<Value>>;
#[derive(Clone)]
pub struct Rpc {
    input: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
    next: Arc<AtomicU64>,
    diagnostics: Arc<std::sync::Mutex<Value>>,
}

impl Rpc {
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: &std::path::Path,
        capture_stderr: bool,
    ) -> Result<(Self, ProviderChild, mpsc::Receiver<Value>)> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let rpc = Self {
            input: Arc::new(Mutex::new(child.stdin.take().unwrap())),
            pending: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
            diagnostics: Arc::new(std::sync::Mutex::new(json!({"transport":"open","beforeCleanup":null,"stderrCapture":if capture_stderr {"pending"} else {"disabled"}}))),
        };
        let (events, receiver) = mpsc::channel(8);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // Private bounded tail only; never echo provider text into AIS or logs.
        let stderr_tail = Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let tail_writer = stderr_tail.clone();
        let stderr_reader = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stderr = BufReader::new(stderr);
            let mut bytes = [0; 4096];
            while let Ok(n) = stderr.read(&mut bytes).await {
                if n == 0 { break; }
                if !capture_stderr { continue; }
                let mut tail = tail_writer.lock().unwrap();
                tail.extend(&bytes[..n]);
                while tail.len() > 8192 { tail.pop_front(); }
            }
        });
        let reader_rpc = rpc.clone();
        let stdout_reader = tokio::spawn(async move {
            let mut lines =
                FramedRead::new(stdout, LinesCodec::new_with_max_length(PROVIDER_LINE_BYTES));
            let reason = loop {
                let line = match lines.next().await {
                    Some(Ok(line)) => line,
                    Some(Err(LinesCodecError::MaxLineLengthExceeded)) => break "line_too_large",
                    Some(Err(_)) => break "line_error",
                    None => break "stdout_eof",
                };
                let Ok(mut message) = serde_json::from_str::<Value>(&line) else {
                    break "invalid_json";
                };
                drop(line);
                if message.get("method").is_some() {
                    // Runtime pumps this bounded queue while awaiting its RPCs.
                    // Full means lossless backpressure, never transport failure.
                    if events.send(message).await.is_err() {
                        break "consumer_closed";
                    }
                } else if let Some(id) = message["id"].as_u64() {
                    if let Some(reply) = reader_rpc.pending.lock().await.remove(&id) {
                        let result = if message.get("error").is_some() {
                            Err(anyhow!("Codex RPC refused"))
                        } else {
                            Ok(message["result"].take())
                        };
                        let _ = reply.send(result);
                    }
                }
            };
            reader_rpc.fail(reason);
            reader_rpc.pending.lock().await.clear();
            let _ = events.send(json!({"method":"spike/provider-exit","params":{"reason":reason}})).await;
        });
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        let diagnostics = rpc.diagnostics.clone();
        Ok((rpc, ProviderChild { child, group, diagnostics, stderr_tail, readers: vec![stderr_reader.abort_handle(), stdout_reader.abort_handle()] }, receiver))
    }

    pub fn diagnostics(&self) -> Value { self.diagnostics.lock().unwrap().clone() }
    pub fn fail(&self, reason: &'static str) {
        let mut diagnostic = self.diagnostics.lock().unwrap();
        if diagnostic["transport"] == "open" { diagnostic["transport"] = json!(reason); }
    }
    pub fn failed(&self) -> bool { self.diagnostics.lock().unwrap()["transport"] != "open" }
    pub fn stderr_saved(&self, saved: bool) {
        self.diagnostics.lock().unwrap()["stderrCapture"] = json!(if saved { "saved" } else { "failed" });
    }

    pub async fn write(&self, message: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&message)?;
        if bytes.len() > 128 * 1024 {
            bail!("RPC input too large");
        }
        bytes.push(b'\n');
        let operation = async {
            let mut input = self.input.lock().await;
            if self.failed() { bail!("provider_transport_closed"); }
            input.write_all(&bytes).await?;
            input.flush().await?;
            Ok::<_, anyhow::Error>(())
        };
        match timeout(Duration::from_secs(5), operation).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => { self.fail("stdin_write_error"); bail!("provider_write_failed"); },
            Err(_) => { self.fail("stdin_write_timeout"); bail!("provider_write_timeout"); },
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let result = async {
            self.write(json!({"id":id,"method":method,"params":params})).await?;
            rx.await.map_err(|_| anyhow!("Codex disconnected"))?
        };
        // The whole request includes mutex acquisition and write/flush.
        let result = match timeout(Duration::from_secs(60), result).await {
            Ok(result) => result,
            Err(_) => { self.fail("rpc_timeout"); Err(anyhow!("provider_rpc_timeout")) },
        };
        self.pending.lock().await.remove(&id);
        result
    }
}
