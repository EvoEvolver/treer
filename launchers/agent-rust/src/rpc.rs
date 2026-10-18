use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
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
use tokio_util::codec::{FramedRead, LinesCodec};

pub struct ProviderChild {
    child: Child,
    group: Option<rustix::process::Pid>,
    readers: Vec<tokio::task::AbortHandle>,
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
        self.kill_group();
        for task in &self.readers { task.abort(); }
        self.child.kill().await
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
}

impl Rpc {
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: &std::path::Path,
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
        };
        let (events, receiver) = mpsc::channel(8);
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // Drain provider stderr without retaining/logging conversation or secrets.
        let stderr_reader = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut BufReader::new(stderr), &mut tokio::io::sink()).await;
        });
        let reader_rpc = rpc.clone();
        let stdout_reader = tokio::spawn(async move {
            let mut lines =
                FramedRead::new(stdout, LinesCodec::new_with_max_length(4 * 1024 * 1024));
            while let Some(Ok(line)) = lines.next().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                if message.get("method").is_some() {
                    // Never block response parsing behind notifications while the
                    // actor awaits an RPC. Overflow closes the provider stream;
                    // it must not silently lose events or grow memory.
                    if events.try_send(message).is_err() {
                        break;
                    }
                } else if let Some(id) = message["id"].as_u64() {
                    if let Some(reply) = reader_rpc.pending.lock().await.remove(&id) {
                        let result = if message.get("error").is_some() {
                            Err(anyhow!("Codex RPC refused"))
                        } else {
                            Ok(message["result"].clone())
                        };
                        let _ = reply.send(result);
                    }
                }
            }
            reader_rpc.pending.lock().await.clear();
            let _ = events.send(json!({"method":"spike/provider-exit"})).await;
        });
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        Ok((rpc, ProviderChild { child, group, readers: vec![stderr_reader.abort_handle(), stdout_reader.abort_handle()] }, receiver))
    }

    pub async fn write(&self, message: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&message)?;
        if bytes.len() > 128 * 1024 {
            bail!("RPC input too large");
        }
        bytes.push(b'\n');
        let mut input = self.input.lock().await;
        input.write_all(&bytes).await?;
        input.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let result = async {
            self.write(json!({"id":id,"method":method,"params":params}))
                .await?;
            timeout(Duration::from_secs(60), rx)
                .await?
                .map_err(|_| anyhow!("Codex disconnected"))?
        }
        .await;
        self.pending.lock().await.remove(&id);
        result
    }
}
