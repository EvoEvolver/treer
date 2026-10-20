use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
    time::{timeout, Duration},
};
use crate::provider_stream::{Reader, Selections};

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
fn reviewed_provider_version(result: &Value) -> bool {
    // Only a fixed allowlist result is retained. Never persist userAgent:
    // its platform, originator and suffix are unrelated private metadata.
    result["userAgent"].as_str().and_then(|s| s.split_ascii_whitespace().next())
        .and_then(|s| s.split_once('/'))
        .is_some_and(|(originator,version)| !originator.is_empty() && originator.len()<=128
            && originator.bytes().all(|b| b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'|b'.'))
            && version=="0.162.1")
}
fn full_history_request(method: &str, params: &Value) -> bool {
    (method == "thread/resume" || (method == "thread/read" && params["includeTurns"] == true))
        && !params.get("excludeTurns").is_some_and(|value| value != false)
        && ["cursor", "limit", "initialTurnsPage", "turnsLimit", "turnsCursor"].iter().all(|key| params.get(key).is_none())
}
#[derive(Clone)]
pub struct Rpc {
    input: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
    next: Arc<AtomicU64>,
    reviewed_provider: Arc<AtomicBool>,
    diagnostics: Arc<std::sync::Mutex<Value>>,
    selections: Selections,
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
            reviewed_provider: Arc::new(AtomicBool::new(false)),
            diagnostics: Arc::new(std::sync::Mutex::new(json!({"transport":"open","beforeCleanup":null,"stderrCapture":if capture_stderr {"pending"} else {"disabled"}}))),
            selections: Arc::default(),
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
            let mut lines = Reader::new(stdout, reader_rpc.selections.clone());
            let reason = loop {
                let line = match lines.next().await {
                    Ok(Some(message)) => message,
                    Err(reason) => {
                        reader_rpc.diagnostics.lock().unwrap()["frameFailure"] = lines.diagnostic();
                        break reason;
                    },
                    Ok(None) => break "stdout_eof",
                };
                let mut message = line;
                let history=&message["result"]["treerHistory"];
                if history.is_object() {
                    reader_rpc.diagnostics.lock().unwrap()["historyScan"]=json!({"frameBytes":history["frameBytes"],"turnCount":history["total"],"nodeCount":history["nodeCount"],"elapsedMs":history["elapsedMs"],"staleInProgressCount":history["staleInProgressCount"],"threadStatus":history["threadStatus"],"historyAuthority":history["historyAuthority"]});
                }
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

    pub async fn request_selected(&self, method: &str, params: Value, selection: Option<(usize,usize)>) -> Result<Value> {
        self.request_history(method,params,selection.map(|range|crate::provider_stream::Selection{range:Some(range),..Default::default()})).await
    }
    pub async fn request_target(&self, method: &str, params: Value, target: Option<String>) -> Result<Value> {
        self.request_history(method,params,target.map(|target|crate::provider_stream::Selection{target:Some(target),..Default::default()})).await
    }
    async fn request_history(&self, method: &str, params: Value, selection: Option<crate::provider_stream::Selection>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let full_history = full_history_request(method, &params);
        if full_history || selection.is_some() {
            let mut selection = selection.unwrap_or_default();
            selection.full_history = full_history;
            selection.resume_history = full_history && method == "thread/resume";
            selection.reviewed_provider = self.reviewed_provider.load(Ordering::Relaxed);
            self.selections.lock().unwrap().insert(id,selection);
        }
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
        self.selections.lock().unwrap().remove(&id);
        if method == "initialize" {
            let reviewed = result.as_ref().is_ok_and(reviewed_provider_version);
            self.reviewed_provider.store(reviewed,Ordering::Relaxed);
            self.diagnostics.lock().unwrap()["providerVersion"] = json!(if reviewed {"0.162.1"} else {"unreviewed"});
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_authority_requires_full_request_contract() {
        assert!(full_history_request("thread/resume", &json!({})));
        assert!(full_history_request("thread/read", &json!({"includeTurns":true})));
        for params in [json!({}),json!({"includeTurns":false}),json!({"includeTurns":true,"cursor":"next"}),json!({"includeTurns":true,"limit":50})] {
            assert!(!full_history_request("thread/read", &params));
        }
        assert!(!full_history_request("thread/resume", &json!({"initialTurnsPage":{}})));
        assert!(full_history_request("thread/resume", &json!({"excludeTurns":false})));
        for value in [json!(true),Value::Null,json!("false")] {
            assert!(!full_history_request("thread/resume", &json!({"excludeTurns":value})));
        }
        assert!(!full_history_request("thread/turns/list", &json!({"includeTurns":true})));
    }
    #[test]
    fn only_reviewed_initialize_version_can_allow_resume_anchors() {
        for ua in [json!("treer-rust-spike/0.162.1 (PRIVATE_SUFFIX)"),json!("codex_cli_rs/0.162.1 (synthetic)")] {
            assert!(reviewed_provider_version(&json!({"userAgent":ua})));
        }
        for ua in [Value::Null,json!(123),json!(""),json!("treer/0.162.2"),json!("treer/0.162.10"),json!("treer/0.162.1-dev"),json!("treer/unknown (0.162.1)"),json!("/0.162.1"),json!("treer/0.162.1/private")] {
            assert!(!reviewed_provider_version(&json!({"userAgent":ua})));
        }
        assert!(!reviewed_provider_version(&json!({})));
    }
}
