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
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
    #[cfg(windows)]
    job: Option<treer_platform::ProcessTreeGuard>,
    readers: Vec<tokio::task::AbortHandle>,
    diagnostics: Arc<std::sync::Mutex<Value>>,
    stderr_tail: Arc<std::sync::Mutex<VecDeque<u8>>>,
}
impl ProviderChild {
    fn kill_group(&mut self) {
        #[cfg(windows)] { self.job.take(); }
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            // This is the group created for this child, never the launcher's
            // inherited process group. Deliberately detached children are not
            // an OS containment boundary and remain outside this experiment.
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
    pub async fn kill(&mut self) -> std::io::Result<()> {
        let observed = match self.child.try_wait() {
            Ok(Some(status)) => {
                #[cfg(unix)] let signal = { use std::os::unix::process::ExitStatusExt; status.signal() };
                #[cfg(windows)] let signal: Option<i32> = None;
                json!({"state":"exited","code":status.code(),"signal":signal})
            },
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
type Pending = Arc<std::sync::Mutex<HashMap<u64, Reply>>>;
// Outer actor deadlines and shutdown can drop a request future. Cleanup must
// not depend on polling that future again, or a late reply can retain state.
struct PendingRequest { id: u64, pending: Pending, selections: Selections, claude: Option<Arc<std::sync::Mutex<crate::claude::Adapter>>> }
#[derive(Clone, Copy)]
enum ResponsePolicy { Required, Discovery, Observation, Settings }
impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
        self.selections.lock().unwrap().remove(&self.id);
        if let Some(adapter)=&self.claude {adapter.lock().unwrap().forget_control(self.id);}
    }
}
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
    pending: Pending,
    next: Arc<AtomicU64>,
    reviewed_provider: Arc<AtomicBool>,
    diagnostics: Arc<std::sync::Mutex<Value>>,
    selections: Selections,
    claude: Option<Arc<std::sync::Mutex<crate::claude::Adapter>>>,
}

impl Rpc {
    pub fn spawn(
        command: &str,
        args: &[String],
        cwd: &std::path::Path,
        capture_stderr: bool,
    ) -> Result<(Self, ProviderChild, mpsc::Receiver<Value>)> {
        Self::spawn_backend(command,args,cwd,capture_stderr,None)
    }
    pub fn spawn_claude(command:&str,args:&[String],cwd:&std::path::Path,capture_stderr:bool,session:String)->Result<(Self,ProviderChild,mpsc::Receiver<Value>)>{
        Self::spawn_backend(command,args,cwd,capture_stderr,Some(crate::claude::Adapter::new(session)?))
    }
    fn spawn_backend(command:&str,args:&[String],cwd:&std::path::Path,capture_stderr:bool,adapter:Option<crate::claude::Adapter>)->Result<(Self,ProviderChild,mpsc::Receiver<Value>)>{
        let mut command_builder = Command::new(command);
        command_builder.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)] command_builder.process_group(0);
        let mut child = command_builder.spawn()?;
        #[cfg(windows)]
        let job = match treer_platform::ProcessTreeGuard::for_process(child.id().ok_or_else(|| anyhow!("provider_process_missing"))?) {
            Ok(Some(job)) => Some(job),
            _ => { let _ = child.start_kill(); bail!("provider_job_assignment_failed"); }
        };
        let rpc = Self {
            input: Arc::new(Mutex::new(child.stdin.take().unwrap())),
            pending: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
            reviewed_provider: Arc::new(AtomicBool::new(false)),
            diagnostics: Arc::new(std::sync::Mutex::new(json!({"transport":"open","beforeCleanup":null,"stderrCapture":if capture_stderr {"pending"} else {"disabled"}}))),
            selections: Arc::default(),
            claude:adapter.map(|a|Arc::new(std::sync::Mutex::new(a))),
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
            let mut wire_sequence = 0u64;
            let pending = reader_rpc.pending.clone();
            let selections = reader_rpc.selections.clone();
            let (mut codex_lines,mut claude_lines)=if reader_rpc.claude.is_some(){(None,Some(BufReader::new(stdout)))}else{(Some(Reader::new(stdout,reader_rpc.selections.clone()).with_response_filter(move |id| {
                // Admission and selection capture are atomic with Drop's
                // pending removal. Never re-read a cancelled selection later.
                let replies = pending.lock().unwrap();
                replies.contains_key(&id).then(||selections.lock().unwrap().get(&id).cloned().unwrap_or_default())
            })),None)};
            let reason = loop {
                let next=if let Some(lines)=&mut claude_lines{crate::claude::line(lines).await.map_err(|_|"claude_invalid_frame")}else{codex_lines.as_mut().unwrap().next().await};
                let line = match next {
                    Ok(Some(message)) => message,
                    Err(reason) => {
                        reader_rpc.diagnostics.lock().unwrap()["frameFailure"] = codex_lines.as_ref().map(|lines|lines.diagnostic()).unwrap_or(json!({"category":"claude_frame_refused"}));
                        break reason;
                    },
                    Ok(None) => break "stdout_eof",
                };
                let messages=if let Some(adapter)=&reader_rpc.claude{
                    match adapter.lock().unwrap().consume(line){Ok(messages)=>messages,Err(_)=>break "claude_protocol_refused"}
                }else{vec![line]};
                for mut message in messages {
                // One reader owns response/event ordering for this provider
                // incarnation. Never trust a producer-supplied watermark.
                wire_sequence = wire_sequence.checked_add(1).expect("provider receive sequence overflow");
                if message.get("method").is_some() {
                    message["treerWireSequence"] = json!(wire_sequence);
                } else if let Some(result) = message.get_mut("result").and_then(Value::as_object_mut) {
                    result.insert("treerWireSequence".into(),json!(wire_sequence));
                }
                let history=&message["result"]["treerHistory"];
                if history.is_object() {
                    reader_rpc.diagnostics.lock().unwrap()["historyScan"]=json!({"frameBytes":history["frameBytes"],"turnCount":history["total"],"nodeCount":history["nodeCount"],"elapsedMs":history["elapsedMs"],"staleInProgressCount":history["staleInProgressCount"],"threadStatus":history["threadStatus"],"historyAuthority":history["historyAuthority"]});
                }
                if message.get("method").is_some() {
                    // Runtime pumps this bounded queue while awaiting its RPCs.
                    // Full means lossless backpressure, never transport failure.
                    if events.send(message).await.is_err() {
                        return;
                    }
                } else if let Some(id) = message["id"].as_u64() {
                    if let Some(reply) = reader_rpc.pending.lock().unwrap().remove(&id) {
                        let result = if message.get("error").is_some() {
                            Err(anyhow!("Codex RPC refused"))
                        } else {
                            Ok(message["result"].take())
                        };
                        let _ = reply.send(result);
                    }
                }
                }
            };
            reader_rpc.fail(reason);
            reader_rpc.pending.lock().unwrap().clear();
            let _ = events.send(json!({"method":"spike/provider-exit","params":{"reason":reason}})).await;
        });
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32));
        let diagnostics = rpc.diagnostics.clone();
        Ok((rpc, ProviderChild { child, #[cfg(unix)] group, #[cfg(windows)] job, diagnostics, stderr_tail, readers: vec![stderr_reader.abort_handle(), stdout_reader.abort_handle()] }, receiver))
    }

    pub fn diagnostics(&self) -> Value { self.diagnostics.lock().unwrap().clone() }
    pub fn fail(&self, reason: &'static str) {
        let mut diagnostic = self.diagnostics.lock().unwrap();
        if diagnostic["transport"] == "open" { diagnostic["transport"] = json!(reason); }
    }
    pub fn failed(&self) -> bool { self.diagnostics.lock().unwrap()["transport"] != "open" }
    pub fn claude_idle_for_disposition(&self) -> bool {
        !self.failed() && self.claude.as_ref().is_some_and(|adapter|adapter.lock().unwrap().idle_for_disposition())
    }
    pub fn stderr_saved(&self, saved: bool) {
        self.diagnostics.lock().unwrap()["stderrCapture"] = json!(if saved { "saved" } else { "failed" });
    }

    pub async fn write(&self, message: Value) -> Result<()> {
        self.write_bounded(message,128*1024).await
    }
    async fn write_claude_input(&self, message: Value) -> Result<()> {
        let images=message["message"]["content"].as_array().is_some_and(|items|items.iter().any(|i|i["type"]=="image"));
        // 24MiB decoded attachment quota expands to 32MiB base64. This
        // allowance applies only to adapter-validated Claude user input.
        self.write_bounded(message,if images {32*1024*1024+128*1024} else {128*1024}).await
    }
    async fn write_bounded(&self, message: Value, limit: usize) -> Result<()> {
        if self.claude.is_some()&&message["method"]=="initialized"{return Ok(());}
        let mut bytes = serde_json::to_vec(&message)?;
        if bytes.len() > limit {
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
        self.request_history(method,params,selection.map(|range|crate::provider_stream::Selection{range:Some(range),..Default::default()}),ResponsePolicy::Required).await
    }
    pub async fn request_target(&self, method: &str, params: Value, target: Option<String>) -> Result<Value> {
        self.request_history(method,params,target.map(|target|crate::provider_stream::Selection{target:Some(target),..Default::default()}),ResponsePolicy::Required).await
    }
    pub async fn request_observation(&self, params: Value, target: Option<String>) -> Result<Value> {
        // Only the periodic read-only observer may time out without poisoning
        // a healthy shared transport. Startup/reconnect/authority RPCs remain
        // required; all writes still use the same fatal five-second deadline.
        self.request_history("thread/read",params,target.map(|target|crate::provider_stream::Selection{target:Some(target),..Default::default()}),ResponsePolicy::Observation).await
    }
    pub async fn request_optional(&self, method: &str, params: Value) -> Result<Value> {
        if method != "model/list" { bail!("unsupported_optional_rpc"); }
        self.request_history(method,params,None,ResponsePolicy::Discovery).await
    }
    pub async fn request_claude_model(&self, params: Value) -> Result<Value> {
        let adapter=self.claude.as_ref().ok_or_else(||anyhow!("claude_model_control_unavailable"))?;
        let result=match self.request_history("model/set",params,None,ResponsePolicy::Settings).await {
            Ok(result)=>result,
            Err(error)=>{if error.to_string()=="claude_model_response_timeout" {adapter.lock().unwrap().unknown_effort();}return Err(error);}
        };
        // No await between successful response/deadline arbitration and commit.
        // A parsed but expired ACK may produce a candidate, never model state.
        adapter.lock().unwrap().commit_model(&result)
    }
    pub async fn request_claude_effort(&self, method: &str, params: Value) -> Result<Value> {
        if !matches!(method,"effort/set"|"effort/probe") {bail!("unsupported_claude_effort_rpc");}
        let adapter=self.claude.as_ref().ok_or_else(||anyhow!("claude_effort_control_unavailable"))?;
        let policy=if method=="effort/probe" {ResponsePolicy::Discovery}else{ResponsePolicy::Settings};
        match self.request_history(method,params,None,policy).await {
            Ok(result)=>adapter.lock().unwrap().commit_effort(&result),
            Err(error)=>{
                if method=="effort/set" && error.to_string()=="claude_model_response_timeout" {adapter.lock().unwrap().unknown_effort();}
                Err(error)
            }
        }
    }
    pub fn claude_model_revision(&self) -> u64 {
        self.claude.as_ref().map(|a|a.lock().unwrap().model_revision()).unwrap_or(0)
    }
    pub fn claude_effort_revision(&self) -> u64 {
        self.claude.as_ref().map(|a|a.lock().unwrap().effort_revision()).unwrap_or(0)
    }
    async fn request_history(&self, method: &str, params: Value, selection: Option<crate::provider_stream::Selection>, policy: ResponsePolicy) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let _pending = PendingRequest { id, pending: self.pending.clone(), selections: self.selections.clone(), claude:self.claude.clone() };
        if let Some(adapter)=&self.claude{
            if selection.is_some(){bail!("claude_older_history_unavailable");}
            let request=adapter.lock().unwrap().request(id,method,&params)?;
            return match request {
                crate::claude::Request::Local(result)=>Ok(result),
                crate::claude::Request::Input {wire,dispatched}=>{
                    self.write_claude_input(wire).await?;
                    Ok(dispatched)
                },
                crate::claude::Request::Steer {wire,turn,item,after}=>{
                    self.write_claude_input(wire).await?;
                    adapter.lock().unwrap().steered(&turn,item,after.as_deref())
                },
                crate::claude::Request::Wire(wire)=>{
                        let(tx,rx)=oneshot::channel();self.pending.lock().unwrap().insert(id,tx);
                        if matches!(policy,ResponsePolicy::Settings|ResponsePolicy::Discovery) {
                            // Match Node control's response-only deadline. Never
                            // cancel a partial write or poison prompt transport
                            // just because set_model acknowledgement is absent.
                            self.write(wire).await?;
                            let seconds=if matches!(policy,ResponsePolicy::Discovery) {5}else{15};
                            return timeout(Duration::from_secs(seconds),rx).await
                                .map_err(|_|anyhow!("claude_model_response_timeout"))?
                                .map_err(|_|anyhow!("Claude disconnected"))?;
                        }
                        let result=async{self.write(wire).await?;rx.await.map_err(|_|anyhow!("Claude disconnected"))?};
                        let result=match timeout(Duration::from_secs(60),result).await{Ok(r)=>r,Err(_)=>{self.fail("claude_control_timeout");Err(anyhow!("claude_control_timeout"))}};
                        result
                }
            };
        }
        let full_history = full_history_request(method, &params);
        if full_history || selection.is_some() {
            let mut selection = selection.unwrap_or_default();
            selection.full_history = full_history;
            selection.resume_history = full_history && method == "thread/resume";
            selection.reviewed_provider = self.reviewed_provider.load(Ordering::Relaxed);
            self.selections.lock().unwrap().insert(id,selection);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Optional discovery tolerates only a missing response AFTER a full,
        // successful write. Never cancel a partial write at the optional timer.
        if !matches!(policy, ResponsePolicy::Required) {
            self.write(json!({"id":id,"method":method,"params":params})).await?;
            let (seconds, reason) = match policy {
                ResponsePolicy::Discovery => (5, "provider_optional_rpc_timeout"),
                ResponsePolicy::Observation => (60, "provider_observation_timeout"),
                ResponsePolicy::Required => unreachable!(),
                ResponsePolicy::Settings => unreachable!(),
            };
            return timeout(Duration::from_secs(seconds), rx).await
                .map_err(|_| {
                    if matches!(policy, ResponsePolicy::Observation) {
                        let mut diagnostic = self.diagnostics.lock().unwrap();
                        diagnostic["observationTimeouts"] = json!(diagnostic["observationTimeouts"].as_u64().unwrap_or(0).saturating_add(1));
                    }
                    anyhow!(reason)
                })?
                .map_err(|_| anyhow!("Codex disconnected"))?;
        }
        let result = async {
            self.write(json!({"id":id,"method":method,"params":params})).await?;
            rx.await.map_err(|_| anyhow!("Codex disconnected"))?
        };
        // The whole request includes mutex acquisition and write/flush.
        let result = match timeout(Duration::from_secs(60), result).await {
            Ok(result) => result,
            Err(_) => { self.fail("rpc_timeout"); Err(anyhow!("provider_rpc_timeout")) },
        };
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
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn required_response_deadline_remains_fatal_and_cleans_exact_target() {
        let node = std::env::var("TREER_TEST_NODE").expect("fixture requires explicit native Node path");
        let (rpc, mut child, _) = Rpc::spawn(&node, &["-e".into(),"process.stdin.resume();setInterval(()=>{},1000)".into()], &std::env::temp_dir(), false).unwrap();
        let result = rpc.request_target("thread/read", json!({"includeTurns":true}), Some("required".into())).await;
        assert_eq!(result.unwrap_err().to_string(), "provider_rpc_timeout");
        assert_eq!(rpc.diagnostics()["transport"], "rpc_timeout");
        assert!(rpc.pending.lock().unwrap().is_empty());
        assert!(rpc.selections.lock().unwrap().is_empty());
        child.kill().await.unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn cancelled_observation_late_full_history_cannot_poison_new_request() {
        let node = std::env::var("TREER_TEST_NODE").expect("fixture requires explicit native Node path");
        let script = "require('readline').createInterface({input:process.stdin}).on('line',l=>{const q=JSON.parse(l);if(q.method==='thread/read')setTimeout(()=>console.log(JSON.stringify({id:q.id,result:{thread:{id:'thread',status:{type:'idle'},turns:[{id:'old',status:'inProgress',items:[]},{id:'recent',status:'inProgress',items:[]}]}}})),150);else console.log(JSON.stringify({id:q.id,result:{token:q.params.token}}))});";
        let (rpc, mut child, _) = Rpc::spawn(&node, &["-e".into(),script.into()], &std::env::temp_dir(), false).unwrap();
        let clone=rpc.clone();
        let task=tokio::spawn(async move {clone.request_observation(json!({"threadId":"thread","includeTurns":true}),Some("old".into())).await});
        for _ in 0..100 {
            if !rpc.pending.lock().unwrap().is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.abort();let _=task.await;
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(!rpc.failed(), "{}",rpc.diagnostics());
        let next=rpc.request_selected("synthetic/read",json!({"token":"fresh"}),None).await.unwrap();
        assert_eq!(next["token"],"fresh");
        assert!(rpc.pending.lock().unwrap().is_empty());assert!(rpc.selections.lock().unwrap().is_empty());
        child.kill().await.unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn cancelled_request_cleans_selection_and_ignores_late_reply() {
        let node = std::env::var("TREER_TEST_NODE").expect("fixture requires explicit native Node path");
        let script = "const r=require('readline').createInterface({input:process.stdin});r.on('line',l=>{const q=JSON.parse(l);setTimeout(()=>console.log(JSON.stringify({id:q.id,result:{token:q.params.token}})),150)});";
        let (rpc, mut child, _) = Rpc::spawn(&node, &["-e".into(),script.into()], &std::env::temp_dir(), false).unwrap();
        let clone=rpc.clone();
        let task=tokio::spawn(async move { clone.request_observation(json!({"token":"cancelled","includeTurns":true}),Some("exact".into())).await });
        for _ in 0..100 {
            if !rpc.pending.lock().unwrap().is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(rpc.pending.lock().unwrap().len(),1);
        assert_eq!(rpc.selections.lock().unwrap().len(),1);
        task.abort();let _=task.await;
        assert!(rpc.pending.lock().unwrap().is_empty());
        assert!(rpc.selections.lock().unwrap().is_empty());
        assert_eq!(rpc.request_optional("thread/resume",json!({})).await.unwrap_err().to_string(),"unsupported_optional_rpc");
        let reply=rpc.request_selected("synthetic/read",json!({"token":"next"}),None).await.unwrap();
        assert_eq!(reply["token"],"next");
        assert!(!rpc.failed());
        assert!(rpc.pending.lock().unwrap().is_empty());
        child.kill().await.unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn claude_result_while_steer_write_waits_cannot_acknowledge_old_turn() {
        let node=std::env::var("TREER_TEST_NODE").expect("explicit native Node fixture path");
        let root=std::env::temp_dir().join(format!("steer-race-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&root).unwrap();
        let gate=root.join("finish");
        let script="const fs=require('fs'),r=require('readline').createInterface({input:process.stdin});let count=0;r.on('line',l=>{const q=JSON.parse(l);if(q.type==='user'&&++count===1){const timer=setInterval(()=>{if(fs.existsSync(process.argv[1])){clearInterval(timer);console.log(JSON.stringify({type:'result',is_error:false,result:'A final'}));}},5);}});";
        let (rpc,mut child,mut events)=Rpc::spawn_claude(&node,&["-e".into(),script.into(),gate.to_string_lossy().into()],&root,false,"session".into()).unwrap();
        let initial=rpc.request_selected("turn/start",json!({"threadId":"session","input":[{"type":"text","text":"A"}]}),None).await.unwrap();
        let turn=initial["turn"]["id"].clone();
        let writer=rpc.input.lock().await;
        let clone=rpc.clone();
        let steering=async move {clone.request_selected("turn/steer",json!({"threadId":"session","expectedTurnId":turn,"input":[{"type":"text","text":"B steer"}]}),None).await};
        tokio::pin!(steering);
        // Poll through request admission into the held writer before allowing
        // the real producer's terminal event to run; no scheduler guess.
        tokio::select! {biased; result=&mut steering=>panic!("write was not held: {result:?}"), _=tokio::task::yield_now()=>{}}
        std::fs::write(&gate,b"finish owned turn").unwrap();
        let completed=loop {
            let event=timeout(Duration::from_secs(3),events.recv()).await.unwrap().unwrap();
            if event["method"]=="turn/completed" {break event;}
        };
        assert!(!completed["params"]["turn"]["items"].as_array().unwrap().iter().any(|i|i["text"]=="B steer"));
        drop(writer);
        assert_eq!(steering.await.unwrap_err().to_string(),"claude_steer_outcome_unknown");
        assert!(!rpc.failed(),"uncertain input is not a forged transport failure");
        assert!(rpc.pending.lock().unwrap().is_empty());
        child.kill().await.unwrap();
        std::fs::remove_file(gate).unwrap();std::fs::remove_dir(root).unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn claude_model_stalled_write_is_fatal_and_cleans_control() {
        let node=std::env::var("TREER_TEST_NODE").expect("explicit native Node fixture path");
        let (rpc,mut child,_)=Rpc::spawn_claude(&node,&["-e".into(),"process.stdin.pause();setInterval(()=>{},1000)".into()],&std::env::temp_dir(),false,"session".into()).unwrap();
        let writer=rpc.input.lock().await;
        let error=rpc.request_claude_model(json!({"model":"native-model"})).await.unwrap_err();
        assert_eq!(error.to_string(),"provider_write_timeout");assert_eq!(rpc.diagnostics()["transport"],"stdin_write_timeout");
        assert!(rpc.pending.lock().unwrap().is_empty());
        // Exact expired control cannot commit even if its reply later parses.
        let adapter=rpc.claude.as_ref().unwrap();
        let late=adapter.lock().unwrap().consume(json!({"type":"control_response","response":{"subtype":"success","request_id":"1","response":{}}})).unwrap();
        assert_eq!(late.last().unwrap()["result"],json!({}));assert_eq!(rpc.claude_model_revision(),0);
        drop(writer);child.kill().await.unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn claude_effort_probe_stalled_write_is_fatal_and_cleans_control() {
        let node=std::env::var("TREER_TEST_NODE").expect("explicit native Node fixture path");
        let (rpc,mut child,_)=Rpc::spawn_claude(&node,&["-e".into(),"process.stdin.pause();setInterval(()=>{},1000)".into()],&std::env::temp_dir(),false,"session".into()).unwrap();
        {
            let mut adapter=rpc.claude.as_ref().unwrap().lock().unwrap();
            adapter.request(900,"initialize",&json!({})).unwrap();
            adapter.consume(json!({"type":"control_response","response":{"subtype":"success","request_id":"900","response":{"models":[{"value":"default","supportsEffort":true,"supportedEffortLevels":["high"]}]}}})).unwrap();
        }
        let writer=rpc.input.lock().await;
        assert_eq!(rpc.request_claude_effort("effort/probe",json!({})).await.unwrap_err().to_string(),"provider_write_timeout");
        assert_eq!(rpc.diagnostics()["transport"],"stdin_write_timeout");assert!(rpc.pending.lock().unwrap().is_empty());
        let late=rpc.claude.as_ref().unwrap().lock().unwrap().consume(json!({"type":"control_response","response":{"subtype":"success","request_id":"1","response":{}}})).unwrap();
        assert_eq!(late[0]["result"],json!({}));
        drop(writer);child.kill().await.unwrap();
    }
    #[cfg(all(unix, feature="fixture-harness"))]
    #[tokio::test]
    async fn optional_discovery_stalled_write_still_poisoned_and_cleaned() {
        let node = std::env::var("TREER_TEST_NODE").expect("fixture requires explicit native Node path");
        let (rpc, mut child, _) = Rpc::spawn(&node, &["-e".into(),"process.stdin.pause();setInterval(()=>{},1000)".into()], &std::env::temp_dir(), false).unwrap();
        // Hold the writer to model backpressure, without relying on pipe size.
        let writer=rpc.input.lock().await;
        let result=rpc.request_optional("model/list",json!({})).await;
        assert_eq!(result.unwrap_err().to_string(),"provider_write_timeout");
        assert_eq!(rpc.diagnostics()["transport"],"stdin_write_timeout");
        assert!(rpc.pending.lock().unwrap().is_empty());
        assert!(rpc.selections.lock().unwrap().is_empty());
        drop(writer);
        assert!(rpc.request_selected("thread/resume",json!({}),None).await.is_err());
        child.kill().await.unwrap();
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_provider_job_terminates_owned_descendant() {
        let node = std::env::var("TREER_WINDOWS_TEST_NODE").expect("native Windows test requires pinned Node executable");
        let root = std::env::temp_dir().join(format!("treer-rust-job-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        let script = "const fs=require('fs'),cp=require('child_process');setTimeout(()=>{const child=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});fs.writeFileSync('descendant.pid',String(child.pid));},100);setInterval(()=>{},1000)";
        let (_, mut child, _) = Rpc::spawn(&node, &["-e".into(),script.into()], &root, false).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Ok(value) = std::fs::read_to_string(root.join("descendant.pid")) { break value.parse::<u32>().unwrap(); }
            assert!(tokio::time::Instant::now() < deadline, "owned descendant startup deadline");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let birth = treer_platform::process_created_at(pid).unwrap().expect("descendant exists");
        let _ = child.kill().await;
        loop {
            if treer_platform::process_created_at(pid).unwrap() != Some(birth) { break; }
            assert!(tokio::time::Instant::now() < deadline, "owned descendant cleanup deadline");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(child);
        std::fs::remove_dir_all(root).unwrap();
    }
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
